//! Park a live agent in its pane and relaunch it later.
//!
//! Suspending asks the agent to exit from its own prompt while the pane keeps
//! the native session reference and the agent's name; the pane reports
//! `suspended` until it is activated. Activating rebuilds the resume plan from
//! that reference and launches it in the same pane, reusing the managed launch
//! path that `agent.start` uses. Restarting is a suspend that owes the pane
//! an automatic activation: the scheduler tick relaunches the agent once the
//! exit is observed and the shell prompt is back.

use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{
    agents::{
        available_shell_name, live_runtime_agent_job, runtime_hosts_agent,
        AGENT_START_SETTLE_DELAY, DEFAULT_AGENT_START_TIMEOUT,
    },
    terminal_targets::{TerminalTarget, TerminalTargetError},
    App,
};
use crate::terminal::{SuspendEscalationOutcome, SuspendExitEscalation, SuspendProbe, TerminalId};

/// How long the agent gets to exit on its own after the graceful exit input
/// before Herdr terminates its foreground job.
pub(crate) const SUSPEND_GRACEFUL_EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long a signaled foreground job gets before the next escalation step:
/// `SIGTERM` after the graceful grace, then `SIGKILL` after this much more.
pub(crate) const SUSPEND_SIGNAL_ESCALATION_GRACE: Duration = Duration::from_secs(2);
/// Terminal size used for a relaunch when the pane has no runtime and no
/// computed geometry yet; the next resize corrects it.
const FALLBACK_RELAUNCH_SIZE: (u16, u16) = (24, 80);
/// How long an `agent.restart` waits for the observed exit and an available
/// shell prompt before it gives up and leaves the agent suspended. Covers the
/// whole exit escalation (graceful grace, `SIGTERM`, `SIGKILL`) with room for
/// the shell to settle.
pub(crate) const RESTART_RESUME_TIMEOUT: Duration = Duration::from_secs(30);
/// How often a pending restart probes for the shell prompt once the exit was
/// observed.
pub(crate) const RESTART_RESUME_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Fork (force suspend): how long after a forced suspend's Esc the exit
/// input waits at least, so the agent never reads Esc and the input as one
/// Alt chord.
pub(crate) const FORCE_INTERRUPT_SETTLE: Duration = Duration::from_millis(500);
/// Fork (force suspend): how long a forced suspend waits for the interrupted
/// agent to come back to its prompt; past it the agent is terminated
/// (`SIGTERM`, then `SIGKILL`) without the exit input.
pub(crate) const FORCE_INTERRUPT_WAIT: Duration = Duration::from_secs(5);
/// Fork (restart screen): how long a restart or wake that gave up stays
/// marked on its parked agent (the suspended card says so).
pub(crate) const RESUME_FAILED_MARK_TTL: Duration = Duration::from_secs(5 * 60);

/// Fork (restart screen): a toast for a relaunch that never started (the
/// pane's shell came back with no agent), for the server to send to client
/// shells. Public ids, so a click focuses the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelaunchFailureNotice {
    pub title: String,
    pub body: Option<String>,
    pub workspace_id: String,
    pub tab_id: Option<String>,
    pub pane_id: Option<String>,
}

pub(super) enum AgentSuspendError {
    Target(TerminalTargetError),
    NotRunning(String),
    Blocked(String),
    /// Mid-turn: the exit input would interrupt the agent's work.
    Working(String),
    /// Background subagents are running: the exit would stop them.
    SubagentsRunning {
        target: String,
        count: u32,
    },
    /// Fork (autosuspend): background tasks (shells, monitors, workflows,
    /// session crons) are in flight: the exit would stop them.
    BackgroundTasksRunning {
        target: String,
        count: u32,
    },
    AlreadySuspended(String),
    NotSuspendable {
        target: String,
        reason: String,
        /// Fork (agents v2): the typed reason; the text above is unchanged.
        kind: NotSuspendableReason,
    },
    InputFailed(String),
}

/// Fork (agents v2): why an agent cannot be suspended, typed (the close flow
/// decides on it; no string matching).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotSuspendableReason {
    /// No native session reference is known yet.
    NoSession,
    /// The agent has no native resume plan.
    NoResumePlan,
    /// The agent has no graceful exit command.
    NoGracefulExit,
    /// The agent is the pane's own process (close the tab instead).
    PaneProcess,
}

/// What a suspend needs once every check passed.
struct SuspendReady {
    terminal_id: TerminalId,
    session: crate::agent_resume::PersistedAgentSession,
    exit_input: &'static str,
}

pub(super) enum AgentRestartError {
    Target(TerminalTargetError),
    Blocked(String),
    Suspended(String),
    Suspend(AgentSuspendError),
}

pub(super) enum AgentActivateError {
    Target(TerminalTargetError),
    NotSuspended(String),
    PaneNotAvailable(String),
    /// Detection has not yet reported the parked process gone; relaunching
    /// now would race the late exit report that wipes the live session.
    ExitPending(String),
    InvalidArgument,
    InputFailed(String),
}

/// The parked agent's pids in the pane's foreground job, as the escalation
/// tick sees them: a missing job is a failed probe; a job that is not the
/// expected agent holds no agent pids. The pane's own child process is never
/// signalled.
pub(super) fn suspend_probe_from_job(
    child_pid: Option<u32>,
    live_job: Option<(crate::platform::ForegroundJob, Option<crate::detect::Agent>)>,
    expected: Option<crate::detect::Agent>,
) -> SuspendProbe {
    let Some((job, agent)) = live_job else {
        return SuspendProbe::Failed;
    };
    let agent_pids = if expected.is_some() && agent == expected {
        job.processes
            .iter()
            .map(|process| process.pid)
            .filter(|pid| Some(*pid) != child_pid)
            .collect()
    } else {
        Vec::new()
    };
    SuspendProbe::Job { agent_pids }
}

/// The identified agent is the pane's direct child (an imported
/// `launch_argv` pane), so there is no shell to return to and the exit
/// escalation could never signal it.
pub(super) fn agent_is_pane_process(
    child_pid: Option<u32>,
    live_job: Option<&(crate::platform::ForegroundJob, Option<crate::detect::Agent>)>,
    expected: crate::detect::Agent,
) -> bool {
    let Some(child_pid) = child_pid else {
        return false;
    };
    live_job
        .is_some_and(|(job, agent)| *agent == Some(expected) && job.process_group_id == child_pid)
}

