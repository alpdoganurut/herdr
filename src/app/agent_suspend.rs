//! Park a live agent in its pane and relaunch it later.
//!
//! Suspending asks the agent to exit from its own prompt while the pane keeps
//! the native session reference and the agent's name; the pane reports
//! `suspended` until it is activated. Activating rebuilds the resume plan from
//! that reference and launches it in the same pane, reusing the managed launch
//! path that `agent.start` uses.

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

pub(super) enum AgentSuspendError {
    Target(TerminalTargetError),
    NotRunning(String),
    Blocked(String),
    AlreadySuspended(String),
    NotSuspendable { target: String, reason: String },
    InputFailed(String),
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
    /// Park the live agent hosted by `target`.
    ///
    /// The suspended record is stored before the exit input is sent because
    /// the process-exit path wipes the live session fields it is built from.
    pub(super) fn suspend_agent(&mut self, target: &str) -> Result<String, AgentSuspendError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentSuspendError::Target)?;
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
        if terminal.state == crate::detect::AgentState::Blocked {
            return Err(AgentSuspendError::Blocked(target.to_string()));
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
            });
        };
        if crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
            .is_none()
        {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: format!("{} has no native resume plan", session.agent),
            });
        }
        let Some(exit_input) = crate::agent_resume::graceful_exit_input(&session.agent) else {
            return Err(AgentSuspendError::NotSuspendable {
                target: target.to_string(),
                reason: format!("{} has no graceful exit command", session.agent),
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
            });
        }
        let (text, enter) = super::api_helpers::encode_api_submission_parts(runtime, exit_input);

        let now = Instant::now();
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
        // The parked session must outlive the agent's own transcript
        // retention; copy it now rather than waiting for the periodic pass.
        self.backup_agent_transcript_now(resolved.ws_idx, resolved.pane_id, session);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_agent_status_transition(resolved.ws_idx, resolved.pane_id);
        Ok(self
            .public_pane_id(resolved.ws_idx, resolved.pane_id)
            .unwrap_or_else(|| target.to_string()))
    }

    /// Relaunch the parked agent hosted by `target` in its own pane.
    pub(super) fn activate_agent(&mut self, target: &str) -> Result<String, AgentActivateError> {
        let resolved = self.resolve_suspended_agent_target(target)?;
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

        if let Some(runtime) = self.terminal_runtimes.get(&terminal_id) {
            // The shell can look available before detection publishes the
            // exit; that late report would then wipe the relaunched session.
            if !record.exit_observed() {
                return Err(AgentActivateError::ExitPending(target.to_string()));
            }
            let shell_name = available_shell_name(runtime)
                .ok_or_else(|| AgentActivateError::PaneNotAvailable(target.to_string()))?;
            let command = crate::platform::interactive_shell_command(&plan.argv, &shell_name)
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
                terminal.restore_suspended_agent(record.agent, record.name, record.session);
                return Err(AgentActivateError::InputFailed(err.to_string()));
            }
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
        self.emit_agent_status_transition(resolved.ws_idx, resolved.pane_id);
        Ok(self
            .public_pane_id(resolved.ws_idx, resolved.pane_id)
            .unwrap_or_else(|| target.to_string()))
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
        let mut changed = false;
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
            AgentSuspendError::AlreadySuspended(target) => crate::api::schema::ErrorBody {
                code: "agent_already_suspended".into(),
                message: format!("agent {target} is already suspended"),
            },
            AgentSuspendError::NotSuspendable { target, reason } => crate::api::schema::ErrorBody {
                code: "agent_not_suspendable".into(),
                message: format!("agent {target} cannot be suspended: {reason}"),
            },
            AgentSuspendError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_suspend_input_failed".into(),
                message,
            },
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
    use crate::api::schema::{AgentActivateParams, AgentStatus, AgentSuspendParams, Method};
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
            process_exited: true,
            observed_at,
        });
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
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
            Agent::Codex,
            "worker",
            Some(("herdr:codex", "codex-session")),
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
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        // Another tab is active so a completion here would normally notify.
        app.state.workspaces[0].test_add_tab(Some("other"));
        app.state.workspaces[0].active_tab = 1;
        assert_eq!(
            suspend(&mut app, "reviewer")["result"]["type"],
            "agent_suspended"
        );

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
}