impl App {
    /// Fork: store the session a live Claude Code or Codex agent was
    /// launched into, read from its process argv
    /// ([`crate::agent_resume::persisted_session_from_process_argv`]), when
    /// neither a hook nor a restore gave the pane one (a lost SessionStart
    /// report). It is the readers' last fallback, so a later hook report
    /// wins. `rederive` (a newly detected agent) drops the previous value and
    /// reads again; otherwise a stored value is kept and a foreground group
    /// whose argv named nothing is not read again. One process read, only
    /// on detection and lifecycle/API requests, never while rendering.
    pub(crate) fn fill_process_agent_session(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        rederive: bool,
    ) {
        let Some(terminal) = self.state.terminals.get_mut(terminal_id) else {
            self.agents_model.process_session_misses.remove(terminal_id);
            return;
        };
        if rederive {
            terminal.set_process_agent_session(None);
            self.agents_model.process_session_misses.remove(terminal_id);
        } else if terminal.has_process_agent_session() {
            return;
        }
        let Some(agent) = terminal.effective_known_agent().filter(|agent| {
            matches!(
                agent,
                crate::detect::Agent::Claude | crate::detect::Agent::Codex
            )
        }) else {
            return;
        };
        if terminal.suspended_agent.is_some() || terminal.suspendable_agent_session().is_some() {
            return;
        }
        let (argvs, group) = self.agent_process_argvs(terminal_id, rederive);
        let Some(argvs) = argvs else {
            return;
        };
        let session = argvs
            .iter()
            .find_map(|argv| crate::agent_resume::persisted_session_from_process_argv(agent, argv));
        match session {
            Some(session) => {
                tracing::info!(
                    terminal = %terminal_id,
                    session = %session.session_ref.value,
                    "agent session read from its command line"
                );
                if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
                    terminal.set_process_agent_session(Some(session));
                }
                self.state.mark_session_dirty();
            }
            None => {
                if let (false, Some(group)) = (rederive, group) {
                    let terminals = &self.state.terminals;
                    self.agents_model
                        .process_session_misses
                        .retain(|id, _| terminals.contains_key(id));
                    self.agents_model
                        .process_session_misses
                        .insert(terminal_id.clone(), group);
                }
            }
        }
    }

    /// Fork: [`Self::fill_process_agent_session`] for every live agent.
    pub(crate) fn fill_process_agent_sessions(&mut self) {
        let terminal_ids: Vec<_> = self
            .state
            .terminals
            .values()
            .filter(|terminal| {
                terminal.suspended_agent.is_none()
                    && matches!(
                        terminal.effective_known_agent(),
                        Some(crate::detect::Agent::Claude | crate::detect::Agent::Codex)
                    )
            })
            .map(|terminal| terminal.id.clone())
            .collect();
        for terminal_id in terminal_ids {
            self.fill_process_agent_session(&terminal_id, false);
        }
    }

    /// The argv of every process in the terminal's foreground job, and the
    /// job's process group; `None` when the group already named nothing
    /// (unless `rederive`) or the job cannot be read.
    fn agent_process_argvs(
        &self,
        terminal_id: &crate::terminal::TerminalId,
        rederive: bool,
    ) -> (Option<Vec<Vec<String>>>, Option<u32>) {
        #[cfg(test)]
        if let Some(argvs) = self.agents_model.test_process_argvs.get(terminal_id) {
            return (Some(argvs.clone()), None);
        }
        let Some(pid) = self
            .terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.child_pid())
        else {
            return (None, None);
        };
        let group = crate::detect::foreground_process_group_id(pid);
        if !rederive
            && group.is_some()
            && self.agents_model.process_session_misses.get(terminal_id) == group.as_ref()
        {
            return (None, group);
        }
        let argvs = crate::detect::foreground_job(pid).map(|job| {
            job.processes
                .into_iter()
                .filter_map(|process| process.argv)
                .collect()
        });
        (argvs, group)
    }

    /// Park the live agent hosted by `target`.
    ///
    /// The suspended record is stored before the exit input is sent because
    /// the process-exit path wipes the live session fields it is built from.
    pub(super) fn suspend_agent(&mut self, target: &str) -> Result<String, AgentSuspendError> {
        self.suspend_agent_by(target, None)
    }

    /// Fork (agents v2): [`Self::suspend_agent`], recording who suspended it
    /// (`None` is the user).
    pub(super) fn suspend_agent_by(
        &mut self,
        target: &str,
        by: Option<crate::api::schema::agents_model::AgentsWho>,
    ) -> Result<String, AgentSuspendError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentSuspendError::Target)?;
        self.fill_resolved_process_agent_session(&resolved);
        let suspended = self.suspend_resolved_agent(target, &resolved, false)?;
        if let Some(by) = by {
            if let Some(record) = self
                .state
                .workspaces
                .get(resolved.ws_idx)
                .and_then(|ws| ws.terminal_id(resolved.pane_id))
                .cloned()
                .and_then(|terminal_id| self.state.terminals.get_mut(&terminal_id))
                .and_then(|terminal| terminal.suspended_agent.as_mut())
            {
                record.suspended_by = Some(by);
            }
        }
        Ok(suspended)
    }

    /// Fork (autosuspend): [`Self::suspend_agent`] by herdr itself for an
    /// agent left idle (src/app/agent_autosuspend.rs): every check is the
    /// same, and the record is marked `auto` so a message wakes it.
    pub(super) fn suspend_agent_auto(&mut self, target: &str) -> Result<String, AgentSuspendError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentSuspendError::Target)?;
        self.fill_resolved_process_agent_session(&resolved);
        let suspended = self.suspend_resolved_agent(target, &resolved, false)?;
        if let Some(record) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|ws| ws.terminal_id(resolved.pane_id))
            .cloned()
            .and_then(|terminal_id| self.state.terminals.get_mut(&terminal_id))
            .and_then(|terminal| terminal.suspended_agent.as_mut())
        {
            record.auto = true;
        }
        // Clients show the mark with the agent's state time.
        self.state.agent_times_view_rev = self.state.agent_times_view_rev.wrapping_add(1).max(1);
        Ok(suspended)
    }

    /// Fork (autosuspend): whether the agent parked in `terminal_id` was
    /// suspended by herdr for idling.
    pub(crate) fn auto_suspended(&self, terminal_id: &str) -> bool {
        self.state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.suspended_agent.as_ref())
            .is_some_and(|record| record.auto)
    }

    /// Fork: [`Self::fill_process_agent_session`] for a resolved target.
    fn fill_resolved_process_agent_session(&mut self, resolved: &TerminalTarget) {
        if let Some(terminal_id) = self
            .state
            .terminal_id_for_pane(resolved.ws_idx, resolved.pane_id)
        {
            self.fill_process_agent_session(&terminal_id, false);
        }
    }

    /// Fork (agents v2): whether the agent in a pane could be suspended now,
    /// without writing anything.
    pub(super) fn check_suspend(&self, target: &str) -> Result<(), AgentSuspendError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentSuspendError::Target)?;
        self.suspend_checks(target, &resolved, false).map(|_| ())
    }

    /// Fork (force suspend): [`Self::suspend_agent`] past the refusals a
    /// forced exit overrides (blocked, working, subagents or background
    /// tasks running), the user's own choice from herdr's interface. A
    /// working or blocked Claude Code or Codex gets an Esc first and the exit
    /// input once it is back at its prompt; another agent that is busy is
    /// terminated without it. Subagents and background tasks end with the
    /// agent. Recorded as suspended by the user.
    pub(super) fn force_suspend_agent(
        &mut self,
        target: &str,
    ) -> Result<String, AgentSuspendError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentSuspendError::Target)?;
        self.fill_resolved_process_agent_session(&resolved);
        self.suspend_resolved_agent(target, &resolved, true)
    }

    fn suspend_resolved_agent(
        &mut self,
        target: &str,
        resolved: &TerminalTarget,
        force: bool,
    ) -> Result<String, AgentSuspendError> {
        let SuspendReady {
            terminal_id,
            session,
            exit_input,
        } = self.suspend_checks(target, resolved, force)?;
        let not_found = || {
            AgentSuspendError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            })
        };
        let now = Instant::now();
        // Fork (restart screen): the suspend clears the pane's resume marks.
        let marked = self
            .state
            .terminals
            .get(&terminal_id)
            .is_some_and(crate::terminal::TerminalState::has_resume_marks);
        let busy = force
            .then(|| self.state.terminals.get(&terminal_id))
            .flatten()
            .filter(|terminal| {
                matches!(
                    terminal.state,
                    crate::detect::AgentState::Working | crate::detect::AgentState::Blocked
                )
            })
            .map(|terminal| {
                matches!(
                    terminal.effective_known_agent(),
                    Some(crate::detect::Agent::Claude | crate::detect::Agent::Codex)
                )
            });
        match busy {
            // Fork (force suspend): Esc ends the turn or the dialog of Claude
            // Code and Codex; the exit input follows once the prompt is back.
            Some(true)
                if self.send_interrupt_esc(
                    resolved,
                    crate::agents_model::InputSource::Programmatic(
                        crate::agents_model::Programmatic::Api,
                    ),
                ) =>
            {
                let terminal = self
                    .state
                    .terminals
                    .get_mut(&terminal_id)
                    .ok_or_else(not_found)?;
                terminal.begin_forced_agent_suspend(
                    session.clone(),
                    now + FORCE_INTERRUPT_SETTLE,
                    crate::terminal::ForcedExitInput {
                        input: exit_input,
                        send_by: now + FORCE_INTERRUPT_WAIT,
                    },
                );
            }
            // Fork (force suspend): typing into a busy agent without an
            // interrupt could answer its dialog; terminate it instead.
            Some(_) => {
                let terminal = self
                    .state
                    .terminals
                    .get_mut(&terminal_id)
                    .ok_or_else(not_found)?;
                terminal.begin_agent_suspend(session.clone(), now);
            }
            None => {
                let runtime = self
                    .terminal_runtimes
                    .get(&terminal_id)
                    .ok_or_else(not_found)?;
                let (text, enter) =
                    super::api_helpers::encode_api_submission_parts(runtime, exit_input);
                let terminal = self
                    .state
                    .terminals
                    .get_mut(&terminal_id)
                    .ok_or_else(not_found)?;
                terminal.begin_agent_suspend(session.clone(), now + SUSPEND_GRACEFUL_EXIT_GRACE);
                if let Err(err) = runtime.queue_user_input_submission(
                    Bytes::from(text),
                    Bytes::from(enter),
                    super::api::AGENT_PROMPT_SUBMIT_DELAY,
                    None,
                ) {
                    terminal.take_suspended_agent();
                    return Err(AgentSuspendError::InputFailed(err.to_string()));
                }
                // Fork (agents v2): a scripted write, for the turn origin.
                self.note_input(
                    &terminal_id,
                    crate::agents_model::InputSource::Programmatic(
                        crate::agents_model::Programmatic::Api,
                    ),
                );
            }
        }
        // The parked session must outlive the agent's own transcript
        // retention; queue it for the next backup pass rather than waiting
        // for the periodic interval.
        self.queue_agent_transcript_backup(resolved.ws_idx, resolved.pane_id, session);
        if marked {
            self.note_agent_resume_marks_changed();
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_agent_status_transition(resolved.ws_idx, resolved.pane_id);
        Ok(self
            .public_pane_id(resolved.ws_idx, resolved.pane_id)
            .unwrap_or_else(|| target.to_string()))
    }

    /// Every check a suspend makes, in order, without writing. Fork (force
    /// suspend): `force` skips the four a forced exit overrides (blocked,
    /// subagents, background tasks, working); the rest still refuse.
    fn suspend_checks(
        &self,
        target: &str,
        resolved: &TerminalTarget,
        force: bool,
    ) -> Result<SuspendReady, AgentSuspendError> {
        let not_found = || {
            AgentSuspendError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            })
        };
        let terminal_id = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
            .ok_or_else(not_found)?;
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or_else(not_found)?;
        if terminal.suspended_agent.is_some() {
            return Err(AgentSuspendError::AlreadySuspended(target.to_string()));
        }
        // The exit command is typed into the agent's prompt; a blocked
        // approval or question UI would swallow it, like `agent.prompt`.
        if !force && terminal.state == crate::detect::AgentState::Blocked {
            return Err(AgentSuspendError::Blocked(target.to_string()));
        }
        // Exiting stops the agent's background subagents and loses their
        // work; wait until the count reaches zero (no override). Checked before
        // Working: an idle agent can still have background subagents.
        let subagents = terminal.active_subagent_count();
        if !force && subagents > 0 {
            return Err(AgentSuspendError::SubagentsRunning {
                target: target.to_string(),
                count: subagents,
            });
        }
        // Fork (autosuspend): likewise its background shells, monitors,
        // workflows and session crons (the Stop hook's snapshot).
        let tasks = terminal.active_background_task_count();
        if !force && tasks > 0 {
            return Err(AgentSuspendError::BackgroundTasksRunning {
                target: target.to_string(),
                count: tasks,
            });
        }
        // A working agent would lose its turn; wait for it to go idle.
        if !force && terminal.state == crate::detect::AgentState::Working {
            return Err(AgentSuspendError::Working(target.to_string()));
        }
        let Some(expected_agent) = terminal.effective_known_agent() else {
            return Err(AgentSuspendError::NotRunning(target.to_string()));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(AgentSuspendError::NotRunning(target.to_string()));
        }
        let Some(session) = terminal.suspendable_agent_session() else {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: "no native session reference is known for it yet".into(),
                kind: NotSuspendableReason::NoSession,
            });
        };
        if crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
            .is_none()
        {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: format!("{} has no native resume plan", session.agent),
                kind: NotSuspendableReason::NoResumePlan,
            });
        }
        let Some(exit_input) = crate::agent_resume::graceful_exit_input(&session.agent) else {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: format!("{} has no graceful exit command", session.agent),
                kind: NotSuspendableReason::NoGracefulExit,
            });
        };
        let runtime = self
            .terminal_runtimes
            .get(&terminal_id)
            .ok_or_else(not_found)?;
        if !runtime_hosts_agent(runtime, expected_agent) {
            return Err(AgentSuspendError::NotRunning(target.to_string()));
        }
        if runtime.child_pid().is_some()
            && agent_is_pane_process(
                runtime.child_pid(),
                live_runtime_agent_job(runtime).as_ref(),
                expected_agent,
            )
        {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: "it is the pane's process; close the tab instead".into(),
                kind: NotSuspendableReason::PaneProcess,
            });
        }
        Ok(SuspendReady {
            terminal_id,
            session,
            exit_input,
        })
    }

    /// Exit the live agent hosted by `target` and relaunch it in the same
    /// pane with its native resume command, as one request.
    ///
    /// Only an idle agent restarts: a working one would lose its turn, a
    /// blocked one would swallow the exit input, and a suspended one is
    /// activated instead. The suspend runs now; the relaunch is owed to the
    /// record and performed by [`App::start_pending_agent_restarts`].
    pub(super) fn restart_agent(&mut self, target: &str) -> Result<String, AgentRestartError> {
        self.restart_agent_by(target, None)
    }

    /// Fork (agents v2): [`Self::restart_agent`], recording who restarted it
    /// (`None` is the user): an abandoned restart leaves the agent suspended
    /// by them.
    pub(super) fn restart_agent_by(
        &mut self,
        target: &str,
        by: Option<crate::api::schema::agents_model::AgentsWho>,
    ) -> Result<String, AgentRestartError> {
        self.restart_agent_with(target, by, false)
    }

    /// Fork (force suspend): [`Self::restart_agent`] as a forced suspend
    /// ([`Self::force_suspend_agent`]) plus the usual automatic relaunch.
    pub(super) fn force_restart_agent(
        &mut self,
        target: &str,
    ) -> Result<String, AgentRestartError> {
        self.restart_agent_with(target, None, true)
    }

    fn restart_agent_with(
        &mut self,
        target: &str,
        by: Option<crate::api::schema::agents_model::AgentsWho>,
        force: bool,
    ) -> Result<String, AgentRestartError> {
        let resolved = self
            .resolve_suspended_agent_target(target)
            .map_err(|err| match err {
                AgentActivateError::Target(err) => AgentRestartError::Target(err),
                _ => AgentRestartError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                }),
            })?;
        let terminal = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
            .ok_or_else(|| {
                AgentRestartError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })?;
        let terminal_id = terminal.id.clone();
        if terminal.suspended_agent.is_some() {
            return Err(AgentRestartError::Suspended(target.to_string()));
        }
        // Blocked keeps its restart-specific wording; the working guard is
        // the shared one in `suspend_resolved_agent`.
        if !force && terminal.state == crate::detect::AgentState::Blocked {
            return Err(AgentRestartError::Blocked(target.to_string()));
        }
        self.fill_resolved_process_agent_session(&resolved);
        let pane_id = self
            .suspend_resolved_agent(target, &resolved, force)
            .map_err(AgentRestartError::Suspend)?;
        let now = Instant::now();
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.mark_suspended_agent_resume_pending(now, now + RESTART_RESUME_TIMEOUT, force);
            if let (Some(by), Some(record)) = (by, terminal.suspended_agent.as_mut()) {
                record.suspended_by = Some(by);
            }
        }
        // Fork (restart screen): clients show the pane restarting.
        self.note_agent_resume_marks_changed();
        Ok(pane_id)
    }

    /// Relaunch every restarted agent whose activation preconditions now
    /// hold: the exit was observed and the pane is back at an available shell
    /// prompt. Runs on every scheduler pass; the shell probe is rate-limited
    /// by the record's `next_check`.
    ///
    /// A restart is abandoned (the agent stays suspended, with a warning)
    /// when the exit escalation had to `SIGKILL` the agent, when activation
    /// fails for any reason other than the pane not being ready yet, or when
    /// [`RESTART_RESUME_TIMEOUT`] passes first. Fork (force suspend): a
    /// forced restart is not abandoned for the `SIGKILL`: it still waits for
    /// the observed exit and an available shell prompt, within the timeout.
    pub(crate) fn start_pending_agent_restarts(&mut self, now: Instant) -> bool {
        // Fork (restart screen): a lapsed wake or failed mark leaves the push.
        let mut expired = false;
        for terminal in self.state.terminals.values_mut() {
            let (changed, lapsed) =
                terminal.expire_suspended_agent_resume_marks(now, RESUME_FAILED_MARK_TTL);
            expired |= changed;
            if let Some(kind) = lapsed {
                tracing::warn!(
                    event = "agent.resume.undetected",
                    terminal_id = %terminal.id,
                    kind = kind.name(),
                    reason = "timeout",
                    "the relaunched agent was not detected in time"
                );
            }
        }
        expired |= self.fail_unstarted_relaunches(now);
        if expired {
            self.note_agent_resume_marks_changed();
        }
        let pending: Vec<_> = self
            .state
            .terminals
            .values()
            .filter_map(|terminal| {
                let record = terminal.suspended_agent.as_ref()?;
                let resume = record.resume_pending()?;
                Some((
                    terminal.id.clone(),
                    record.agent.clone(),
                    resume,
                    record.escalation(),
                    record.exit_observed(),
                ))
            })
            .collect();
        let mut changed = expired;
        for (terminal_id, agent, resume, escalation, exit_observed) in pending {
            let abandon = if escalation == SuspendExitEscalation::Killed && !resume.forced {
                Some("the agent had to be killed; leaving it suspended".to_string())
            } else if now >= resume.give_up_at {
                Some(if exit_observed {
                    "the pane never returned to an available shell prompt; leaving the agent suspended"
                        .to_string()
                } else {
                    "the agent exit was never observed; leaving it suspended".to_string()
                })
            } else {
                None
            };
            if let Some(reason) = abandon {
                self.abandon_agent_restart(&terminal_id, &agent, &reason);
                changed = true;
                continue;
            }
            if !exit_observed || now < resume.next_check {
                continue;
            }
            let Some(resolved) = self
                .terminal_targets()
                .into_iter()
                .find(|candidate| candidate.terminal_id == terminal_id.to_string())
            else {
                self.abandon_agent_restart(&terminal_id, &agent, "the pane is gone");
                changed = true;
                continue;
            };
            // Fork (agents v2): the relaunch types into the pane's shell; it
            // waits while the user types there (the typing guard).
            if self
                .terminal_runtimes
                .get(&terminal_id)
                .is_some_and(|runtime| {
                    super::typing_guard::runtime_typing_block(None, runtime, now).is_some()
                })
            {
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    terminal.defer_suspended_agent_resume(now + RESTART_RESUME_POLL_INTERVAL);
                }
                continue;
            }
            let target = self
                .public_pane_id(resolved.ws_idx, resolved.pane_id)
                .unwrap_or_else(|| terminal_id.to_string());
            match self.activate_resolved_agent(&target, &resolved) {
                Ok(_) => {
                    tracing::info!(
                        event = "agent.restart.resumed",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        "relaunched the restarted agent with its native resume command"
                    );
                    changed = true;
                }
                Err(
                    AgentActivateError::ExitPending(_) | AgentActivateError::PaneNotAvailable(_),
                ) => {
                    if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                        terminal.defer_suspended_agent_resume(now + RESTART_RESUME_POLL_INTERVAL);
                    }
                }
                Err(err) => {
                    let reason = self.agent_activate_error_body(err).message;
                    self.abandon_agent_restart(&terminal_id, &agent, &reason);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Fork (restart screen): a relaunch typed into the pane's shell whose
    /// agent never started (the shell is back at an available prompt and no
    /// agent is detected) ends at its probe instead of holding the restarting
    /// card until `detect_by`, with a toast. Probes are rate-limited by the
    /// mark's `probe_at`. Returns whether a mark changed.
    fn fail_unstarted_relaunches(&mut self, now: Instant) -> bool {
        let due: Vec<_> = self
            .state
            .terminals
            .values()
            .filter_map(|terminal| {
                terminal
                    .agent_resuming_probe_due(now)
                    .map(|resuming| (terminal.id.clone(), resuming))
            })
            .collect();
        let mut changed = false;
        for (terminal_id, resuming) in due {
            let shell_back = self
                .terminal_runtimes
                .get(&terminal_id)
                .is_some_and(|runtime| available_shell_name(runtime).is_some());
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            if !shell_back || terminal.effective_known_agent().is_some() {
                terminal.defer_agent_resuming_probe(now + RESTART_RESUME_POLL_INTERVAL);
                continue;
            }
            let name = terminal
                .agent_name
                .clone()
                .or_else(|| {
                    resuming
                        .agent
                        .map(|agent| crate::detect::agent_label(agent).to_string())
                })
                .unwrap_or_else(|| "agent".to_string());
            terminal.agent_resuming = None;
            changed = true;
            tracing::warn!(
                event = "agent.resume.undetected",
                terminal_id = %terminal_id,
                kind = resuming.kind.name(),
                agent = %name,
                reason = "shell_back",
                "the relaunched agent never started; the shell is back"
            );
            self.queue_relaunch_failure_notice(&terminal_id, resuming.kind, &name);
        }
        if changed {
            self.note_agent_resume_marks_changed();
        }
        changed
    }

    /// Fork (restart screen): queue the toast for a relaunch that never
    /// started; a muted tab gets none.
    fn queue_relaunch_failure_notice(
        &mut self,
        terminal_id: &TerminalId,
        kind: crate::terminal::AgentResumeKind,
        name: &str,
    ) {
        let Some(target) = self
            .terminal_targets()
            .into_iter()
            .find(|candidate| candidate.terminal_id == terminal_id.to_string())
        else {
            return;
        };
        if self.state.pane_notifications_muted(target.pane_id) {
            return;
        }
        let Some(workspace) = self.state.workspaces.get(target.ws_idx) else {
            return;
        };
        let label = workspace.display_name_from(&self.state.terminals, &self.terminal_runtimes);
        let body =
            super::actions::notification_context(workspace, &label, target.ws_idx, target.pane_id);
        let title = match kind {
            crate::terminal::AgentResumeKind::Restart => {
                format!("{name}: restart didn't start — the shell is back")
            }
            crate::terminal::AgentResumeKind::Wake => {
                format!("{name}: couldn't wake — the shell is back")
            }
        };
        let notice = RelaunchFailureNotice {
            title,
            body: (!body.is_empty()).then_some(body),
            workspace_id: self.public_workspace_id(target.ws_idx),
            tab_id: self.public_tab_id(target.ws_idx, target.tab_idx),
            pane_id: self.public_pane_id(target.ws_idx, target.pane_id),
        };
        self.relaunch_failure_notices.push(notice);
    }

    fn abandon_agent_restart(&mut self, terminal_id: &TerminalId, agent: &str, reason: &str) {
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.clear_suspended_agent_resume_pending();
            // Fork (restart screen): the card says the restart didn't finish.
            terminal.fail_agent_resume(
                crate::terminal::AgentResumeKind::Restart,
                Instant::now() + RESUME_FAILED_MARK_TTL,
            );
        }
        self.note_agent_resume_marks_changed();
        tracing::warn!(
            event = "agent.restart.abandoned",
            terminal_id = %terminal_id,
            agent = %agent,
            reason,
            "agent restart did not relaunch the agent"
        );
    }

    /// Relaunch the parked agent hosted by `target` in its own pane.
    pub(super) fn activate_agent(&mut self, target: &str) -> Result<String, AgentActivateError> {
        let resolved = self.resolve_suspended_agent_target(target)?;
        self.activate_resolved_agent(target, &resolved)
    }

    fn activate_resolved_agent(
        &mut self,
        target: &str,
        resolved: &TerminalTarget,
    ) -> Result<String, AgentActivateError> {
        let not_found = || {
            AgentActivateError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            })
        };
        let terminal_id = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
            .ok_or_else(not_found)?;
        self.advance_suspended_agent_session(&terminal_id);
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or_else(not_found)?;
        let Some(record) = terminal.suspended_agent.as_ref() else {
            return Err(AgentActivateError::NotSuspended(target.to_string()));
        };
        let Some(plan) = crate::agent_resume::plan(
            &record.session.source,
            &record.session.agent,
            &record.session.session_ref,
        ) else {
            return Err(AgentActivateError::NotSuspended(target.to_string()));
        };
        let kind = crate::detect::parse_agent_label(&record.agent);
        // Fork (restart screen): what the relaunch shows until it is detected.
        let resume_kind = if record.resume_pending().is_some() {
            crate::terminal::AgentResumeKind::Restart
        } else {
            crate::terminal::AgentResumeKind::Wake
        };

        let typed_into_shell = self.terminal_runtimes.get(&terminal_id).is_some();
        if let Some(runtime) = self.terminal_runtimes.get(&terminal_id) {
            // The shell can look available before detection publishes the
            // exit; that late report would then wipe the relaunched session.
            if !record.exit_observed() {
                return Err(AgentActivateError::ExitPending(target.to_string()));
            }
            let shell_name = available_shell_name(runtime)
                .ok_or_else(|| AgentActivateError::PaneNotAvailable(target.to_string()))?;
            let argv = self.herdr_launch_argv(&plan.agent, plan.argv.clone());
            let command = crate::platform::interactive_shell_command(&argv, &shell_name)
                .ok_or(AgentActivateError::InvalidArgument)?;
            let bytes = super::api_helpers::encode_api_submission(runtime, &command);
            self.restore_agent_transcript_before_resume(&record.session);
            let now = Instant::now();
            let terminal = self
                .state
                .terminals
                .get_mut(&terminal_id)
                .ok_or_else(not_found)?;
            let record = terminal.take_suspended_agent().ok_or_else(not_found)?;
            let managed = record.name.clone().zip(kind);
            if let Some((name, kind)) = managed.clone() {
                terminal.begin_managed_agent(
                    name,
                    kind,
                    now,
                    AGENT_START_SETTLE_DELAY,
                    DEFAULT_AGENT_START_TIMEOUT,
                );
            }
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                if managed.is_some() {
                    terminal.clear_agent_name();
                }
                let (suspended_by, auto) = (record.suspended_by.clone(), record.auto);
                terminal.restore_suspended_agent(record.agent, record.name, record.session);
                // Fork: the record stays whoever's it was (a message's wake
                // retries an auto-suspended one).
                if let Some(restored) = terminal.suspended_agent.as_mut() {
                    restored.suspended_by = suspended_by;
                    restored.auto = auto;
                }
                return Err(AgentActivateError::InputFailed(err.to_string()));
            }
            // Fork (agents v2): a scripted write, for the turn origin.
            terminal.turn_mut().note_input(
                &crate::agents_model::InputSource::Programmatic(
                    crate::agents_model::Programmatic::Api,
                ),
                now,
            );
            if managed.is_some() {
                terminal.set_managed_agent_launch_session(record.session);
            } else {
                terminal.set_persisted_agent_session(record.session);
            }
        } else {
            let (rows, cols) = self
                .state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == resolved.pane_id)
                .map(|info| (info.inner_rect.height, info.inner_rect.width))
                .filter(|(rows, cols)| *rows > 0 && *cols > 0)
                .unwrap_or(FALLBACK_RELAUNCH_SIZE);
            let terminal = self
                .state
                .terminals
                .get_mut(&terminal_id)
                .ok_or_else(not_found)?;
            let record = terminal.take_suspended_agent().ok_or_else(not_found)?;
            terminal.pending_agent_resume_plan = Some(plan);
            terminal.set_persisted_agent_session(record.session);
            if let Some((name, kind)) = record.name.zip(kind) {
                terminal.restore_managed_agent(name, kind);
            }
            self.start_pending_agent_resume_for_terminal(&terminal_id, rows, cols, true);
        }

        self.state.mark_session_dirty();
        self.schedule_session_save();
        // Fork (restart screen): the pane shows restarting or waking until
        // detection reports the agent again (or RESTART_RESUME_TIMEOUT).
        // Typed into a live shell: probed for a relaunch that never started.
        let now = Instant::now();
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.begin_agent_resuming(
                resume_kind,
                kind,
                now + RESTART_RESUME_TIMEOUT,
                typed_into_shell.then_some(now + AGENT_START_SETTLE_DELAY),
            );
        }
        self.note_agent_resume_marks_changed();
        self.emit_agent_status_transition(resolved.ws_idx, resolved.pane_id);
        Ok(self
            .public_pane_id(resolved.ws_idx, resolved.pane_id)
            .unwrap_or_else(|| target.to_string()))
    }

    /// The respawned shell of `terminal_id` hosts no agent. Fork (restart
    /// screen): a resume mark it carried leaves the push.
    pub(crate) fn clear_agent_identity_after_respawn(&mut self, terminal_id: &TerminalId) {
        let Some(terminal) = self.state.terminals.get_mut(terminal_id) else {
            return;
        };
        let marked = terminal.has_resume_marks();
        terminal.clear_agent_runtime_identity_after_respawn();
        if marked {
            self.note_agent_resume_marks_changed();
        }
    }

    /// Fork (restart screen): a parked agent's restarting, waking or failed
    /// mark changed: clients get the agent times push again. O(1).
    pub(crate) fn note_agent_resume_marks_changed(&mut self) {
        self.state.agent_times_view_rev = self.state.agent_times_view_rev.wrapping_add(1).max(1);
    }

    /// A parked Claude session that was backgrounded out of its pane and
    /// carried on under a new id (src/agent_session_chain.rs) is resumed as
    /// the newest session of its chain: the record follows it, and the
    /// session file is saved with the new id. Activation and restart read
    /// the record here, never on a render or detection path.
    fn advance_suspended_agent_session(&mut self, terminal_id: &TerminalId) {
        let Some(session) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.suspended_agent.as_ref())
            .map(|record| record.session.clone())
        else {
            return;
        };
        let Some(next) = crate::agent_session_chain::advance(&session) else {
            return;
        };
        // Another pane already runs (or parks) the newest session.
        if self.session_recorded_elsewhere(Some(terminal_id), &next.session_ref.value) {
            return;
        }
        self.follow_queued_session(&session.session_ref.value, &next.session_ref.value);
        if let Some(record) = self
            .state
            .terminals
            .get_mut(terminal_id)
            .and_then(|terminal| terminal.suspended_agent.as_mut())
        {
            record.session = next;
            self.state.mark_session_dirty();
        }
    }

    /// Fork (session chains): whether a terminal other than `except`
    /// records agent session `id` (a live, launched or parked agent).
    pub(crate) fn session_recorded_elsewhere(&self, except: Option<&TerminalId>, id: &str) -> bool {
        self.state.terminals.values().any(|terminal| {
            Some(&terminal.id) != except
                && terminal
                    .persistable_agent_session()
                    .is_some_and(|session| session.session_ref.value == id)
        })
    }

    /// Escalate suspended agents whose exit grace has run out.
    ///
    /// The foreground job is inspected for the parked agent kind; its pids are
    /// signaled with `SIGTERM`, then `SIGKILL`, never the pane shell itself.
    /// A probe that cannot read the job keeps waiting (up to a retry cap);
    /// once a job is read without the agent the wait ends. Only the detection
    /// loop's process-exit observation marks the exit as seen.
    pub(crate) fn escalate_suspended_agent_exits(&mut self, now: Instant) -> bool {
        self.escalate_suspended_agent_exits_with(
            now,
            |runtimes, terminal_id, expected| {
                let runtime = runtimes.get(terminal_id);
                suspend_probe_from_job(
                    runtime.and_then(|runtime| runtime.child_pid()),
                    runtime.and_then(live_runtime_agent_job),
                    expected,
                )
            },
            crate::platform::signal_processes,
        )
    }

    /// The escalation tick with its process I/O injected: `probe` reads the
    /// pane's foreground job and `signal` delivers the escalation signal. The
    /// state transition itself lives in `TerminalState`.
    pub(crate) fn escalate_suspended_agent_exits_with(
        &mut self,
        now: Instant,
        mut probe: impl FnMut(
            &crate::terminal::TerminalRuntimeRegistry,
            &TerminalId,
            Option<crate::detect::Agent>,
        ) -> SuspendProbe,
        mut signal: impl FnMut(&[u32], crate::platform::Signal),
    ) -> bool {
        let mut changed = self.send_forced_exit_inputs(now);
        let due: Vec<_> = self
            .state
            .terminals
            .values()
            .filter(|terminal| {
                terminal
                    .suspended_agent_exit_deadline()
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|terminal| terminal.id.clone())
            .collect();
        for terminal_id in due {
            let Some(agent) = self
                .state
                .terminals
                .get(&terminal_id)
                .and_then(|terminal| terminal.suspended_agent.as_ref())
                .map(|record| record.agent.clone())
            else {
                continue;
            };
            let expected = crate::detect::parse_agent_label(&agent);
            let probe = probe(&self.terminal_runtimes, &terminal_id, expected);
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            match terminal.advance_suspend_escalation(now, SUSPEND_SIGNAL_ESCALATION_GRACE, probe) {
                SuspendEscalationOutcome::NotDue => continue,
                SuspendEscalationOutcome::ProbeRetried { retries } => {
                    tracing::debug!(
                        event = "agent.suspend.probe_retry",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        retries,
                        "could not read the pane's foreground job; retrying the exit wait"
                    );
                }
                SuspendEscalationOutcome::ProbeGaveUp => {
                    let (retries, escalation) = terminal
                        .suspended_agent
                        .as_ref()
                        .map_or((0, SuspendExitEscalation::Pending), |record| {
                            (record.probe_retries(), record.escalation())
                        });
                    tracing::warn!(
                        event = "agent.suspend.probe_gave_up",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        retries,
                        escalation = ?escalation,
                        "could not read the pane's foreground job; giving up the exit wait, the agent may still be running"
                    );
                }
                SuspendEscalationOutcome::NoAgentProcess => {
                    tracing::debug!(
                        event = "agent.suspend.exit_wait_ended",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        "no agent process left in the pane's foreground job"
                    );
                }
                SuspendEscalationOutcome::Signal { signal: step, pids } => {
                    tracing::warn!(
                        event = "agent.suspend.escalate",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        pids = ?pids,
                        step = ?step,
                        "escalating suspended agent exit"
                    );
                    signal(&pids, step);
                }
                SuspendEscalationOutcome::Exhausted => {
                    tracing::debug!(
                        event = "agent.suspend.exit_wait_ended",
                        terminal_id = %terminal_id,
                        agent = %agent,
                        "agent process outlived SIGKILL; waiting on detection only"
                    );
                }
            }
            changed = true;
        }
        changed
    }

    /// Fork (force suspend): type the exit input a forced suspend owes once
    /// its interrupted agent is back at its prompt (neither working nor
    /// blocked); past [`FORCE_INTERRUPT_WAIT`], or when the input cannot be
    /// typed, the exit escalation starts now instead (`SIGTERM`). Never types
    /// into a dialog.
    fn send_forced_exit_inputs(&mut self, now: Instant) -> bool {
        let due: Vec<_> =
            self.state
                .terminals
                .values()
                .filter_map(|terminal| {
                    let record = terminal.suspended_agent.as_ref()?;
                    let forced = record.forced_exit()?;
                    (record.exit_observed() || record.exit_deadline().is_some_and(|at| now >= at))
                        .then(|| {
                            // Back at its prompt: idle, still the agent the Esc
                            // went to (the screen is read below, only then).
                            let idle_agent = (terminal.state == crate::detect::AgentState::Idle)
                                .then(|| terminal.effective_known_agent())
                                .flatten()
                                .filter(|agent| {
                                    matches!(
                                        agent,
                                        crate::detect::Agent::Claude | crate::detect::Agent::Codex
                                    )
                                });
                            (
                                terminal.id.clone(),
                                forced,
                                idle_agent,
                                record.exit_observed(),
                            )
                        })
                })
                .collect();
        let mut changed = false;
        for (terminal_id, forced, idle_agent, exit_observed) in due {
            if exit_observed {
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    terminal.finish_forced_exit_wait(None);
                }
                changed = true;
                continue;
            }
            // Its input box is on screen and holds no draft (not the typing
            // guard: its recent-input hold outlasts the force wait).
            let free = idle_agent.is_some_and(|agent| {
                self.terminal_runtimes
                    .get(&terminal_id)
                    .is_some_and(|runtime| {
                        let rows = usize::from(runtime.current_size().0.max(1));
                        super::typing_guard::composer(
                            agent,
                            &runtime.recent_ansi_snapshot(rows).text,
                        ) == super::typing_guard::Composer::Free
                    })
            });
            if !free && now < forced.send_by {
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    terminal.defer_forced_exit(
                        (now + RESTART_RESUME_POLL_INTERVAL).min(forced.send_by),
                    );
                }
                continue;
            }
            changed = true;
            let typed = free
                && self
                    .terminal_runtimes
                    .get(&terminal_id)
                    .is_some_and(|runtime| {
                        let (text, enter) =
                            super::api_helpers::encode_api_submission_parts(runtime, forced.input);
                        runtime
                            .queue_user_input_submission(
                                Bytes::from(text),
                                Bytes::from(enter),
                                super::api::AGENT_PROMPT_SUBMIT_DELAY,
                                None,
                            )
                            .is_ok()
                    });
            if typed {
                self.note_input(
                    &terminal_id,
                    crate::agents_model::InputSource::Programmatic(
                        crate::agents_model::Programmatic::Api,
                    ),
                );
            }
            tracing::info!(
                event = "agent.suspend.forced_exit",
                terminal_id = %terminal_id,
                typed,
                "forced suspend: {}",
                if typed {
                    "typed the exit input after the interrupt"
                } else {
                    "the agent did not come back to its prompt; terminating it"
                }
            );
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.finish_forced_exit_wait(Some(if typed {
                    now + SUSPEND_GRACEFUL_EXIT_GRACE
                } else {
                    now
                }));
            }
        }
        changed
    }

    /// Resolve an activation target: a pane id, a live agent name, or the
    /// name stored in a suspended record.
    fn resolve_suspended_agent_target(
        &self,
        target: &str,
    ) -> Result<TerminalTarget, AgentActivateError> {
        match self.resolve_agent_target(target) {
            Ok(resolved) => return Ok(resolved),
            Err(err @ TerminalTargetError::Ambiguous { .. }) => {
                return Err(AgentActivateError::Target(err));
            }
            Err(TerminalTargetError::NotFound { .. }) => {}
        }
        let matches: Vec<_> = self
            .terminal_targets()
            .into_iter()
            .filter(|candidate| {
                self.state
                    .terminals
                    .values()
                    .find(|terminal| terminal.id.to_string() == candidate.terminal_id)
                    .and_then(|terminal| terminal.suspended_agent.as_ref())
                    .is_some_and(|record| record.name.as_deref() == Some(target))
            })
            .collect();
        match matches.len() {
            0 => Err(AgentActivateError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            })),
            1 => Ok(matches.into_iter().next().expect("one match")),
            _ => Err(AgentActivateError::Target(TerminalTargetError::Ambiguous {
                target: target.to_string(),
                candidates: matches
                    .into_iter()
                    .filter_map(|candidate| {
                        self.terminal_target_candidate(candidate.ws_idx, candidate.pane_id)
                    })
                    .collect(),
            })),
        }
    }

    fn emit_agent_status_transition(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        self.mark_coordinator_input_dirty();
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return;
        };
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneAgentStatusChanged,
            data: crate::api::schema::EventData::PaneAgentStatusChanged {
                pane_id: pane.pane_id,
                workspace_id: pane.workspace_id,
                agent_status: pane.agent_status,
                agent: pane.agent,
                title: pane.title,
                display_agent: pane.display_agent,
                state_labels: pane.state_labels,
            },
        });
        self.emit_pane_updated(ws_idx, pane_id);
    }

    pub(super) fn agent_suspend_error_body(
        &self,
        err: AgentSuspendError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentSuspendError::Target(err) => self.agent_target_error_body(err),
            AgentSuspendError::NotRunning(target) => crate::api::schema::ErrorBody {
                code: "agent_not_ready".into(),
                message: format!("agent {target} is not a running agent"),
            },
            AgentSuspendError::Blocked(target) => crate::api::schema::ErrorBody {
                code: "agent_blocked".into(),
                message: format!(
                    "agent {target} is blocked on a prompt; answer it before suspending"
                ),
            },
            AgentSuspendError::Working(target) => crate::api::schema::ErrorBody {
                code: "agent_working".into(),
                message: format!("agent {target} is working; wait for idle or blocked-free state"),
            },
            AgentSuspendError::SubagentsRunning { target, count } => {
                crate::api::schema::ErrorBody {
                    code: "agent_subagents_running".into(),
                    message: format!(
                        "agent {target} has {count} subagent{} running; suspending would stop {}. Wait for {} to finish",
                        if count == 1 { "" } else { "s" },
                        if count == 1 { "it" } else { "them" },
                        if count == 1 { "it" } else { "them" },
                    ),
                }
            }
            AgentSuspendError::BackgroundTasksRunning { target, count } => {
                crate::api::schema::ErrorBody {
                    code: "background_tasks_running".into(),
                    message: format!(
                        "agent {target} has {count} background task{} running (a shell, a monitor, a loop); suspending would stop {}. Wait for {} to finish",
                        if count == 1 { "" } else { "s" },
                        if count == 1 { "it" } else { "them" },
                        if count == 1 { "it" } else { "them" },
                    ),
                }
            }
            AgentSuspendError::AlreadySuspended(target) => crate::api::schema::ErrorBody {
                code: "agent_already_suspended".into(),
                message: format!("agent {target} is already suspended"),
            },
            AgentSuspendError::NotSuspendable { target, reason, .. } => crate::api::schema::ErrorBody {
                code: "agent_not_suspendable".into(),
                message: format!("agent {target} cannot be suspended: {reason}"),
            },
            AgentSuspendError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_suspend_input_failed".into(),
                message,
            },
        }
    }

    /// Fork (force suspend): the refusal of a forced `action` that did not
    /// come from the client shell lane: agents (their MCP tools, the CLI)
    /// keep today's refusals.
    pub(super) fn force_user_only_refusal(
        &self,
        action: &str,
    ) -> Option<crate::api::schema::ErrorBody> {
        (!self.request_from_client_shell).then(|| crate::api::schema::ErrorBody {
            code: "force_user_only".into(),
            message: format!(
                "only the user can force a {action}, from herdr's interface; use agent.{action}"
            ),
        })
    }

    pub(super) fn agent_restart_error_body(
        &self,
        err: AgentRestartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRestartError::Target(err) => self.agent_target_error_body(err),
            AgentRestartError::Blocked(target) => crate::api::schema::ErrorBody {
                code: "agent_blocked".into(),
                message: format!(
                    "agent {target} is blocked on a prompt; answer it before restarting"
                ),
            },
            AgentRestartError::Suspended(target) => crate::api::schema::ErrorBody {
                code: "agent_suspended".into(),
                message: format!("agent {target} is suspended; activate it instead"),
            },
            AgentRestartError::Suspend(err) => self.agent_suspend_error_body(err),
        }
    }

    pub(super) fn agent_activate_error_body(
        &self,
        err: AgentActivateError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentActivateError::Target(err) => self.agent_target_error_body(err),
            AgentActivateError::NotSuspended(target) => crate::api::schema::ErrorBody {
                code: "agent_not_suspended".into(),
                message: format!("agent {target} is not suspended"),
            },
            AgentActivateError::PaneNotAvailable(target) => crate::api::schema::ErrorBody {
                code: "pane_not_available".into(),
                message: format!(
                    "the pane hosting suspended agent {target} is not at an available shell prompt"
                ),
            },
            AgentActivateError::ExitPending(target) => crate::api::schema::ErrorBody {
                code: "pane_not_available".into(),
                message: format!(
                    "the process of suspended agent {target} is still exiting; try again in a moment"
                ),
            },
            AgentActivateError::InvalidArgument => crate::api::schema::ErrorBody {
                code: "invalid_agent_argument".into(),
                message: "the resume command cannot be encoded safely for the target shell".into(),
            },
            AgentActivateError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_activate_input_failed".into(),
                message,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::api::schema::{
        AgentActivateParams, AgentRestartParams, AgentStatus, AgentSuspendParams, Method,
    };
    use crate::detect::{Agent, AgentState};
    use crate::terminal::SuspendExitEscalation;
    use crate::workspace::Workspace;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("suspend")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        app
    }

    fn root_terminal_id(app: &App) -> crate::terminal::TerminalId {
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone()
    }

    fn claude_session(id: &str) -> crate::agent_resume::PersistedAgentSession {
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id(id).unwrap(),
            transcript_path: None,
        }
    }

    fn host_live_agent(app: &mut App, agent: Agent, name: &str, session: Option<(&str, &str)>) {
        let terminal_id = root_terminal_id(app);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(agent), AgentState::Idle);
        if let Some((source, session_id)) = session {
            terminal
                .set_agent_session_ref(
                    source.into(),
                    crate::detect::agent_label(agent).into(),
                    crate::agent_resume::AgentSessionRef::id(session_id),
                    Some(1),
                )
                .expect("session ref accepted");
        }
        terminal.set_agent_name(name.into());
    }

    fn request(app: &mut App, method: Method) -> serde_json::Value {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method,
        });
        serde_json::from_str(&response).unwrap()
    }

    fn suspend(app: &mut App, target: &str) -> serde_json::Value {
        request(
            app,
            Method::AgentSuspend(AgentSuspendParams {
                target: target.into(),
            }),
        )
    }

    fn activate(app: &mut App, target: &str) -> serde_json::Value {
        request(
            app,
            Method::AgentActivate(AgentActivateParams {
                target: target.into(),
            }),
        )
    }

    fn restart(app: &mut App, target: &str) -> serde_json::Value {
        request(
            app,
            Method::AgentRestart(AgentRestartParams {
                target: target.into(),
            }),
        )
    }

    fn agent_status(app: &mut App, target: &str) -> AgentStatus {
        let agent = app
            .agent_info_for_target(target)
            .unwrap_or_else(|_| panic!("agent {target} should resolve"));
        agent.agent_status
    }

    async fn next_input(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> bytes::Bytes {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("pane input arrives")
            .expect("runtime channel open")
    }

    fn observe_exit(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let observed_at = Instant::now();
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited: true,
            observed_at,
        });
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited: false,
            observed_at: observed_at + Duration::from_millis(10),
        });
    }

    #[tokio::test]
    async fn suspend_stores_the_record_before_the_exit_input_and_reports_suspended() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let pane_id = app
            .pane_info(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .pane_id;

        let response = suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        assert_eq!(response["result"]["pane_id"], pane_id);

        let terminal = &app.state.terminals[&terminal_id];
        let record = terminal.suspended_agent.as_ref().expect("record stored");
        assert_eq!(record.name.as_deref(), Some("reviewer"));
        assert_eq!(record.session, claude_session("claude-session"));
        assert!(record.exit_deadline().is_some());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        assert_eq!(agent_status(&mut app, &pane_id), AgentStatus::Suspended);
        assert!(app.collect_agent_infos().iter().any(|agent| {
            agent.name.as_deref() == Some("reviewer")
                && agent.agent_status == AgentStatus::Suspended
                && agent
                    .agent_session
                    .as_ref()
                    .map(|session| session.value.as_str())
                    == Some("claude-session")
        }));
        assert!(app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| matches!(
                &event.data,
                crate::api::schema::EventData::PaneAgentStatusChanged { agent_status, .. }
                    if *agent_status == AgentStatus::Suspended
            )));

        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));

        let again = suspend(&mut app, "reviewer");
        assert_eq!(again["error"]["code"], "agent_already_suspended");
    }

    const ARGV_SESSION: &str = "5b21611c-f122-4167-be9a-d57b522a9979";

    fn give_argv(app: &mut App, argv: &[&str]) {
        let terminal_id = root_terminal_id(app);
        app.agents_model.test_process_argvs.insert(
            terminal_id,
            vec![
                vec!["-zsh".into()],
                argv.iter().map(|part| part.to_string()).collect(),
            ],
        );
    }

    fn reported_session(app: &mut App, target: &str) -> Option<String> {
        app.agent_info_for_target(target)
            .ok()?
            .agent_session
            .map(|session| session.value)
    }

    #[tokio::test]
    async fn suspend_reads_a_missing_session_from_the_agent_argv() {
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Claude, "reviewer", None);
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        give_argv(
            &mut app,
            &[
                "/Users/u/.local/bin/claude",
                "--resume",
                ARGV_SESSION,
                "--mcp-config=/tmp/x.json",
            ],
        );

        // agent.get reports it.
        let got = request(
            &mut app,
            Method::AgentGet(crate::api::schema::AgentTarget {
                target: "reviewer".into(),
            }),
        );
        assert_eq!(
            got["result"]["agent"]["agent_session"]["value"], ARGV_SESSION,
            "{got}"
        );

        let response = suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("record stored");
        assert_eq!(record.session, claude_session(ARGV_SESSION));
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
    }

    #[tokio::test]
    async fn a_codex_resume_argv_gives_the_session() {
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Codex, "worker", None);
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        give_argv(&mut app, &["codex", "resume", "codex-session"]);

        let response = suspend(&mut app, "worker");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("record stored");
        assert_eq!(record.session.source, "herdr:codex");
        assert_eq!(record.session.session_ref.value, "codex-session");
    }

    #[tokio::test]
    async fn a_hook_reported_session_wins_over_the_argv() {
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Claude, "reviewer", None);
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        give_argv(&mut app, &["claude", "--resume", ARGV_SESSION]);
        app.fill_process_agent_sessions();
        assert_eq!(
            reported_session(&mut app, "reviewer").as_deref(),
            Some(ARGV_SESSION)
        );

        // A later hook report names the session: it wins everywhere.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("hook-session"),
                Some(1),
            )
            .expect("session ref accepted");
        app.fill_process_agent_sessions();
        assert_eq!(
            reported_session(&mut app, "reviewer").as_deref(),
            Some("hook-session")
        );
        let response = suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        assert_eq!(
            app.state.terminals[&terminal_id]
                .suspended_agent
                .as_ref()
                .expect("record stored")
                .session,
            claude_session("hook-session")
        );
    }

    #[tokio::test]
    async fn an_argv_without_a_session_leaves_the_agent_unsuspendable() {
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Claude, "reviewer", None);
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        give_argv(
            &mut app,
            &["claude", "--resume", ARGV_SESSION, "--fork-session"],
        );

        let response = suspend(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_not_suspendable",
            "{response}"
        );
        assert_eq!(reported_session(&mut app, "reviewer"), None);

        // The derived session is only reported while its agent is the live one.
        give_argv(&mut app, &["claude", "--session-id", ARGV_SESSION]);
        app.fill_process_agent_sessions();
        assert_eq!(
            reported_session(&mut app, "reviewer").as_deref(),
            Some(ARGV_SESSION)
        );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(app.state.terminals[&terminal_id]
            .process_agent_session()
            .is_none());
    }

    #[tokio::test]
    async fn suspend_needs_a_native_session_and_a_graceful_exit_command() {
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Claude, "reviewer", None);
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_not_suspendable",
            "{response}"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());

        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::GithubCopilot,
            "worker",
            Some(("herdr:copilot", "copilot-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "worker");
        assert_eq!(
            response["error"]["code"], "agent_not_suspendable",
            "{response}"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("graceful exit"));
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert_eq!(agent_status(&mut app, "worker"), AgentStatus::Idle);
    }

    #[tokio::test]
    async fn process_exit_keeps_the_suspended_pane_listed_by_name() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );

        observe_exit(&mut app);

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_some());
        assert!(terminal.suspended_agent_exit_deadline().is_none());
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(terminal.effective_agent_label(), None);
        let agent = app.agent_info_for_target("reviewer").expect("still listed");
        assert_eq!(agent.agent_status, AgentStatus::Suspended);
        assert_eq!(agent.agent.as_deref(), Some("claude"));
        assert!(!agent.interactive_ready);
        assert!(!agent.launch_pending);
        assert_eq!(
            app.tab_info(0, 0).unwrap().agent_status,
            AgentStatus::Suspended
        );
        assert_eq!(app.workspace_info(0).agent_status, AgentStatus::Suspended);
    }

    #[tokio::test]
    async fn activate_clears_the_record_and_relaunches_with_the_resume_plan() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));
        observe_exit(&mut app);

        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_none());
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.managed_agent_launch_pending());
        assert_eq!(terminal.managed_agent_kind(), Some(Agent::Claude));
        assert_eq!(
            terminal.persisted_agent_session,
            Some(claude_session("claude-session"))
        );
        let plan = crate::agent_resume::plan(
            "herdr:claude",
            "claude",
            &crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        )
        .unwrap();
        let expected = crate::platform::interactive_shell_command(&plan.argv, "sh").unwrap();
        let sent = next_input(&mut rx).await;
        let sent = String::from_utf8(sent.to_vec()).unwrap();
        assert!(
            sent.starts_with(&expected),
            "sent {sent:?}, expected {expected:?}"
        );
        assert!(sent.ends_with('\r'));
        assert_ne!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);

        let again = activate(&mut app, "reviewer");
        assert_eq!(again["error"]["code"], "agent_not_suspended", "{again}");
    }

    #[tokio::test]
    async fn activate_rejects_live_agents_and_unknown_targets() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        assert_eq!(
            activate(&mut app, "reviewer")["error"]["code"],
            "agent_not_suspended"
        );
        assert_eq!(
            activate(&mut app, "nobody")["error"]["code"],
            "agent_not_found"
        );
    }

    #[tokio::test]
    async fn activate_without_a_runtime_uses_the_pending_resume_path() {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_suspended_agent(
                "claude".into(),
                Some("reviewer".into()),
                claude_session("claude-session"),
            );
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        app.state.default_shell = "__herdr_missing_activate_shell__".into();

        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_none());
        assert!(
            terminal.pending_agent_resume_plan.is_none(),
            "the plan was consumed by the deferred resume attempt"
        );
        assert!(
            terminal.restore_error.is_some(),
            "a failed shell spawn surfaces as a restore error like any deferred resume"
        );
    }

    #[tokio::test]
    async fn suspended_exit_never_notifies_or_marks_the_pane_unseen() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        // Another tab is active so a completion here would normally notify.
        app.state.workspaces[0].test_add_tab(Some("other"));
        app.state.workspaces[0].active_tab = 1;
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        // The exiting process is seen working (for example while it saves),
        // so its exit is a working-to-idle transition that would normally
        // count as a completion.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_some());

        observe_exit(&mut app);

        assert!(
            app.state.pending_agent_notifications.is_empty(),
            "a parked agent's exit is not a completion"
        );
        let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
        assert!(pane.seen, "nothing to look at once the agent is parked");
        assert_eq!(
            app.state.terminals[&terminal_id].last_agent_completion_seq,
            None
        );
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
    }

    #[tokio::test]
    async fn suspend_rejects_a_blocked_agent_without_writing() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "reviewer");
        assert_eq!(response["error"]["code"], "agent_blocked", "{response}");
        assert_eq!(
            response["error"]["message"],
            "agent reviewer is blocked on a prompt; answer it before suspending"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Blocked);
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a blocked suspend wrote or scheduled terminal input"
        );
    }

    #[tokio::test]
    async fn activate_waits_until_detection_observed_the_exit() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));

        // The shell prompt can look available before the exit is published.
        let response = activate(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "pane_not_available",
            "{response}"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("still exiting"));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_some(), "the record is kept");
        assert!(!terminal.managed_agent_launch_pending());
        assert!(rx.try_recv().is_err(), "nothing was launched");

        observe_exit(&mut app);
        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
    }

    #[tokio::test]
    async fn a_manual_relaunch_dropping_the_record_publishes_the_new_status() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        observe_exit(&mut app);
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        let seen_events = app
            .event_hub
            .events_after(0)
            .last()
            .map(|(seq, _)| *seq)
            .unwrap_or(0);
        app.state.session_dirty = false;

        // The same agent kind seen live after the observed exit: the user
        // relaunched it by hand, so the parked record no longer applies.
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited: false,
            observed_at: Instant::now(),
        });

        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert!(
            app.state.session_dirty,
            "dropping the persisted record must be saved"
        );
        let statuses: Vec<_> = app
            .event_hub
            .events_after(seen_events)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                crate::api::schema::EventData::PaneAgentStatusChanged { agent_status, .. } => {
                    Some(agent_status)
                }
                _ => None,
            })
            .collect();
        assert!(
            statuses
                .iter()
                .any(|status| *status != AgentStatus::Suspended),
            "subscribers must learn the pane left suspended: {statuses:?}"
        );
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        assert_ne!(
            agent_status(&mut app, &public_pane_id),
            AgentStatus::Suspended
        );
    }

    #[test]
    fn activation_by_name_is_ambiguous_across_suspended_panes() {
        let mut app = test_app();
        app.state.workspaces[0].test_add_tab(Some("other"));
        app.state.ensure_test_terminals();
        let terminal_ids: Vec<_> = app.state.terminals.keys().cloned().collect();
        assert!(terminal_ids.len() >= 2, "two panes host suspended records");
        for terminal_id in &terminal_ids {
            app.state
                .terminals
                .get_mut(terminal_id)
                .unwrap()
                .restore_suspended_agent(
                    "claude".into(),
                    Some("reviewer".into()),
                    claude_session("claude-session"),
                );
        }

        let response = activate(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_target_ambiguous",
            "{response}"
        );
        assert!(app
            .state
            .terminals
            .values()
            .all(|terminal| terminal.suspended_agent.is_some()));
    }

    fn resume_command() -> String {
        let plan = crate::agent_resume::plan(
            "herdr:claude",
            "claude",
            &crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        )
        .unwrap();
        crate::platform::interactive_shell_command(&plan.argv, "sh").unwrap()
    }

    #[tokio::test]
    async fn restart_exits_then_relaunches_with_the_resume_command_without_a_second_request() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let pane_id = app
            .pane_info(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .pane_id;

        let response = restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        assert_eq!(response["result"]["pane_id"], pane_id);
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("the restart parks the agent first");
        assert!(record.resume_pending().is_some());
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));

        // Before detection reports the exit the tick relaunches nothing.
        assert!(!app.start_pending_agent_restarts(Instant::now()));
        assert!(rx.try_recv().is_err(), "nothing was launched");
        assert!(app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .is_some());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);

        // The observed exit plus the available shell prompt (the test runtime
        // has no child, so the prompt counts as available) are enough.
        observe_exit(&mut app);
        assert!(app.state.next_suspended_agent_resume_deadline().is_some());
        assert!(app.start_pending_agent_restarts(Instant::now()));

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_none());
        assert_eq!(terminal.agent_name.as_deref(), Some("reviewer"));
        assert!(terminal.managed_agent_launch_pending());
        assert_eq!(
            terminal.persisted_agent_session,
            Some(claude_session("claude-session"))
        );
        let sent = String::from_utf8(next_input(&mut rx).await.to_vec()).unwrap();
        let expected = resume_command();
        assert!(
            sent.starts_with(&expected),
            "sent {sent:?}, expected {expected:?}"
        );
        assert!(sent.ends_with('\r'));
        assert_ne!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        // Fork (restart screen): only the shell probe and the detection
        // timeout are left.
        let resuming = app.state.terminals[&terminal_id]
            .agent_resuming
            .expect("waits for detection");
        assert!(resuming.probe_at.is_some_and(|at| at < resuming.detect_by));
        assert_eq!(
            app.state.next_suspended_agent_resume_deadline(),
            resuming.probe_at
        );
        assert!(!app.start_pending_agent_restarts(Instant::now()));
    }

    #[tokio::test]
    async fn restart_relaunch_waits_while_the_user_types_in_the_shell() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let response = restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));
        observe_exit(&mut app);

        // The user typed into the shell: the resume command waits.
        let typed_at = Instant::now();
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .note_user_input(typed_at);
        assert!(!app.start_pending_agent_restarts(typed_at));
        assert!(rx.try_recv().is_err(), "nothing was typed into the shell");
        assert!(app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .is_some());

        // Once the user is quiet it relaunches.
        let quiet = typed_at + crate::app::typing_guard::USER_INPUT_QUIET;
        assert!(app.start_pending_agent_restarts(quiet));
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        let sent = String::from_utf8(next_input(&mut rx).await.to_vec()).unwrap();
        assert!(sent.starts_with(&resume_command()), "sent {sent:?}");
    }

    #[tokio::test]
    async fn suspend_refuses_a_working_agent_without_writing() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "reviewer");
        assert_eq!(response["error"]["code"], "agent_working", "{response}");
        assert_eq!(
            response["error"]["message"],
            "agent reviewer is working; wait for idle or blocked-free state"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Working);
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a refused suspend wrote or scheduled terminal input"
        );

        // Blocked still wins over working in the check order.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        assert_eq!(
            suspend(&mut app, "reviewer")["error"]["code"],
            "agent_blocked"
        );

        // Once idle again the same agent suspends normally.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
    }

    #[tokio::test]
    async fn suspend_and_restart_refuse_while_subagents_run_without_writing() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.replace_subagents(["bg-1", "bg-2"]);
        }
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_subagents_running",
            "{response}"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has 2 subagents running"));
        let response = restart(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_subagents_running",
            "restart inherits the suspend guard: {response}"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a refused suspend or restart wrote or scheduled terminal input"
        );

        // Once the last one reports back, the agent suspends normally.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .replace_subagents([]);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
    }

    #[tokio::test]
    async fn suspend_and_restart_refuse_while_background_tasks_run_without_writing() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        {
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal.replace_subagents([]);
            terminal.replace_background_tasks(["shell-1", "cron:loop-1"]);
        }
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = suspend(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "background_tasks_running",
            "{response}"
        );
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has 2 background tasks running"));
        let response = restart(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "background_tasks_running",
            "{response}"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a refused suspend or restart wrote or scheduled terminal input"
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .replace_background_tasks([]);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
    }

    #[tokio::test]
    async fn restart_refuses_a_working_agent_without_writing() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = restart(&mut app, "reviewer");
        assert_eq!(response["error"]["code"], "agent_working", "{response}");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is working; wait for idle"));
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Working);
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a refused restart wrote or scheduled terminal input"
        );
    }

    #[tokio::test]
    async fn restart_refuses_blocked_and_suspended_agents() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        let response = restart(&mut app, "reviewer");
        assert_eq!(response["error"]["code"], "agent_blocked", "{response}");
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "a blocked restart wrote or scheduled terminal input"
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        observe_exit(&mut app);
        for target in [
            "reviewer".to_string(),
            app.public_pane_id(0, app.state.workspaces[0].tabs[0].root_pane)
                .unwrap(),
        ] {
            let response = restart(&mut app, &target);
            assert_eq!(response["error"]["code"], "agent_suspended", "{response}");
        }
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .unwrap();
        assert!(
            record.resume_pending().is_none(),
            "a refused restart never arms the relaunch of a manual suspend"
        );
    }

    #[tokio::test]
    async fn restart_gives_up_and_stays_suspended_when_the_relaunch_never_becomes_possible() {
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        // The exit is never observed: the restart times out.
        let give_up_at = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .unwrap()
            .give_up_at;
        assert!(!app.start_pending_agent_restarts(give_up_at - Duration::from_millis(1)));
        assert!(app.start_pending_agent_restarts(give_up_at));
        let terminal = &app.state.terminals[&terminal_id];
        let record = terminal.suspended_agent.as_ref().expect("still parked");
        assert!(record.resume_pending().is_none());
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);

        // An exit that needed SIGKILL abandons the relaunch at once.
        let mut app = test_app();
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        let now = Instant::now() + SUSPEND_GRACEFUL_EXIT_GRACE;
        let probe = |_: &_, _: &_, _| SuspendProbe::Job {
            agent_pids: vec![4242],
        };
        assert!(app.escalate_suspended_agent_exits_with(now, probe, |_, _| {}));
        let later = now + SUSPEND_SIGNAL_ESCALATION_GRACE;
        assert!(app.escalate_suspended_agent_exits_with(later, probe, |_, _| {}));
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(later));
        let terminal = &app.state.terminals[&terminal_id];
        let record = terminal.suspended_agent.as_ref().expect("still parked");
        assert_eq!(record.escalation(), SuspendExitEscalation::Killed);
        assert!(record.resume_pending().is_none());
        assert!(!terminal.managed_agent_launch_pending());
    }

    #[test]
    fn suspend_probe_excludes_the_pane_child_and_flags_a_pane_process_agent() {
        fn process(pid: u32, name: &str) -> crate::platform::ForegroundProcess {
            crate::platform::ForegroundProcess {
                pid,
                name: name.into(),
                argv0: None,
                argv: None,
                cmdline: None,
            }
        }
        let child_job = || {
            (
                crate::platform::ForegroundJob {
                    process_group_id: 200,
                    processes: vec![process(200, "claude"), process(201, "node")],
                },
                Some(Agent::Claude),
            )
        };

        assert_eq!(
            suspend_probe_from_job(Some(100), None, Some(Agent::Claude)),
            SuspendProbe::Failed,
            "no readable job is not a verdict"
        );
        assert_eq!(
            suspend_probe_from_job(Some(100), Some(child_job()), Some(Agent::Claude)),
            SuspendProbe::Job {
                agent_pids: vec![200, 201]
            }
        );
        assert_eq!(
            suspend_probe_from_job(Some(200), Some(child_job()), Some(Agent::Claude)),
            SuspendProbe::Job {
                agent_pids: vec![201]
            },
            "the pane's own child is never signalled"
        );
        assert_eq!(
            suspend_probe_from_job(Some(100), Some(child_job()), Some(Agent::Codex)),
            SuspendProbe::Job { agent_pids: vec![] },
            "a job that is not the parked agent holds no agent pids"
        );
        assert_eq!(
            suspend_probe_from_job(Some(100), Some(child_job()), None),
            SuspendProbe::Job { agent_pids: vec![] }
        );

        assert!(agent_is_pane_process(
            Some(200),
            Some(&child_job()),
            Agent::Claude
        ));
        assert!(!agent_is_pane_process(
            Some(100),
            Some(&child_job()),
            Agent::Claude
        ));
        assert!(!agent_is_pane_process(
            Some(200),
            Some(&child_job()),
            Agent::Codex
        ));
        assert!(!agent_is_pane_process(
            None,
            Some(&child_job()),
            Agent::Claude
        ));
        assert!(!agent_is_pane_process(Some(200), None, Agent::Claude));
    }

    #[test]
    fn escalation_signals_terminate_then_kill_with_the_probed_pids() {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        let now = Instant::now();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .begin_agent_suspend(claude_session("claude-session"), now);
        let mut signals: Vec<(Vec<u32>, crate::platform::Signal)> = Vec::new();
        let mut probed = Vec::new();

        let changed = app.escalate_suspended_agent_exits_with(
            now,
            |_, terminal_id, expected| {
                probed.push((terminal_id.clone(), expected));
                SuspendProbe::Job {
                    agent_pids: vec![4242, 4243],
                }
            },
            |pids, signal| signals.push((pids.to_vec(), signal)),
        );
        assert!(changed);
        assert_eq!(probed, vec![(terminal_id.clone(), Some(Agent::Claude))]);
        assert_eq!(
            signals,
            vec![(vec![4242, 4243], crate::platform::Signal::Terminate)]
        );
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .unwrap();
        assert_eq!(record.escalation(), SuspendExitEscalation::Terminated);
        assert_eq!(
            record.exit_deadline(),
            Some(now + SUSPEND_SIGNAL_ESCALATION_GRACE)
        );

        // Not due yet: no probe, no signal.
        assert!(!app.escalate_suspended_agent_exits_with(
            now + Duration::from_millis(10),
            |_, _, _| panic!("no probe before the deadline"),
            |_, _| panic!("no signal before the deadline"),
        ));

        let later = now + SUSPEND_SIGNAL_ESCALATION_GRACE;
        assert!(app.escalate_suspended_agent_exits_with(
            later,
            |_, _, _| SuspendProbe::Job {
                agent_pids: vec![4242],
            },
            |pids, signal| signals.push((pids.to_vec(), signal)),
        ));
        assert_eq!(
            signals.last(),
            Some(&(vec![4242], crate::platform::Signal::Kill))
        );
        let terminal = &app.state.terminals[&terminal_id];
        let record = terminal.suspended_agent.as_ref().unwrap();
        assert_eq!(record.escalation(), SuspendExitEscalation::Killed);
        assert!(terminal.suspended_agent_exit_deadline().is_none());
        assert!(
            !record.exit_observed(),
            "only detection marks the exit seen"
        );
        assert_eq!(app.state.next_suspended_agent_exit_deadline(), None);
        assert!(!app.escalate_suspended_agent_exits_with(
            later + Duration::from_secs(10),
            |_, _, _| panic!("the wait is over"),
            |_, _| panic!("the wait is over"),
        ));
    }

    #[test]
    fn escalation_ends_the_wait_when_a_read_job_has_no_agent_process() {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        let now = Instant::now();
        let overdue = now - Duration::from_secs(1);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .begin_agent_suspend(claude_session("claude-session"), overdue);
        assert_eq!(
            app.state.next_suspended_agent_exit_deadline(),
            Some(overdue)
        );

        assert!(app.escalate_suspended_agent_exits_with(
            now,
            |_, _, _| SuspendProbe::Job { agent_pids: vec![] },
            |_, _| panic!("nothing to signal"),
        ));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(
            terminal.suspended_agent.is_some(),
            "the parked session is kept"
        );
        assert!(terminal.suspended_agent_exit_deadline().is_none());
        assert_eq!(
            terminal.suspended_agent.as_ref().unwrap().escalation(),
            SuspendExitEscalation::Pending,
            "nothing was signaled because no agent process was found"
        );
        assert!(!app.escalate_suspended_agent_exits(now + Duration::from_secs(10)));
        assert_eq!(app.state.next_suspended_agent_exit_deadline(), None);
    }

    #[test]
    fn escalation_keeps_waiting_while_the_probe_fails_then_gives_up() {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        let mut now = Instant::now();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .begin_agent_suspend(claude_session("claude-session"), now);

        // A pane with no runtime: the real probe cannot read a job.
        for retries in 1..=crate::terminal::state::SUSPEND_PROBE_RETRY_LIMIT {
            assert!(app.escalate_suspended_agent_exits(now));
            let record = app.state.terminals[&terminal_id]
                .suspended_agent
                .as_ref()
                .unwrap();
            assert_eq!(record.probe_retries(), retries);
            assert_eq!(record.escalation(), SuspendExitEscalation::Pending);
            assert_eq!(
                app.state.next_suspended_agent_exit_deadline(),
                Some(now + SUSPEND_SIGNAL_ESCALATION_GRACE),
                "the deadline is pushed forward instead of cleared"
            );
            now += SUSPEND_SIGNAL_ESCALATION_GRACE;
        }
        assert!(app.escalate_suspended_agent_exits(now));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_some());
        assert!(
            terminal.suspended_agent_exit_deadline().is_none(),
            "after the cap the wait is given up"
        );
        assert!(!terminal.suspended_agent.as_ref().unwrap().exit_observed());
        let pane_id = app
            .public_pane_id(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap();
        assert_eq!(agent_status(&mut app, &pane_id), AgentStatus::Suspended);
    }

    #[test]
    fn suspended_ranks_below_every_running_status() {
        use crate::app::api_helpers::{agent_status_priority, terminal_agent_status};
        let ordered = [
            AgentStatus::Suspended,
            AgentStatus::Unknown,
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Done,
            AgentStatus::Blocked,
        ];
        for pair in ordered.windows(2) {
            assert!(
                agent_status_priority(pair[0]) < agent_status_priority(pair[1]),
                "{:?} should rank below {:?}",
                pair[0],
                pair[1]
            );
        }

        let mut terminal = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            "/tmp".into(),
        );
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert_eq!(terminal_agent_status(&terminal, true), AgentStatus::Working);
        terminal.begin_agent_suspend(claude_session("claude-session"), Instant::now());
        assert_eq!(
            terminal_agent_status(&terminal, true),
            AgentStatus::Suspended
        );
        assert_eq!(
            terminal_agent_status(&terminal, false),
            AgentStatus::Suspended
        );
    }

    /// Fork (agent wrap): the relaunch as typed while `[agents] wrap` is on.
    #[cfg(unix)]
    fn wrapped_resume_command() -> String {
        let herdr = crate::platform::launch_executable().unwrap();
        let argv: Vec<String> = vec![
            herdr.display().to_string(),
            "agent".into(),
            "wrap".into(),
            "claude".into(),
            "--".into(),
            "--resume".into(),
            "claude-session".into(),
        ];
        crate::platform::interactive_shell_command(&argv, "sh").unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activate_and_restart_relaunch_through_the_agent_wrap_while_it_is_on() {
        let mut app = test_app();
        app.agents_config.wrap = Some(true);
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(&app);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        // activate after a suspend
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));
        observe_exit(&mut app);
        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");
        let sent = String::from_utf8(next_input(&mut rx).await.to_vec()).unwrap();
        let expected = wrapped_resume_command();
        assert!(
            sent.starts_with(&expected),
            "sent {sent:?}, expected {expected:?}"
        );
        assert_ne!(sent, resume_command());
        assert_eq!(
            app.state.terminals[&terminal_id].persisted_agent_session,
            Some(claude_session("claude-session")),
            "the session id is kept for the next resume"
        );
        while rx.try_recv().is_ok() {}

        // restart: exit, then the same wrapped relaunch
        host_live_agent(
            &mut app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.clear_agent_name();
        terminal.set_agent_name("reviewer".into());
        let response = restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        assert_eq!(next_input(&mut rx).await, bytes::Bytes::from_static(b"\r"));
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(Instant::now()));
        let sent = String::from_utf8(next_input(&mut rx).await.to_vec()).unwrap();
        assert!(
            sent.starts_with(&expected),
            "sent {sent:?}, expected {expected:?}"
        );
    }

    /// A parked Claude session activated with no shell to type into: the
    /// deferred resume takes the record's session.
    fn activate_parked(session: crate::agent_resume::PersistedAgentSession) -> App {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_suspended_agent("claude".into(), Some("reviewer".into()), session);
        app.state.default_shell = "__herdr_missing_activate_shell__".into();
        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");
        app
    }

    #[tokio::test]
    async fn activating_a_session_continued_elsewhere_resumes_the_newest_one() {
        const OLD: &str = "62766cda-e2c3-4fc3-bb20-0a9094158ef2";
        const NEW: &str = "8ac93ea8-c090-43aa-a1e4-58a2abbee34c";
        let project = crate::agent_session_chain::test_support::Project::new("activate-fork");
        project.session_file(OLD, Some(NEW));
        project.session_file(NEW, None);
        let app = activate_parked(project.session(OLD));
        let terminal = &app.state.terminals[&root_terminal_id(&app)];
        assert_eq!(
            terminal.persisted_agent_session,
            Some(project.session(NEW)),
            "the pane now records the newest session, saved like any activation"
        );
    }

    #[tokio::test]
    async fn activating_a_session_without_a_chain_keeps_it() {
        const ID: &str = "11111111-1111-4111-8111-111111111111";
        let project = crate::agent_session_chain::test_support::Project::new("activate-plain");
        project.session_file(ID, None);
        let app = activate_parked(project.session(ID));
        let terminal = &app.state.terminals[&root_terminal_id(&app)];
        assert_eq!(terminal.persisted_agent_session, Some(project.session(ID)));
    }

    // ----- fork (force suspend) ---------------------------------------------

    fn force_request(app: &mut App, method: Method) -> serde_json::Value {
        app.request_from_client_shell = true;
        let response = request(app, method);
        app.request_from_client_shell = false;
        response
    }

    fn force_suspend(app: &mut App, target: &str) -> serde_json::Value {
        force_request(
            app,
            Method::AgentSuspendForce(AgentSuspendParams {
                target: target.into(),
            }),
        )
    }

    fn force_restart(app: &mut App, target: &str) -> serde_json::Value {
        force_request(
            app,
            Method::AgentRestartForce(AgentRestartParams {
                target: target.into(),
            }),
        )
    }

    /// A live, named Claude in the root pane in `state`, with its runtime.
    fn live_claude(
        app: &mut App,
        state: AgentState,
    ) -> (
        crate::terminal::TerminalId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        host_live_agent(
            app,
            Agent::Claude,
            "reviewer",
            Some(("herdr:claude", "claude-session")),
        );
        let terminal_id = root_terminal_id(app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), state);
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        (terminal_id, rx)
    }

    async fn no_input(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>, what: &str) {
        assert!(
            tokio::time::timeout(
                super::super::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(100),
                rx.recv()
            )
            .await
            .is_err(),
            "{what}"
        );
    }

    /// Run the exit escalation at `now` with a probe that still sees the
    /// agent; returns the signals sent.
    fn escalate(app: &mut App, now: Instant) -> Vec<crate::platform::Signal> {
        let mut sent = Vec::new();
        app.escalate_suspended_agent_exits_with(
            now,
            |_, _, _| SuspendProbe::Job {
                agent_pids: vec![4242],
            },
            |_, signal| sent.push(signal),
        );
        sent
    }

    /// The graceful grace from `typed_at`, then `SIGTERM`, then `SIGKILL`.
    fn escalation_reaches_kill(
        app: &mut App,
        terminal_id: &crate::terminal::TerminalId,
        typed_at: Instant,
    ) {
        let term_at = app.state.terminals[terminal_id]
            .suspended_agent_exit_deadline()
            .expect("the exit wait runs");
        assert!(term_at >= typed_at + SUSPEND_GRACEFUL_EXIT_GRACE);
        assert!(escalate(app, term_at - Duration::from_millis(1)).is_empty());
        assert_eq!(
            escalate(app, term_at),
            vec![crate::platform::Signal::Terminate]
        );
        assert_eq!(
            escalate(app, term_at + SUSPEND_SIGNAL_ESCALATION_GRACE),
            vec![crate::platform::Signal::Kill]
        );
    }

    #[tokio::test]
    async fn force_suspend_interrupts_a_working_agent_then_exits_it_at_its_prompt() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Working);
        let asked = Instant::now();

        let response = force_suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        assert_eq!(&next_input(&mut rx).await[..], b"\x1b", "the Esc first");
        no_input(&mut rx, "the exit input waits for the prompt").await;
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("parked at once");
        assert!(record.forced_exit().is_some());
        assert_eq!(record.suspended_by, None, "suspended by the user");
        let settled = record.exit_deadline().expect("the first look");
        assert!(settled >= asked + FORCE_INTERRUPT_SETTLE);
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);

        // Still working after the settle: nothing typed, nothing signalled.
        assert!(escalate(&mut app, settled).is_empty());
        no_input(&mut rx, "never typed into a working agent").await;

        // Back at its prompt (idle, a free input box): the exit input, then
        // the usual escalation.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(super::super::typing_guard::TEST_FREE_INPUT_BOX);
        let idle_at = settled + RESTART_RESUME_POLL_INTERVAL;
        assert!(escalate(&mut app, idle_at).is_empty());
        assert_eq!(
            next_input(&mut rx).await,
            bytes::Bytes::from_static(b"/exit")
        );
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .unwrap();
        assert!(record.forced_exit().is_none());
        escalation_reaches_kill(&mut app, &terminal_id, idle_at);
    }

    /// Force a working Claude, then leave it in `state` with `screen` until
    /// the wait runs out: never typed into, terminated at `send_by`.
    async fn forced_exit_never_types_unless_back_at_a_free_prompt(
        state: AgentState,
        screen: &[u8],
    ) {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Working);
        let response = force_suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        assert_eq!(&next_input(&mut rx).await[..], b"\x1b");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), state);
        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(screen);
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .unwrap();
        let (first, send_by) = (
            record.exit_deadline().unwrap(),
            record.forced_exit().unwrap().send_by,
        );
        let mut at = first;
        while at < send_by {
            assert!(escalate(&mut app, at).is_empty(), "{state:?}");
            at += RESTART_RESUME_POLL_INTERVAL;
        }
        assert_eq!(
            escalate(&mut app, send_by),
            vec![crate::platform::Signal::Terminate]
        );
        no_input(&mut rx, "never typed").await;
    }

    #[tokio::test]
    async fn force_suspend_never_types_over_a_draft_in_the_composer() {
        let screen = "\r\n─────\r\n❯ my unsent draft\r\n─────\r\n";
        assert_eq!(
            super::super::typing_guard::composer(Agent::Claude, screen),
            super::super::typing_guard::Composer::Draft
        );
        forced_exit_never_types_unless_back_at_a_free_prompt(AgentState::Idle, screen.as_bytes())
            .await;
    }

    #[tokio::test]
    async fn force_suspend_never_types_into_an_unknown_state() {
        forced_exit_never_types_unless_back_at_a_free_prompt(
            AgentState::Unknown,
            super::super::typing_guard::TEST_FREE_INPUT_BOX,
        )
        .await;
    }

    #[tokio::test]
    async fn force_suspend_dismisses_a_blocked_dialog_and_never_types_into_one() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Blocked);
        let asked = Instant::now();

        let response = force_suspend(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
        assert_eq!(&next_input(&mut rx).await[..], b"\x1b");

        // The dialog stayed up the whole wait: terminated without the input.
        let send_by = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.forced_exit())
            .unwrap()
            .send_by;
        assert!(send_by <= asked + FORCE_INTERRUPT_WAIT + Duration::from_secs(1));
        assert!(escalate(&mut app, send_by - Duration::from_millis(1)).is_empty());
        assert_eq!(
            escalate(&mut app, send_by),
            vec![crate::platform::Signal::Terminate]
        );
        no_input(&mut rx, "nothing typed into the dialog").await;
        assert_eq!(
            escalate(&mut app, send_by + SUSPEND_SIGNAL_ESCALATION_GRACE),
            vec![crate::platform::Signal::Kill]
        );
    }

    #[tokio::test]
    async fn force_suspend_exits_an_idle_agent_with_subagents_or_background_tasks() {
        for tasks in [false, true] {
            let mut app = test_app();
            let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
            {
                let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
                if tasks {
                    terminal.replace_subagents([]);
                    terminal.replace_background_tasks(["shell-1"]);
                } else {
                    terminal.replace_subagents(["bg-1"]);
                }
            }
            let refused = suspend(&mut app, "reviewer");
            assert_eq!(
                refused["error"]["code"],
                if tasks {
                    "background_tasks_running"
                } else {
                    "agent_subagents_running"
                },
                "the plain suspend still refuses: {refused}"
            );
            let asked = Instant::now();
            let response = force_suspend(&mut app, "reviewer");
            assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
            assert_eq!(
                next_input(&mut rx).await,
                bytes::Bytes::from_static(b"/exit"),
                "an idle agent needs no Esc"
            );
            assert!(app.state.terminals[&terminal_id]
                .suspended_agent
                .as_ref()
                .unwrap()
                .forced_exit()
                .is_none());
            escalation_reaches_kill(&mut app, &terminal_id, asked);
        }
    }

    #[tokio::test]
    async fn force_restart_interrupts_then_relaunches_as_a_restart() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Blocked);
        assert_eq!(
            restart(&mut app, "reviewer")["error"]["code"],
            "agent_blocked"
        );
        let response = force_restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        assert_eq!(&next_input(&mut rx).await[..], b"\x1b");
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("parked");
        assert!(record.resume_pending().is_some(), "owes the relaunch");
        assert!(record.forced_exit().is_some());
        assert_eq!(record.suspended_by, None);

        // A suspended agent is activated, never force-restarted.
        assert_eq!(
            force_restart(&mut app, "reviewer")["error"]["code"],
            "agent_suspended"
        );
    }

    #[tokio::test]
    async fn a_forced_restart_relaunches_even_after_sigkill() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        let typed_at = Instant::now();
        let response = force_restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        assert_eq!(&next_input(&mut rx).await[..], b"/exit");
        assert_eq!(&next_input(&mut rx).await[..], b"\r");
        let resume = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .expect("owes the relaunch");
        assert!(resume.forced);
        escalation_reaches_kill(&mut app, &terminal_id, typed_at);
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("parked");
        assert_eq!(record.escalation(), SuspendExitEscalation::Killed);

        // Killed, but not abandoned: it waits for the observed exit.
        assert!(!app.start_pending_agent_restarts(Instant::now()));
        assert!(app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .is_some());
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(Instant::now()));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.suspended_agent.is_none(), "relaunched");
        assert!(terminal.managed_agent_launch_pending());
        let sent = String::from_utf8(next_input(&mut rx).await.to_vec()).unwrap();
        assert!(sent.starts_with(&resume_command()), "{sent:?}");
    }

    #[tokio::test]
    async fn a_killed_forced_restart_whose_exit_never_shows_stays_suspended() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        let typed_at = Instant::now();
        let response = force_restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        let _ = next_input(&mut rx).await;
        escalation_reaches_kill(&mut app, &terminal_id, typed_at);
        let give_up_at = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .expect("owes the relaunch")
            .give_up_at;
        assert!(!app.start_pending_agent_restarts(give_up_at - Duration::from_millis(1)));
        assert!(app.start_pending_agent_restarts(give_up_at));
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("still parked");
        assert!(record.resume_pending().is_none(), "given up");
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
    }

    #[tokio::test]
    async fn a_plain_restart_still_gives_up_after_sigkill() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        let typed_at = Instant::now();
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        let _ = next_input(&mut rx).await;
        assert!(
            !app.state.terminals[&terminal_id]
                .suspended_agent
                .as_ref()
                .and_then(|record| record.resume_pending())
                .expect("owes the relaunch")
                .forced
        );
        escalation_reaches_kill(&mut app, &terminal_id, typed_at);
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(Instant::now()));
        let record = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .expect("still parked");
        assert!(record.resume_pending().is_none(), "abandoned for the kill");
    }

    #[tokio::test]
    async fn force_keeps_the_refusals_it_cannot_fix() {
        // No native session reference: still not suspendable, no Esc.
        let mut app = test_app();
        host_live_agent(&mut app, Agent::Claude, "reviewer", None);
        let terminal_id = root_terminal_id(&app);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        let response = force_suspend(&mut app, "reviewer");
        assert_eq!(
            response["error"]["code"], "agent_not_suspendable",
            "{response}"
        );
        assert_eq!(
            force_restart(&mut app, "reviewer")["error"]["code"],
            "agent_not_suspendable"
        );
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        no_input(&mut rx, "a refused force wrote nothing").await;

        // Already suspended, and an unknown target.
        let mut app = test_app();
        let (_terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        let _ = next_input(&mut rx).await;
        assert_eq!(
            force_suspend(&mut app, "reviewer")["error"]["code"],
            "agent_already_suspended"
        );
        assert_eq!(
            force_suspend(&mut app, "nobody")["error"]["code"],
            "agent_not_found"
        );
    }

    #[tokio::test]
    async fn only_the_client_shell_lane_may_force() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Working);
        for method in [
            Method::AgentSuspendForce(AgentSuspendParams {
                target: "reviewer".into(),
            }),
            Method::AgentRestartForce(AgentRestartParams {
                target: "reviewer".into(),
            }),
        ] {
            let response = request(&mut app, method);
            assert_eq!(response["error"]["code"], "force_user_only", "{response}");
        }
        assert!(app.state.terminals[&terminal_id].suspended_agent.is_none());
        no_input(&mut rx, "a refused force wrote nothing").await;
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Working);
    }

    /// Fork (restart screen): the pane's resume marks as the agent times
    /// push carries them (the pane gets a stamped state time first, as any
    /// agent that ever changed state has).
    fn pushed_marks(app: &mut App) -> (Option<String>, Option<String>) {
        let terminal_id = root_terminal_id(app);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.last_agent_state_change_seq.get_or_insert(1);
        terminal.agent_state_since_unix_ms.get_or_insert(1);
        let pane = app.agent_times().into_iter().next().expect("listed");
        (pane.resuming, pane.resume_failed)
    }

    fn restart_mark() -> (Option<String>, Option<String>) {
        (Some("restart".into()), None)
    }

    #[tokio::test]
    async fn a_restart_is_pushed_as_restarting_until_the_relaunch() {
        let mut app = test_app();
        let (_terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(pushed_marks(&mut app), (None, None));
        let rev = app.state.agent_times_view_rev;
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        assert!(app.state.agent_times_view_rev > rev, "clients hear of it");
        assert_eq!(pushed_marks(&mut app), restart_mark());
        assert_eq!(&next_input(&mut rx).await[..], b"/exit");
        // Through the exit: still restarting.
        observe_exit(&mut app);
        assert_eq!(pushed_marks(&mut app), restart_mark());
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(Instant::now()));
        assert!(app.state.agent_times_view_rev > rev);
        assert_ne!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);
        // Relaunched but not detected yet: still restarting.
        assert_eq!(pushed_marks(&mut app), restart_mark(), "until detected");
        let rev = app.state.agent_times_view_rev;
        report_agent_back(&mut app);
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(pushed_marks(&mut app), (None, None), "detected");
    }

    /// Detection reports the relaunched Claude.
    fn report_agent_back(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited: false,
            observed_at: Instant::now() + Duration::from_millis(50),
        });
    }

    /// Suspend then activate: the parked agent is relaunched as a wake.
    fn woken_claude(app: &mut App) -> crate::terminal::TerminalId {
        let (terminal_id, _rx) = live_claude(app, AgentState::Idle);
        assert_eq!(
            suspend(app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        observe_exit(app);
        assert_eq!(
            activate(app, "reviewer")["result"]["type"],
            "agent_activated"
        );
        assert_eq!(pushed_marks(app), (Some("wake".into()), None));
        terminal_id
    }

    fn resuming(
        app: &App,
        terminal_id: &crate::terminal::TerminalId,
    ) -> crate::terminal::state::AgentResuming {
        app.state.terminals[terminal_id]
            .agent_resuming
            .expect("waits for detection")
    }

    #[tokio::test]
    async fn a_relaunch_that_never_starts_fails_at_its_probe_with_a_toast() {
        let mut app = test_app();
        let terminal_id = woken_claude(&mut app);
        let resuming = resuming(&app, &terminal_id);
        let probe_at = resuming.probe_at.expect("typed into the shell: probed");
        assert!(
            probe_at + Duration::from_secs(20) < resuming.detect_by,
            "well before the timeout"
        );
        assert_eq!(
            app.state.next_suspended_agent_resume_deadline(),
            Some(probe_at),
            "the scheduler wakes for the probe"
        );
        assert!(!app.start_pending_agent_restarts(probe_at - Duration::from_millis(1)));
        assert!(app.relaunch_failure_notices.is_empty(), "settling");

        // The test runtime counts as an available shell, and nothing is
        // detected: the relaunch never started.
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(probe_at));
        assert!(app.state.agent_times_view_rev > rev, "clients hear of it");
        assert_eq!(pushed_marks(&mut app), (None, None), "the card unlocks");
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.agent_resuming.is_none());
        assert_eq!(terminal.agent_resume_failed, None, "no failed mark");
        assert_eq!(app.state.next_suspended_agent_resume_deadline(), None);
        assert_eq!(app.relaunch_failure_notices.len(), 1, "one toast");
        let notice = &app.relaunch_failure_notices[0];
        assert_eq!(notice.title, "reviewer: couldn't wake — the shell is back");
        let root = app.state.workspaces[0].tabs[0].root_pane;
        assert_eq!(notice.pane_id, app.public_pane_id(0, root));
        assert_eq!(notice.tab_id, app.public_tab_id(0, 0));
        assert_eq!(notice.workspace_id, app.public_workspace_id(0));

        // Later passes raise nothing more.
        assert!(!app.start_pending_agent_restarts(probe_at + Duration::from_secs(60)));
        assert_eq!(app.relaunch_failure_notices.len(), 1);
    }

    #[tokio::test]
    async fn a_restart_that_never_starts_toasts_as_a_restart() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        assert_eq!(&next_input(&mut rx).await[..], b"/exit");
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(Instant::now()));
        assert_eq!(pushed_marks(&mut app), restart_mark());
        let probe_at = resuming(&app, &terminal_id).probe_at.expect("probed");
        assert!(app.start_pending_agent_restarts(probe_at));
        assert_eq!(pushed_marks(&mut app), (None, None));
        assert_eq!(app.state.terminals[&terminal_id].agent_resume_failed, None);
        let titles: Vec<_> = app
            .relaunch_failure_notices
            .iter()
            .map(|notice| notice.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["reviewer: restart didn't start — the shell is back"]
        );
    }

    #[tokio::test]
    async fn a_detected_relaunch_never_toasts() {
        let mut app = test_app();
        let terminal_id = woken_claude(&mut app);
        let resuming = resuming(&app, &terminal_id);
        report_agent_back(&mut app);
        assert_eq!(pushed_marks(&mut app), (None, None), "detected");
        assert!(!app.start_pending_agent_restarts(resuming.probe_at.expect("probed")));
        assert!(!app.start_pending_agent_restarts(resuming.detect_by));
        assert!(app.relaunch_failure_notices.is_empty());
    }

    #[tokio::test]
    async fn a_relaunch_still_starting_is_probed_again() {
        let mut app = test_app();
        let terminal_id = woken_claude(&mut app);
        let probe_at = resuming(&app, &terminal_id).probe_at.expect("probed");
        // No shell to find (here: no runtime): the probe waits a poll.
        let runtime = app.terminal_runtimes.remove(&terminal_id).expect("runtime");
        assert!(!app.start_pending_agent_restarts(probe_at));
        let next = probe_at + RESTART_RESUME_POLL_INTERVAL;
        assert_eq!(resuming(&app, &terminal_id).probe_at, Some(next));
        assert_eq!(app.state.next_suspended_agent_resume_deadline(), Some(next));
        assert_eq!(pushed_marks(&mut app), (Some("wake".into()), None));
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert!(app.start_pending_agent_restarts(next));
        assert_eq!(app.relaunch_failure_notices.len(), 1);
    }

    #[tokio::test]
    async fn a_muted_tab_gets_no_relaunch_toast() {
        let mut app = test_app();
        let terminal_id = woken_claude(&mut app);
        app.state.workspaces[0].tabs[0].muted = true;
        app.state.tab_mutes_view_rev = 1;
        let probe_at = resuming(&app, &terminal_id).probe_at.expect("probed");
        assert!(app.start_pending_agent_restarts(probe_at));
        assert_eq!(pushed_marks(&mut app), (None, None), "still unlocks");
        assert!(app.relaunch_failure_notices.is_empty(), "muted");
    }

    #[tokio::test]
    async fn a_relaunch_never_detected_times_out_without_a_failed_mark() {
        let mut app = test_app();
        let terminal_id = woken_claude(&mut app);
        let detect_by = resuming(&app, &terminal_id).detect_by;
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(detect_by));
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(pushed_marks(&mut app), (None, None));
        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.agent_resuming.is_none());
        assert_eq!(terminal.agent_resume_failed, None, "nobody renders it");
        assert_eq!(app.state.next_suspended_agent_resume_deadline(), None);
        assert!(
            app.relaunch_failure_notices.is_empty(),
            "the timeout only logs"
        );
    }

    #[tokio::test]
    async fn a_suspend_is_never_pushed_as_restarting() {
        let mut app = test_app();
        let (_terminal_id, _rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert_eq!(pushed_marks(&mut app), (None, None));
    }

    #[tokio::test]
    async fn a_forced_restart_stays_restarting_through_sigkill_to_the_relaunch() {
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        let typed_at = Instant::now();
        let response = force_restart(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_restarted", "{response}");
        let _ = next_input(&mut rx).await;
        assert_eq!(pushed_marks(&mut app), restart_mark());
        escalation_reaches_kill(&mut app, &terminal_id, typed_at);
        assert!(!app.start_pending_agent_restarts(Instant::now()));
        assert_eq!(
            pushed_marks(&mut app),
            restart_mark(),
            "killed, not given up"
        );
        observe_exit(&mut app);
        assert!(app.start_pending_agent_restarts(Instant::now()));
        assert_eq!(
            pushed_marks(&mut app),
            restart_mark(),
            "relaunched, not detected"
        );
        report_agent_back(&mut app);
        assert_eq!(pushed_marks(&mut app), (None, None), "detected");
    }

    #[tokio::test]
    async fn a_restart_that_gives_up_is_pushed_as_failed_for_a_while() {
        // A plain restart that needed SIGKILL.
        let mut app = test_app();
        let (terminal_id, mut rx) = live_claude(&mut app, AgentState::Idle);
        let typed_at = Instant::now();
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        let _ = next_input(&mut rx).await;
        escalation_reaches_kill(&mut app, &terminal_id, typed_at);
        observe_exit(&mut app);
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(Instant::now()));
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(
            pushed_marks(&mut app),
            (None, Some("restart".into())),
            "abandoned for the kill"
        );
        assert_eq!(agent_status(&mut app, "reviewer"), AgentStatus::Suspended);

        // The mark lapses on the scheduler: its deadline is due then.
        let lapse = app
            .state
            .next_suspended_agent_resume_deadline()
            .expect("the mark's lapse is scheduled");
        assert!(lapse >= Instant::now() + RESUME_FAILED_MARK_TTL - Duration::from_secs(5));
        assert!(!app.start_pending_agent_restarts(lapse - Duration::from_millis(1)));
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(lapse));
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(app.state.next_suspended_agent_resume_deadline(), None);
        assert_eq!(
            app.state.terminals[&terminal_id].resume_failed_mark(Instant::now()),
            None
        );

        // A restart that timed out; activating clears the mark.
        let mut app = test_app();
        let (terminal_id, _rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(
            restart(&mut app, "reviewer")["result"]["type"],
            "agent_restarted"
        );
        let give_up_at = app.state.terminals[&terminal_id]
            .suspended_agent
            .as_ref()
            .and_then(|record| record.resume_pending())
            .unwrap()
            .give_up_at;
        assert!(app.start_pending_agent_restarts(give_up_at));
        assert_eq!(pushed_marks(&mut app), (None, Some("restart".into())));
        observe_exit(&mut app);
        let rev = app.state.agent_times_view_rev;
        assert_eq!(
            activate(&mut app, "reviewer")["result"]["type"],
            "agent_activated"
        );
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(
            pushed_marks(&mut app),
            (Some("wake".into()), None),
            "the failed mark goes; the activation waits for detection"
        );
        report_agent_back(&mut app);
        assert_eq!(pushed_marks(&mut app), (None, None));
    }

    #[tokio::test]
    async fn activating_a_cold_restored_pane_pushes_the_wake_mark_without_a_time() {
        let mut app = test_app();
        let terminal_id = root_terminal_id(&app);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.restore_suspended_agent(
            "claude".into(),
            Some("reviewer".into()),
            claude_session("claude-session"),
        );
        assert_eq!(terminal.last_agent_state_change_seq, None, "no stamps");
        assert_eq!(terminal.agent_state_since_unix_ms, None);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert!(app.agent_times().is_empty(), "nothing to list yet");

        let before = crate::codex_sessions::now_unix_ms();
        let rev = app.state.agent_times_view_rev;
        let response = activate(&mut app, "reviewer");
        assert_eq!(response["result"]["type"], "agent_activated", "{response}");
        assert!(app.state.agent_times_view_rev > rev);
        let panes = app.agent_times();
        assert_eq!(panes.len(), 1, "listed for its mark alone");
        assert_eq!(panes[0].resuming.as_deref(), Some("wake"));
        assert_eq!(panes[0].state_change_seq, 0, "never matched for a time");
        assert!(panes[0].since_unix_ms >= before);
        assert_eq!(
            app.state.terminals[&terminal_id].last_agent_state_change_seq, None,
            "the push stamps nothing"
        );
    }

    #[tokio::test]
    async fn a_respawn_pushes_the_cleared_resuming_mark() {
        let mut app = test_app();
        let (terminal_id, _rx) = live_claude(&mut app, AgentState::Idle);
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        observe_exit(&mut app);
        assert_eq!(
            activate(&mut app, "reviewer")["result"]["type"],
            "agent_activated"
        );
        assert!(app.state.terminals[&terminal_id].agent_resuming.is_some());
        let rev = app.state.agent_times_view_rev;
        app.clear_agent_identity_after_respawn(&terminal_id);
        assert!(app.state.agent_times_view_rev > rev, "clients hear of it");
        assert!(app.state.terminals[&terminal_id].agent_resuming.is_none());
        assert!(app.agent_times().iter().all(|pane| pane.resuming.is_none()));

        // Nothing marked: a respawn pushes nothing.
        let rev = app.state.agent_times_view_rev;
        app.clear_agent_identity_after_respawn(&terminal_id);
        assert_eq!(app.state.agent_times_view_rev, rev);
    }

    #[tokio::test]
    async fn a_suspend_of_a_pane_with_a_failed_mark_pushes_the_change() {
        let mut app = test_app();
        let (terminal_id, _rx) = live_claude(&mut app, AgentState::Idle);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .fail_agent_resume(
                crate::terminal::AgentResumeKind::Wake,
                Instant::now() + RESUME_FAILED_MARK_TTL,
            );
        assert_eq!(pushed_marks(&mut app), (None, Some("wake".into())));
        let rev = app.state.agent_times_view_rev;
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );
        assert!(app.state.agent_times_view_rev > rev, "clients hear of it");
        assert_eq!(pushed_marks(&mut app), (None, None));
    }
}
