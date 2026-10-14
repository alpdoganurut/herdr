//! Agent handoff, then compact (fork): the App side of
//! [`crate::agent_handoff`] (the pure state machine, texts and file layout).
//!
//! - Triggers: the context use reaching `[agents.handoff] at_percent`
//!   (checked when [`App::apply_agent_context_results`] stores a read, every
//!   5 s at most, never per frame), `agents.handoff` (the tab menu, the
//!   `handoff_compact` key, `herdr agent handoff`, an agent for itself), and
//!   the Claude hook asset's PreCompact report for a compaction herdr did not
//!   start (`pane.report_compact`).
//! - One run per terminal, runtime only (a restart or a live handoff
//!   forgets it). The server tick ([`App::handle_agent_handoff_tasks`], about
//!   once a second while a run is on, O(runs)) steps each run; file work
//!   goes to the `herdr-agent-handoff` worker thread and comes back through
//!   its channel.
//! - Typing follows the message queue's rules
//!   ([`App::agent_handoff_target`]: live, idle, not starting, suspended,
//!   blocked or in voice mode, not the coordinator, no hook-held turn,
//!   nothing queued before the prompt) for [`SETTLE`], then `agent.prompt`'s
//!   own path with the typing guard. Queued messages and other
//!   `agent.prompt` calls wait while a run holds the pane
//!   ([`crate::agent_handoff::Run::holds_messages`]); they never keep the
//!   run from `/compact`.
//! - The automatic trigger fires once per compaction cycle; a start it
//!   could not make, or a run that stopped before `/compact`, re-arms it
//!   after [`AUTO_RETRY`].
//! - A PreCompact during a run (before `/compact`) turns the run into that
//!   compaction's, keeping its files and a handoff the agent wrote.
//! - A run belongs to one Claude session (the id stays across `/compact`):
//!   another session in the pane (`/clear`, a resume) ends it.
//! - A handoff the coordinator or another agent asks for is refused for
//!   [`HANDOFF_COOLDOWN`] after the target's last one (a loop guard).
//! - Every step is logged in `actions.jsonl` (action `handoff`); a failure
//!   shows the user an agent card from the pane.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::message_queue::SETTLE;
use super::App;
use crate::agent_handoff::files::{self, Job, Outcome};
use crate::agent_handoff::{
    self as handoff, Effect, Facts, HandoffAgent, Kind, Phase, Run, Trigger,
};
use crate::api::schema::agents_model::{
    error_code, AgentsActionOutcome, AgentsHandoffOutcome, AgentsHandoffParams, AgentsHandoffResult,
};
use crate::api::schema::{CompactEvent, Method, PaneReportCompactParams, ResponseResult};
use crate::terminal::TerminalId;

use super::agents_model::{LogLine, ModelError, ModelResult};
use super::api::responses::{encode_error, encode_success};

/// How often a pane with a run is stepped.
const STEP_EVERY: Duration = Duration::from_secs(1);
/// How often the worker's results are looked for while a job is out.
const POLL_EVERY: Duration = Duration::from_millis(200);
/// Failed writes in a row (other than the typing guard) before a run stops.
const MAX_WRITE_FAILURES: u32 = 5;
/// A saved handoff is offered to a SessionStart(compact) without a run
/// this long.
const SAVED_FRESH: Duration = Duration::from_secs(15 * 60);
/// Checkpoints copied into a mechanical handoff.
const MECHANICAL_CHECKPOINTS: u32 = 10;
/// The automatic trigger tries again this long after a start it could not
/// make or a run that stopped before `/compact` (the context stays over the
/// threshold until a compaction, so it would never re-arm on its own).
const AUTO_RETRY: Duration = Duration::from_secs(5 * 60);
/// A handoff the coordinator or another agent asks for is refused this long
/// after the terminal's last handoff or compaction (a loop guard: an agent
/// handing another off again and again). The agent itself, its user and
/// the automatic trigger are not held back.
pub(crate) const HANDOFF_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// A handoff saved for a terminal (the latest).
#[derive(Debug, Clone)]
pub(crate) struct SavedHandoff {
    /// The agent session it was saved for (a later session of the pane
    /// does not get it).
    pub(crate) session_id: String,
    pub(crate) paths: handoff::RunPaths,
    pub(crate) kind: Kind,
    pub(crate) at: Instant,
    pub(crate) at_unix: u64,
    /// The transcript copy may be there (not known to have failed).
    pub(crate) transcript: bool,
}

/// A mechanical handoff on the worker: what to record once it is written.
#[derive(Debug, Clone)]
struct PendingWrite {
    terminal_id: TerminalId,
    saved: SavedHandoff,
}

/// The runtime state (App field `agent_handoff`).
#[derive(Default)]
pub(crate) struct AgentHandoffRuntime {
    pub(crate) runs: HashMap<TerminalId, Run>,
    /// Whether the automatic trigger may fire for a terminal (absent = yes).
    pub(crate) armed: HashMap<TerminalId, bool>,
    /// Not before then: the automatic trigger's retry after a refused start
    /// or a run that stopped before `/compact`.
    pub(crate) auto_retry_at: HashMap<TerminalId, Instant>,
    /// The latest saved handoff per terminal (the sidebar's age, the
    /// SessionStart(compact) text without a run).
    pub(crate) saved: HashMap<TerminalId, SavedHandoff>,
    /// When a terminal's last run compacted or finished well: a handoff
    /// another agent asks for is refused for [`HANDOFF_COOLDOWN`] after it.
    pub(crate) last_finished: HashMap<TerminalId, Instant>,
    pending_writes: HashMap<u64, PendingWrite>,
    worker: Option<files::Worker>,
    /// Jobs whose outcome has not come back.
    in_flight: u32,
    next_id: u64,
    deadline: Option<Instant>,
    /// `<state_dir>/handoffs` (tests: a temp dir).
    pub(crate) root: Option<PathBuf>,
}

impl AgentHandoffRuntime {
    /// Whether queued messages for `terminal_id` wait for its run.
    pub(crate) fn holds_messages(&self, terminal_id: &str) -> bool {
        // O(runs): a map of a few entries at most.
        self.runs
            .iter()
            .any(|(id, run)| run.holds_messages() && id.to_string() == terminal_id)
    }

    fn root(&self) -> PathBuf {
        self.root.clone().unwrap_or_else(default_root)
    }
}

#[cfg(not(test))]
fn default_root() -> PathBuf {
    handoff::root(&crate::config::state_dir())
}

#[cfg(test)]
fn default_root() -> PathBuf {
    std::env::temp_dir().join(format!("herdr-agent-handoff-test-{}", std::process::id()))
}

/// Why a run could not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartRefusal {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl StartRefusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn now_unix() -> u64 {
    crate::notes::now_unix()
}

fn response_code(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

impl App {
    pub(crate) fn next_agent_handoff_deadline(&self) -> Option<Instant> {
        self.agent_handoff.deadline
    }

    fn wake_agent_handoff(&mut self, at: Instant) {
        self.agent_handoff.deadline = Some(
            self.agent_handoff
                .deadline
                .map_or(at, |deadline| deadline.min(at)),
        );
    }

    fn submit_handoff_job(&mut self, job: Job) -> bool {
        if self.agent_handoff.worker.is_none() {
            match files::Worker::spawn() {
                Ok(worker) => self.agent_handoff.worker = Some(worker),
                Err(err) => {
                    tracing::warn!(err = %err, "agent handoff: cannot start the worker thread");
                    return false;
                }
            }
        }
        let expects_outcome = !matches!(job, Job::Meta { .. });
        let submitted = self
            .agent_handoff
            .worker
            .as_ref()
            .is_some_and(|worker| worker.submit(job));
        if !submitted {
            tracing::warn!("agent handoff: the worker thread is gone");
            self.agent_handoff.worker = None;
            return false;
        }
        if expects_outcome {
            self.agent_handoff.in_flight += 1;
        }
        true
    }

    // ----- the tick -----------------------------------------------------------

    /// Step every run whose deadline came, apply finished file work.
    /// Returns whether anything changed. O(1) without runs and jobs.
    pub(crate) fn handle_agent_handoff_tasks(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.agent_handoff.deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        let mut changed = false;
        let outcomes = self
            .agent_handoff
            .worker
            .as_ref()
            .map(files::Worker::drain)
            .unwrap_or_default();
        for outcome in outcomes {
            self.agent_handoff.in_flight = self.agent_handoff.in_flight.saturating_sub(1);
            changed |= self.apply_handoff_outcome(outcome, now);
        }
        let terminals: Vec<TerminalId> = self.agent_handoff.runs.keys().cloned().collect();
        for terminal_id in terminals {
            changed |= self.step_agent_handoff(&terminal_id, now);
        }
        self.agent_handoff.deadline = if self.agent_handoff.in_flight > 0 {
            Some(now + POLL_EVERY)
        } else if !self.agent_handoff.runs.is_empty() {
            Some(now + STEP_EVERY)
        } else {
            None
        };
        changed
    }

    /// The pane `terminal_id` holds and whether herdr may type into its
    /// agent now ([`App::agent_handoff_target`]); `Err(None)` when it is gone.
    fn handoff_free(&self, terminal_id: &TerminalId) -> Result<String, Option<String>> {
        self.agent_handoff_target(&terminal_id.to_string())
    }

    fn step_agent_handoff(&mut self, terminal_id: &TerminalId, now: Instant) -> bool {
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            self.finish_agent_handoff(terminal_id, Err("its pane is gone".into()), now);
            return true;
        };
        let agent_ok = terminal
            .effective_known_agent()
            .and_then(HandoffAgent::from_agent)
            .is_some();
        if !agent_ok && terminal.suspended_agent.is_none() {
            self.finish_agent_handoff(terminal_id, Err("its agent is gone".into()), now);
            return true;
        }
        if terminal.suspended_agent.is_some() {
            self.finish_agent_handoff(terminal_id, Err("its agent was suspended".into()), now);
            return true;
        }
        // The run is for one Claude session: `/clear` or a resume of another
        // one in the pane ends it (Claude keeps the id across `/compact`). An
        // empty binding (briefly, around SessionStart) does not.
        let session_changed = self
            .agent_handoff
            .runs
            .get(terminal_id)
            .is_some_and(|run| session_changed(run, terminal.persistable_agent_session()));
        if session_changed {
            self.finish_agent_handoff(terminal_id, Err("the agent's session changed".into()), now);
            return true;
        }
        let hook_end = terminal.turn().hook_turn_ended();
        let idle_edge = terminal.turn().last_idle().map(|(at, _)| at);
        let blocked = terminal.state == crate::detect::AgentState::Blocked;
        let percent = terminal.agent_context.map(|usage| usage.percent());
        let free = self.handoff_free(terminal_id);
        let pane = match free {
            Err(None) => {
                self.finish_agent_handoff(terminal_id, Err("its pane is gone".into()), now);
                return true;
            }
            Err(Some(_)) => {
                if let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) {
                    run.free_since = None;
                }
                None
            }
            Ok(pane) => Some(pane),
        };
        let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) else {
            return false;
        };
        // The handoff turn stuck on a prompt (a permission, a question): tell
        // the user once instead of failing 40 minutes later.
        let blocked_notice = if blocked && matches!(run.phase, Phase::AwaitHandoff { .. }) {
            let since = *run.blocked_since.get_or_insert(now);
            let due = !run.blocked_noted && now >= since + handoff::BLOCKED_NOTICE;
            if due {
                run.blocked_noted = true;
            }
            due
        } else {
            run.blocked_since = None;
            false
        };
        if blocked_notice {
            let reason = "the handoff turn waits on a prompt in the agent (a permission or a question); answer it, or herdr writes its own handoff 10 minutes after the request";
            self.log_handoff(terminal_id, AgentsActionOutcome::Ok, reason);
            self.agent_handoff_card(terminal_id, "Handoff waits on you", reason);
            return true;
        }
        let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) else {
            return false;
        };
        let settled = pane.is_some() && {
            let since = *run.free_since.get_or_insert(now);
            now >= since + SETTLE
        };
        let facts = Facts {
            now,
            free: settled,
            hook_end,
            idle_edge,
        };
        let effect = handoff::next(run, &facts);
        let pane = pane.unwrap_or_default();
        match effect {
            Effect::Wait => false,
            Effect::TypePrompt => {
                let text = handoff::prompt(run, percent);
                if self.type_for_handoff(terminal_id, &pane, &text, now) {
                    if let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) {
                        run.enter(
                            Phase::AwaitHandoff {
                                typed_at: now,
                                checked: None,
                                checking: false,
                            },
                            now,
                        );
                    }
                    self.log_handoff(terminal_id, AgentsActionOutcome::Ok, "handoff prompt typed");
                }
                true
            }
            Effect::CheckFile { end } => {
                let paths = run.paths.clone();
                let id = run.id;
                run.phase = Phase::AwaitHandoff {
                    typed_at: match run.phase {
                        Phase::AwaitHandoff { typed_at, .. } => typed_at,
                        _ => now,
                    },
                    checked: Some(end),
                    checking: true,
                };
                run.check_since = Some(now);
                if !self.submit_handoff_job(Job::Check {
                    run: id,
                    end,
                    paths,
                }) {
                    self.finish_agent_handoff(
                        terminal_id,
                        Err("the handoff worker could not start".into()),
                        now,
                    );
                }
                true
            }
            Effect::TypeReminder => {
                let line = handoff::reminder_line(&run.paths.handoff);
                if self.type_for_handoff(terminal_id, &pane, &line, now) {
                    if let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) {
                        run.reminded = true;
                        run.missing_since = None;
                        // The same phase (its start still bounds it): the
                        // reminder's turn is the one waited for now.
                        run.phase = Phase::AwaitHandoff {
                            typed_at: now,
                            checked: None,
                            checking: false,
                        };
                        run.free_since = None;
                    }
                    self.log_handoff(
                        terminal_id,
                        AgentsActionOutcome::Ok,
                        "the turn ended without the handoff file; reminder typed",
                    );
                }
                true
            }
            Effect::WriteMechanical => {
                let reason = if run.reminded {
                    "the agent did not write one after the prompt and a reminder"
                } else {
                    "the agent did not write one within 10 minutes of the prompt"
                };
                run.enter(Phase::WritingFallback, now);
                self.log_handoff(
                    terminal_id,
                    AgentsActionOutcome::Ok,
                    "no handoff from the agent in time; herdr writes a mechanical one",
                );
                self.start_mechanical_handoff(terminal_id, reason, now);
                true
            }
            Effect::TypeCompact => {
                let command = run.agent.compact_command();
                let (id, paths) = (run.id, run.paths.clone());
                if self.type_for_handoff(terminal_id, &pane, command, now) {
                    if let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) {
                        run.enter(Phase::Compacting { typed_at: now }, now);
                    }
                    self.agent_handoff.armed.insert(terminal_id.clone(), false);
                    self.log_handoff(terminal_id, AgentsActionOutcome::Ok, "/compact typed");
                    // The copy again, now with the handoff turn in it (the
                    // compaction appends only after its summary call).
                    self.submit_transcript_copy(terminal_id, id, paths, None);
                }
                true
            }
            Effect::Compacted => {
                self.agent_handoff_compacted(terminal_id, "the screen settled idle", now);
                true
            }
            Effect::TypeResume => {
                let Some(saved) = self.agent_handoff.saved.get(terminal_id).cloned() else {
                    self.finish_agent_handoff(terminal_id, Ok("compacted".into()), now);
                    return true;
                };
                let line = handoff::resume_line(&saved.paths.handoff, &saved.paths.transcript);
                if self.type_for_handoff(terminal_id, &pane, &line, now) {
                    self.finish_agent_handoff(terminal_id, Ok("compacted and resumed".into()), now);
                }
                true
            }
            Effect::Abort(reason) => {
                self.finish_agent_handoff(terminal_id, Err(reason), now);
                true
            }
            Effect::Finish(reason) => {
                self.finish_agent_handoff(terminal_id, Ok(reason), now);
                true
            }
        }
    }

    /// Type `text` into `pane` through `agent.prompt`'s path with the typing
    /// guard. `false` when nothing was typed (the guard, a refusal); a run
    /// that keeps failing stops.
    fn type_for_handoff(
        &mut self,
        terminal_id: &TerminalId,
        pane: &str,
        text: &str,
        now: Instant,
    ) -> bool {
        let result = self.queue_agent_prompt_as(
            format!("agent-handoff:{pane}"),
            crate::api::schema::AgentPromptParams {
                target: pane.to_string(),
                text: text.to_string(),
                wait: None,
                guard_user_typing: true,
            },
            crate::agents_model::InputSource::Programmatic(crate::agents_model::Programmatic::Api),
            false,
        );
        match result {
            Ok(_) => {
                if let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) {
                    run.write_failures = 0;
                }
                true
            }
            Err(response) => {
                let code = response_code(&response);
                let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) else {
                    return false;
                };
                run.free_since = None;
                // Its user typing or a dialog over its input box holds the
                // write; neither counts as a failure.
                if code != "user_typing" && code != "input_box_absent" {
                    run.write_failures += 1;
                    tracing::info!(code = %code, phase = run.phase.name(), "agent handoff: a write was refused");
                    if run.write_failures >= MAX_WRITE_FAILURES {
                        self.finish_agent_handoff(
                            terminal_id,
                            Err(format!("herdr could not type into the agent ({code})")),
                            now,
                        );
                    }
                }
                false
            }
        }
    }

    fn apply_handoff_outcome(&mut self, outcome: Outcome, now: Instant) -> bool {
        match outcome {
            Outcome::Started { run, transcript } => {
                let Some((terminal_id, current)) = self
                    .agent_handoff
                    .runs
                    .iter_mut()
                    .find(|(_, current)| current.id == run)
                else {
                    return false;
                };
                let terminal_id = terminal_id.clone();
                // A failed second copy leaves the first in place.
                let saved_copy = transcript.is_ok() || current.transcript_saved == Some(true);
                current.transcript_saved = Some(saved_copy);
                if let Some(saved) = self.agent_handoff.saved.get_mut(&terminal_id) {
                    if saved.paths == current.paths {
                        saved.transcript = saved_copy;
                    }
                }
                match transcript {
                    Ok(bytes) => self.log_handoff(
                        &terminal_id,
                        AgentsActionOutcome::Ok,
                        &format!("transcript saved ({bytes} bytes)"),
                    ),
                    Err(err) => self.log_handoff(
                        &terminal_id,
                        AgentsActionOutcome::Failed,
                        &format!("transcript not saved: {err}"),
                    ),
                }
                true
            }
            Outcome::Checked { run, end, nonempty } => {
                let Some((terminal_id, current)) = self
                    .agent_handoff
                    .runs
                    .iter_mut()
                    .find(|(_, current)| current.id == run)
                else {
                    return false;
                };
                let Phase::AwaitHandoff {
                    typed_at, checked, ..
                } = current.phase
                else {
                    return false;
                };
                if checked != Some(end) {
                    return false;
                }
                let terminal_id = terminal_id.clone();
                current.check_since = None;
                if nonempty {
                    current.kind = Some(Kind::Agent);
                    let saved = SavedHandoff {
                        session_id: current.session_id.clone(),
                        paths: current.paths.clone(),
                        kind: Kind::Agent,
                        at: now,
                        at_unix: now_unix(),
                        transcript: current.transcript_saved != Some(false),
                    };
                    current.enter(Phase::AwaitCompact, now);
                    self.record_saved_handoff(&terminal_id, saved);
                } else {
                    current.phase = Phase::AwaitHandoff {
                        typed_at,
                        checked,
                        checking: false,
                    };
                    current.missing_since.get_or_insert(now);
                }
                true
            }
            Outcome::Written { run, result } => {
                let pending = self.agent_handoff.pending_writes.remove(&run);
                let terminal_id = pending.as_ref().map(|pending| pending.terminal_id.clone());
                match (result, pending) {
                    (Ok(kind), Some(mut pending)) => {
                        pending.saved.kind = kind;
                        self.record_saved_handoff(&pending.terminal_id, pending.saved);
                        if let Some(current) = self
                            .agent_handoff
                            .runs
                            .get_mut(&pending.terminal_id)
                            .filter(|current| current.id == run)
                        {
                            current.kind = Some(kind);
                            if current.phase == Phase::WritingFallback {
                                current.enter(Phase::AwaitCompact, now);
                            }
                        }
                    }
                    (Err(err), Some(_)) => {
                        let Some(terminal_id) = terminal_id else {
                            return false;
                        };
                        let herdr_run =
                            self.agent_handoff
                                .runs
                                .get(&terminal_id)
                                .is_some_and(|current| {
                                    current.id == run && current.phase == Phase::WritingFallback
                                });
                        let reason = format!("herdr could not write its handoff: {err}");
                        if herdr_run {
                            self.finish_agent_handoff(&terminal_id, Err(reason), now);
                        } else {
                            self.log_handoff(&terminal_id, AgentsActionOutcome::Failed, &reason);
                            self.agent_handoff_card(
                                &terminal_id,
                                "Handoff & compact failed",
                                &reason,
                            );
                        }
                    }
                    (_, None) => return false,
                }
                true
            }
        }
    }

    // ----- starting a run -------------------------------------------------------

    /// Start a run for the agent of `terminal_id`. `Ok((pane, outcome))`.
    pub(crate) fn start_agent_handoff(
        &mut self,
        terminal_id: &TerminalId,
        trigger: Trigger,
        now: Instant,
    ) -> Result<(String, AgentsHandoffOutcome), StartRefusal> {
        if !self.agents_config.handoff.enabled {
            return Err(StartRefusal::new(
                error_code::INVALID_PARAMS,
                "handoff is off ([agents.handoff] enabled = false)",
            ));
        }
        let target = self
            .resolve_terminal_target(&terminal_id.to_string())
            .map_err(|_| StartRefusal::new(error_code::NOT_FOUND, "the pane is gone"))?;
        let pane = self
            .public_pane_id(target.ws_idx, target.pane_id)
            .unwrap_or_default();
        if self.agent_handoff.runs.contains_key(terminal_id) {
            return Ok((pane, AgentsHandoffOutcome::AlreadyRunning));
        }
        if self.is_coordinator_pane(target.ws_idx, target.pane_id) {
            return Err(StartRefusal::new(
                error_code::PROTECTED_TAB,
                "the coordinator is never handed off",
            ));
        }
        let terminal = self
            .state
            .terminals
            .get(terminal_id)
            .ok_or_else(|| StartRefusal::new(error_code::NOT_FOUND, "the pane is gone"))?;
        if terminal.suspended_agent.is_some() {
            return Err(StartRefusal::new(
                "agent_suspended",
                format!("{pane} is suspended; activate it first"),
            ));
        }
        let agent = terminal
            .effective_known_agent()
            .and_then(HandoffAgent::from_agent)
            .ok_or_else(|| {
                StartRefusal::new(
                    "unsupported_agent",
                    format!(
                        "{pane} runs no Claude Code agent (handoff, then compact, is Claude only)"
                    ),
                )
            })?;
        let session = terminal
            .persistable_agent_session()
            .filter(|session| session.agent == "claude")
            .ok_or_else(|| {
                StartRefusal::new(
                    "agent_not_ready",
                    format!("{pane}'s Claude session is not known yet"),
                )
            })?;
        let settings = handoff::RunSettings {
            command: self.agents_config.handoff.command().map(str::to_string),
            resume: self.agents_config.handoff.resume,
        };
        let id = self.next_handoff_id();
        let started_unix = now_unix();
        let session_id = session.session_ref.value.clone();
        let paths = handoff::run_paths(&self.agent_handoff.root(), &session_id, started_unix);
        let run = Run {
            id,
            agent,
            trigger: trigger.clone(),
            settings,
            phase: Phase::AwaitPrompt,
            phase_since: now,
            started_unix,
            session_id: session_id.clone(),
            paths: paths.clone(),
            transcript_saved: None,
            kind: None,
            free_since: None,
            write_failures: 0,
            missing_since: None,
            reminded: false,
            blocked_since: None,
            blocked_noted: false,
            check_since: None,
        };
        let meta = self.handoff_meta(&run, &pane, None);
        let locate = Some(crate::agent_context::ContextSession {
            agent: crate::agent_context::ContextAgent::Claude,
            id: session_id,
            session: session.clone(),
        });
        let submitted = self.submit_handoff_job(Job::Start {
            run: id,
            paths,
            source: session.transcript_path.clone(),
            locate,
            home: crate::integration::home_dir().ok(),
            meta,
        });
        if !submitted {
            return Err(StartRefusal::new(
                error_code::FAILED,
                "the handoff worker could not start",
            ));
        }
        self.agent_handoff.runs.insert(terminal_id.clone(), run);
        self.wake_agent_handoff(now);
        self.log_handoff(
            terminal_id,
            AgentsActionOutcome::Ok,
            &format!("started ({})", trigger_detail(&trigger)),
        );
        Ok((pane, AgentsHandoffOutcome::Started))
    }

    fn next_handoff_id(&mut self) -> u64 {
        self.agent_handoff.next_id += 1;
        self.agent_handoff.next_id
    }

    fn handoff_meta(
        &self,
        run: &Run,
        pane: &str,
        outcome: Option<&Result<String, String>>,
    ) -> serde_json::Value {
        let mut meta = serde_json::json!({
            "pane": pane,
            "agent": "claude",
            "session_id": run.session_id,
            "trigger": run.trigger.as_str(),
            "started_unix": run.started_unix,
            "phase": run.phase.name(),
            "kind": run.kind.map(Kind::as_str),
            "transcript_saved": run.transcript_saved,
            "handoff": run.paths.handoff.display().to_string(),
            "transcript": run.paths.transcript.display().to_string(),
        });
        if let Some(outcome) = outcome {
            meta["finished_unix"] = now_unix().into();
            match outcome {
                Ok(detail) => {
                    meta["outcome"] = "ok".into();
                    meta["detail"] = detail.clone().into();
                }
                Err(reason) => {
                    meta["outcome"] = "failed".into();
                    meta["detail"] = reason.clone().into();
                }
            }
        }
        meta
    }

    /// herdr's own handoff for the run of `terminal_id`, on the worker.
    fn start_mechanical_handoff(&mut self, terminal_id: &TerminalId, reason: &str, now: Instant) {
        let Some(run) = self.agent_handoff.runs.get(terminal_id).cloned() else {
            return;
        };
        let target = self.resolve_terminal_target(&terminal_id.to_string()).ok();
        let pane = target
            .as_ref()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
            .unwrap_or_default();
        let terminal = self.state.terminals.get(terminal_id);
        let agent_name = terminal
            .and_then(|terminal| terminal.agent_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "claude".to_string());
        // The agent's own directory (it may have been started after a cd),
        // else the pane's; one process lookup per mechanical handoff.
        let cwd = self
            .terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.foreground_cwd())
            .or_else(|| terminal.map(|terminal| terminal.cwd.clone()));
        let transcript_hint = terminal
            .and_then(|terminal| terminal.persistable_agent_session())
            .and_then(|session| session.transcript_path);
        let (notes_tail, checkpoints) = if pane.is_empty() {
            (None, Vec::new())
        } else {
            self.handoff_notes(&pane)
        };
        let input = handoff::Mechanical {
            agent_name,
            pane,
            trigger: run.trigger.as_str().to_string(),
            reason: reason.to_string(),
            written_unix: now_unix(),
            cwd,
            notes_tail,
            checkpoints,
            git: Vec::new(),
            prompts: Vec::new(),
            transcript: transcript_hint,
        };
        self.agent_handoff.pending_writes.insert(
            run.id,
            PendingWrite {
                terminal_id: terminal_id.clone(),
                saved: SavedHandoff {
                    session_id: run.session_id.clone(),
                    paths: run.paths.clone(),
                    kind: Kind::Mechanical,
                    at: now,
                    at_unix: now_unix(),
                    transcript: run.transcript_saved != Some(false),
                },
            },
        );
        if !self.submit_handoff_job(Job::Mechanical {
            run: run.id,
            paths: run.paths,
            input: Box::new(input),
        }) {
            self.agent_handoff.pending_writes.remove(&run.id);
            self.finish_agent_handoff(
                terminal_id,
                Err("the handoff worker could not start".into()),
                now,
            );
        }
    }

    /// The tail of the pane's notes and its recent checkpoints, one line
    /// each (the existing notes methods, in process).
    fn handoff_notes(&mut self, pane: &str) -> (Option<String>, Vec<String>) {
        use crate::api::schema::notes::{CheckpointsListParams, NotesGetParams, NotesTarget};
        let target = NotesTarget {
            pane_id: Some(pane.to_string()),
            ..NotesTarget::default()
        };
        let notes = match self.call_method(Method::NotesGet(NotesGetParams {
            target: target.clone(),
            known_revision: None,
        })) {
            Ok(ResponseResult::NotesGet { notes }) => notes
                .text
                .map(|text| handoff::tail_lines(&text, handoff::MECHANICAL_NOTES_LINES)),
            Ok(_) => {
                tracing::debug!(pane, "agent handoff: notes.get answered another result");
                None
            }
            Err(err) => {
                tracing::debug!(pane, code = %err.code, message = %err.message, "agent handoff: no notes for the mechanical handoff");
                None
            }
        };
        let checkpoints = match self.call_method(Method::CheckpointsList(CheckpointsListParams {
            target,
            kinds: Vec::new(),
            since_seq: None,
            limit: Some(MECHANICAL_CHECKPOINTS),
        })) {
            Ok(ResponseResult::CheckpointsList { checkpoints }) => {
                let offset = crate::notes::recall::local_offset_seconds();
                // herdr's own prompt bookmarks are noise here (the prompts
                // section has the prompts).
                crate::notes::recall::pick_checkpoints(checkpoints.checkpoints)
                    .iter()
                    .map(|cp| {
                        let mut line = format!(
                            "{} {}: {}",
                            crate::notes::recall::hhmm(cp.ts, offset),
                            checkpoint_kind_name(cp.kind),
                            handoff::one_line(&cp.title, 160)
                        );
                        if let Some(detail) = cp.detail.as_deref().filter(|d| !d.trim().is_empty())
                        {
                            line.push_str(" \u{2014} ");
                            line.push_str(&handoff::one_line(detail, 200));
                        }
                        line
                    })
                    .collect()
            }
            Ok(_) => {
                tracing::debug!(
                    pane,
                    "agent handoff: checkpoints.list answered another result"
                );
                Vec::new()
            }
            Err(err) => {
                tracing::debug!(pane, code = %err.code, message = %err.message, "agent handoff: no checkpoints for the mechanical handoff");
                Vec::new()
            }
        };
        (notes, checkpoints)
    }

    /// A handoff is saved: remember it (the sidebar's age, the
    /// SessionStart(compact) text), point the agent's notes at it and add a
    /// checkpoint, log it.
    fn record_saved_handoff(&mut self, terminal_id: &TerminalId, saved: SavedHandoff) {
        let path = saved.paths.handoff.display().to_string();
        let kind = saved.kind;
        self.agent_handoff.saved.insert(terminal_id.clone(), saved);
        self.state.agent_context_view_rev =
            self.state.agent_context_view_rev.wrapping_add(1).max(1);
        self.log_handoff(
            terminal_id,
            AgentsActionOutcome::Ok,
            &format!("handoff saved ({}): {path}", kind.as_str()),
        );
        let Some(pane) = self
            .resolve_terminal_target(&terminal_id.to_string())
            .ok()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
        else {
            return;
        };
        use crate::api::schema::notes::{
            CheckpointKind, CheckpointsAddParams, NotesAppendParams, NotesAuthor, NotesTarget,
        };
        let target = NotesTarget {
            pane_id: Some(pane),
            ..NotesTarget::default()
        };
        let who = match kind {
            Kind::Agent => "written by the agent",
            Kind::Mechanical => "written by herdr",
        };
        if let Err(err) = self.call_method(Method::NotesAppend(NotesAppendParams {
            target: target.clone(),
            text: format!("handoff before compaction ({who}): {path}"),
            section: Some("Log".into()),
            stamp: true,
            author: NotesAuthor::Agent,
        })) {
            tracing::debug!(code = %err.code, "agent handoff: notes line skipped");
        }
        if let Err(err) = self.call_method(Method::CheckpointsAdd(CheckpointsAddParams {
            target,
            kind: CheckpointKind::Note,
            title: "handoff saved before compaction".into(),
            detail: Some(format!("{path} ({who})")),
            tags: vec!["handoff".into()],
            author: NotesAuthor::Agent,
        })) {
            tracing::debug!(code = %err.code, "agent handoff: checkpoint skipped");
        }
    }

    /// The compaction of `terminal_id`'s agent ended (its SessionStart
    /// report or the idle fallback).
    fn agent_handoff_compacted(&mut self, terminal_id: &TerminalId, how: &str, now: Instant) {
        self.agent_handoff.armed.insert(terminal_id.clone(), false);
        let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) else {
            return;
        };
        if !matches!(run.phase, Phase::Compacting { .. }) {
            return;
        }
        match handoff::after_compaction(run) {
            Some(phase) => {
                run.enter(phase, now);
                self.log_handoff(
                    terminal_id,
                    AgentsActionOutcome::Ok,
                    &format!("compacted ({how}); the resume line goes in once it is free"),
                );
                self.wake_agent_handoff(now);
            }
            None => {
                self.finish_agent_handoff(terminal_id, Ok(format!("compacted ({how})")), now);
            }
        }
    }

    /// End the run of `terminal_id`: `Ok(detail)` or `Err(reason)` (logged,
    /// and shown as an agent card).
    pub(crate) fn finish_agent_handoff(
        &mut self,
        terminal_id: &TerminalId,
        outcome: Result<String, String>,
        now: Instant,
    ) {
        let Some(run) = self.agent_handoff.runs.remove(terminal_id) else {
            return;
        };
        let pane = self
            .resolve_terminal_target(&terminal_id.to_string())
            .ok()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
            .unwrap_or_default();
        let meta = self.handoff_meta(&run, &pane, Some(&outcome));
        self.submit_handoff_job(Job::Meta {
            paths: run.paths.clone(),
            meta,
        });
        // A run that stopped before /compact leaves the context over the
        // threshold: the automatic trigger may try again after a while.
        let compacted = matches!(run.phase, Phase::Compacting { .. } | Phase::AwaitResume);
        if compacted || outcome.is_ok() {
            // The loop guard for handoffs another agent asks for.
            self.agent_handoff
                .last_finished
                .insert(terminal_id.clone(), now);
        }
        if outcome.is_err() {
            // A mechanical handoff still on the worker (a timed-out write)
            // is not recorded for a run the user was told failed.
            self.agent_handoff.pending_writes.remove(&run.id);
        }
        if outcome.is_err() && !compacted {
            self.agent_handoff.armed.insert(terminal_id.clone(), true);
            self.agent_handoff
                .auto_retry_at
                .insert(terminal_id.clone(), now + AUTO_RETRY);
        }
        // Queued messages held for the run go in now.
        self.message_queue.mark_due();
        match outcome {
            Ok(detail) => self.log_handoff(
                terminal_id,
                AgentsActionOutcome::Ok,
                &format!("done: {detail}"),
            ),
            Err(reason) => {
                tracing::warn!(pane = %pane, phase = run.phase.name(), reason = %reason, "agent handoff failed");
                self.log_handoff(
                    terminal_id,
                    AgentsActionOutcome::Failed,
                    &format!("stopped at {}: {reason}", run.phase.name()),
                );
                self.agent_handoff_card(terminal_id, "Handoff & compact failed", &reason);
            }
        }
    }

    /// An agent card from the pane telling the user a handoff failed.
    fn agent_handoff_card(&mut self, terminal_id: &TerminalId, title: &str, reason: &str) {
        let Some(pane) = self
            .resolve_terminal_target(&terminal_id.to_string())
            .ok()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
        else {
            return;
        };
        let Some(sender) = self.notice_sender(&pane) else {
            return;
        };
        let title = crate::app::agent_notices::sanitize_title(title).unwrap_or_default();
        let body = crate::app::agent_notices::sanitize_body(reason);
        if let Ok((_, outcome)) = self.agent_notices.notify(
            sender,
            crate::api::schema::AgentNoticeKind::Warning,
            title,
            body,
            crate::coordinator::now_unix(),
        ) {
            if outcome == crate::api::schema::AgentNotifyOutcome::Shown {
                self.render_dirty.request_generic();
                self.render_notify.notify_one();
            }
        }
    }

    /// One `actions.jsonl` line (action `handoff`), as herdr.
    fn log_handoff(
        &mut self,
        terminal_id: &TerminalId,
        outcome: AgentsActionOutcome,
        detail: &str,
    ) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        let target = self.resolve_terminal_target(&terminal_id.to_string()).ok();
        let line = LogLine {
            action: "handoff",
            target_tab: target
                .as_ref()
                .and_then(|target| self.public_tab_id(target.ws_idx, target.tab_idx)),
            target_pane: target
                .as_ref()
                .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id)),
            target_name: self
                .state
                .terminals
                .get(terminal_id)
                .and_then(|terminal| terminal.agent_name.clone()),
            detail: Some(detail.to_string()),
            ..LogLine::default()
        };
        let mut entry = self.action_entry(None, outcome, line);
        entry.actor_name = Some("herdr".into());
        if let Err(err) = crate::agents_model::actions_log::append(&dir, &entry) {
            tracing::warn!(err = %err, "agent handoff: cannot append to the action log");
        }
    }

    // ----- the automatic trigger ------------------------------------------------

    /// A context read was stored for `terminal_id`: start a run when it
    /// reached `at_percent` (once per compaction cycle). O(1).
    pub(crate) fn note_agent_handoff_context(&mut self, terminal_id: &TerminalId, now: Instant) {
        let Some(at_percent) = self.agents_config.handoff.auto_percent() else {
            return;
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return;
        };
        let Some(usage) = terminal.agent_context else {
            return;
        };
        if terminal
            .effective_known_agent()
            .and_then(HandoffAgent::from_agent)
            .is_none()
        {
            return;
        }
        // The coordinator is never handed off: no retries every read.
        if self
            .resolve_terminal_target(&terminal_id.to_string())
            .is_ok_and(|target| self.is_coordinator_pane(target.ws_idx, target.pane_id))
        {
            return;
        }
        let armed = self
            .agent_handoff
            .armed
            .get(terminal_id)
            .copied()
            .unwrap_or(true);
        let running = self.agent_handoff.runs.contains_key(terminal_id);
        let percent = usage.percent();
        let (fire, armed) = handoff::auto_fire(armed, percent, at_percent, running);
        if armed {
            // Below the threshold: a pending retry is moot.
            self.agent_handoff.auto_retry_at.remove(terminal_id);
        }
        if fire
            && self
                .agent_handoff
                .auto_retry_at
                .get(terminal_id)
                .is_some_and(|at| now < *at)
        {
            // Armed, but a refused start or a stopped run is recent.
            return;
        }
        self.agent_handoff.armed.insert(terminal_id.clone(), armed);
        if !fire {
            return;
        }
        match self.start_agent_handoff(terminal_id, Trigger::Auto { percent }, now) {
            Ok(_) => {
                self.agent_handoff.auto_retry_at.remove(terminal_id);
            }
            Err(refusal) => {
                // Not disarmed: try again after a while.
                self.agent_handoff.armed.insert(terminal_id.clone(), true);
                self.agent_handoff
                    .auto_retry_at
                    .insert(terminal_id.clone(), now + AUTO_RETRY);
                tracing::info!(code = refusal.code, reason = %refusal.message, "agent handoff: the automatic trigger did not start; it tries again later");
            }
        }
    }

    /// Forget the automatic trigger's marks of terminals that are gone.
    pub(crate) fn retain_agent_handoff_terminals(&mut self) {
        let terminals = &self.state.terminals;
        self.agent_handoff
            .armed
            .retain(|terminal_id, _| terminals.contains_key(terminal_id));
        self.agent_handoff
            .saved
            .retain(|terminal_id, _| terminals.contains_key(terminal_id));
        self.agent_handoff
            .auto_retry_at
            .retain(|terminal_id, _| terminals.contains_key(terminal_id));
        self.agent_handoff
            .last_finished
            .retain(|terminal_id, _| terminals.contains_key(terminal_id));
    }

    /// When the handoff of a terminal was saved (unix seconds), for the
    /// sidebar.
    /// Only for the session it was saved for (not after `/clear` or
    /// another session in the pane).
    pub(crate) fn agent_handoff_saved_unix(&self, terminal_id: &TerminalId) -> Option<u64> {
        let saved = self.agent_handoff.saved.get(terminal_id)?;
        let current = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.persistable_agent_session());
        match current {
            Some(session) if session.session_ref.value != saved.session_id => None,
            _ => Some(saved.at_unix),
        }
    }

    // ----- agents.handoff ---------------------------------------------------------

    pub(super) fn handle_agents_handoff(
        &mut self,
        id: String,
        params: AgentsHandoffParams,
    ) -> String {
        let result = self.agents_handoff(&params);
        Self::model_reply(
            id,
            result.map(|handoff| ResponseResult::AgentsHandoff { handoff }),
        )
    }

    fn agents_handoff(&mut self, params: &AgentsHandoffParams) -> ModelResult<AgentsHandoffResult> {
        let caller = self.model_caller(params.caller_pane.as_deref())?;
        let target = self.resolve_model_tab(&params.target)?;
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let mut line = self.tab_line("handoff", target);
        line.target_pane = self.public_pane_id(target.ws_idx, pane);
        let terminal_id = self
            .state
            .workspaces
            .get(target.ws_idx)
            .and_then(|ws| ws.terminal_id(pane))
            .cloned()
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        if let Some(caller) = &caller {
            self.authorize_on_tab(
                caller,
                target,
                crate::agents_model::policy::Action::SoftEdit(
                    crate::agents_model::policy::SoftEdit::Handoff,
                ),
                &line,
            )?;
            // The loop guard, before the soft edit is spent.
            let other_agent = caller.actor != crate::agents_model::policy::Actor::User
                && caller.terminal_id != terminal_id;
            let running = self.agent_handoff.runs.contains_key(&terminal_id);
            let since_last = self
                .agent_handoff
                .last_finished
                .get(&terminal_id)
                .map(|at| Instant::now().saturating_duration_since(*at));
            if let Some(left) = handoff_cooldown_left(other_agent, running, since_last) {
                let minutes = left.as_secs().div_ceil(60).max(1);
                let message = format!(
                    "this agent finished a handoff and compaction less than {} minutes ago; another one asked by another agent is refused for {minutes} more minute{} (the agent itself or its user may still ask)",
                    HANDOFF_COOLDOWN.as_secs() / 60,
                    if minutes == 1 { "" } else { "s" }
                );
                line.code = Some(error_code::LOOP_GUARD.to_string());
                line.detail = Some(message.clone());
                self.log_model_action(Some(caller), AgentsActionOutcome::Failed, line);
                return Err(ModelError::new(error_code::LOOP_GUARD, message));
            }
            self.take_soft_edit(caller, &line)?;
        }
        let by = caller
            .as_ref()
            .map(|caller| caller.name.clone())
            .unwrap_or_else(|| "the user".to_string());
        match self.start_agent_handoff(&terminal_id, Trigger::Manual { by }, Instant::now()) {
            Ok((pane_id, outcome)) => {
                self.log_model_action(caller.as_ref(), AgentsActionOutcome::Ok, line);
                let run = self.agent_handoff.runs.get(&terminal_id);
                Ok(AgentsHandoffResult {
                    pane_id,
                    outcome,
                    dir: run
                        .map(|run| run.paths.dir.display().to_string())
                        .unwrap_or_default(),
                    phase: run
                        .map(|run| run.phase.name().to_string())
                        .unwrap_or_default(),
                })
            }
            Err(refusal) => {
                line.code = Some(refusal.code.to_string());
                line.detail = Some(refusal.message.clone());
                self.log_model_action(caller.as_ref(), AgentsActionOutcome::Failed, line);
                Err(ModelError::new(refusal.code, refusal.message))
            }
        }
    }

    // ----- pane.report_compact ------------------------------------------------------

    pub(super) fn handle_pane_report_compact(
        &mut self,
        id: String,
        params: PaneReportCompactParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_reported_pane(&params.pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let Some(agent) = crate::detect::parse_agent_label(params.agent.trim()) else {
            return encode_error(id, "invalid_agent", "unknown agent label");
        };
        let live = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(|terminal| terminal.effective_known_agent());
        let none = |id| encode_success(id, ResponseResult::PaneReportCompact { context: None });
        // A late report from an agent the pane no longer runs is dropped.
        if live != Some(agent) || HandoffAgent::from_agent(agent).is_none() {
            return none(id);
        }
        let now = Instant::now();
        match params.event {
            CompactEvent::Pre => {
                self.agent_handoff_precompact(&terminal_id, &params, now);
                none(id)
            }
            CompactEvent::Post => {
                // The compacted session is not the run's: the run ends
                // (no injection of its files, no resume line).
                self.end_agent_handoff_of_other_session(
                    &terminal_id,
                    params.session_id.as_deref(),
                    now,
                );
                let context =
                    self.agent_handoff_injection(&terminal_id, params.session_id.as_deref(), now);
                self.agent_handoff_compacted(&terminal_id, "its SessionStart hook", now);
                encode_success(id, ResponseResult::PaneReportCompact { context })
            }
            CompactEvent::Unknown => none(id),
        }
    }

    /// End the run of `terminal_id` when a hook reported `session_id` for
    /// another session than the run's.
    fn end_agent_handoff_of_other_session(
        &mut self,
        terminal_id: &TerminalId,
        session_id: Option<&str>,
        now: Instant,
    ) {
        let other = self
            .agent_handoff
            .runs
            .get(terminal_id)
            .is_some_and(|run| hook_session_differs(run, session_id));
        if other {
            self.finish_agent_handoff(terminal_id, Err("the agent's session changed".into()), now);
        }
    }

    /// PreCompact: herdr's own `/compact` goes on; any other compaction
    /// gets the transcript copy and a mechanical handoff now.
    fn agent_handoff_precompact(
        &mut self,
        terminal_id: &TerminalId,
        params: &PaneReportCompactParams,
        now: Instant,
    ) {
        if !self.agents_config.handoff.enabled {
            return;
        }
        let auto = params.trigger.as_deref() == Some("auto");
        // Another session compacts: the run ends and the compaction gets
        // its own files below.
        self.end_agent_handoff_of_other_session(terminal_id, params.session_id.as_deref(), now);
        if let Some(run) = self.agent_handoff.runs.get(terminal_id) {
            match run.phase {
                Phase::Compacting { .. } => {
                    let detail = match run.trigger {
                        Trigger::External { .. } => {
                            "PreCompact again while a compaction herdr did not start is on"
                        }
                        _ => "PreCompact (herdr's /compact)",
                    };
                    self.log_handoff(terminal_id, AgentsActionOutcome::Ok, detail);
                    return;
                }
                // Compacted, before the resume line: this is a new
                // compaction, with its own files.
                Phase::AwaitResume => self.finish_agent_handoff(
                    terminal_id,
                    Ok("compacted; a new compaction came before the resume line".into()),
                    now,
                ),
                // The agent is compacted before the run got there (the
                // user's /compact, Claude's auto-compact during the handoff
                // turn or the wait for /compact): the compaction goes on
                // with the run's files, keeping a handoff already written.
                _ => {
                    self.supersede_agent_handoff(terminal_id, params, auto, now);
                    return;
                }
            }
        }
        let Some(target) = self.resolve_terminal_target(&terminal_id.to_string()).ok() else {
            return;
        };
        if self.is_coordinator_pane(target.ws_idx, target.pane_id) {
            return;
        }
        let pane = self
            .public_pane_id(target.ws_idx, target.pane_id)
            .unwrap_or_default();
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return;
        };
        let session = terminal
            .persistable_agent_session()
            .filter(|session| session.agent == "claude");
        let session_id = params
            .session_id
            .clone()
            .filter(|id| !id.trim().is_empty())
            .or_else(|| session.as_ref().map(|s| s.session_ref.value.clone()))
            .unwrap_or_default();
        // Only a path of Claude's own transcript shape for this session is
        // copied (as for the session reports).
        let source = crate::agent_resume::AgentSessionRef::id(&session_id)
            .and_then(|session_ref| {
                crate::agent_resume::transcript_path_from_report(
                    "herdr:claude",
                    "claude",
                    Some(&session_ref),
                    params.transcript_path.as_deref(),
                )
            })
            .or_else(|| session.as_ref().and_then(|s| s.transcript_path.clone()));
        let id = self.next_handoff_id();
        let started_unix = now_unix();
        let paths = handoff::run_paths(&self.agent_handoff.root(), &session_id, started_unix);
        let run = Run {
            id,
            agent: HandoffAgent::Claude,
            trigger: Trigger::External { auto },
            settings: handoff::RunSettings {
                command: None,
                resume: self.agents_config.handoff.resume,
            },
            phase: Phase::Compacting { typed_at: now },
            phase_since: now,
            started_unix,
            session_id: session_id.clone(),
            paths: paths.clone(),
            transcript_saved: None,
            // Known once the worker wrote (or kept) the handoff.
            kind: None,
            free_since: None,
            write_failures: 0,
            missing_since: None,
            reminded: false,
            blocked_since: None,
            blocked_noted: false,
            check_since: None,
        };
        let meta = self.handoff_meta(&run, &pane, None);
        let locate = session.map(|session| crate::agent_context::ContextSession {
            agent: crate::agent_context::ContextAgent::Claude,
            id: session_id,
            session,
        });
        self.agent_handoff.runs.insert(terminal_id.clone(), run);
        self.agent_handoff.armed.insert(terminal_id.clone(), false);
        self.log_handoff(
            terminal_id,
            AgentsActionOutcome::Ok,
            &format!(
                "PreCompact ({}): herdr saves the transcript and a mechanical handoff",
                if auto { "auto" } else { "manual" }
            ),
        );
        // The handoff first: the SessionStart(compact) answer names it, and
        // a long transcript copy must not hold it up (the mechanical one
        // reads the prompts from the live transcript meanwhile).
        self.start_mechanical_handoff(terminal_id, external_reason(auto), now);
        if !self.agent_handoff.runs.contains_key(terminal_id) {
            return;
        }
        if !self.submit_handoff_job(Job::Start {
            run: id,
            paths,
            source,
            locate,
            home: crate::integration::home_dir().ok(),
            meta,
        }) {
            self.finish_agent_handoff(
                terminal_id,
                Err("the handoff worker could not start".into()),
                now,
            );
            return;
        }
        self.wake_agent_handoff(now);
    }

    /// PreCompact while herdr's run had not typed `/compact` yet: the run
    /// becomes the compaction's (its files, a handoff the agent already
    /// wrote), herdr writes its own only when there is none, and the
    /// transcript is copied again now.
    fn supersede_agent_handoff(
        &mut self,
        terminal_id: &TerminalId,
        params: &PaneReportCompactParams,
        auto: bool,
        now: Instant,
    ) {
        let Some(run) = self.agent_handoff.runs.get_mut(terminal_id) else {
            return;
        };
        let was = run.phase;
        let before = trigger_detail(&run.trigger);
        run.trigger = Trigger::External { auto };
        run.enter(Phase::Compacting { typed_at: now }, now);
        let (id, paths, kind, session_id) =
            (run.id, run.paths.clone(), run.kind, run.session_id.clone());
        self.agent_handoff.armed.insert(terminal_id.clone(), false);
        self.log_handoff(
            terminal_id,
            AgentsActionOutcome::Ok,
            &format!(
                "PreCompact ({}) during the run ({before}) at {}: the compaction goes on with the run's files",
                if auto { "auto" } else { "manual" },
                was.name()
            ),
        );
        // A saved handoff stays; herdr's own is on the worker already in
        // WritingFallback; else herdr writes one (keeping a file the agent
        // wrote that no check saw yet).
        if kind.is_none() && was != Phase::WritingFallback {
            self.start_mechanical_handoff(terminal_id, external_reason(auto), now);
        }
        let source =
            crate::agent_resume::AgentSessionRef::id(&session_id).and_then(|session_ref| {
                crate::agent_resume::transcript_path_from_report(
                    "herdr:claude",
                    "claude",
                    Some(&session_ref),
                    params.transcript_path.as_deref(),
                )
            });
        if self.agent_handoff.runs.contains_key(terminal_id) {
            self.submit_transcript_copy(terminal_id, id, paths, source);
        }
        self.wake_agent_handoff(now);
    }

    /// Copy the transcript of `terminal_id`'s session into the run's files
    /// again (on the worker): `source` when the hook named it, else the
    /// session's known or looked-up file.
    fn submit_transcript_copy(
        &mut self,
        terminal_id: &TerminalId,
        run: u64,
        paths: handoff::RunPaths,
        source: Option<PathBuf>,
    ) {
        let session = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.persistable_agent_session())
            .filter(|session| session.agent == "claude");
        let source = source.or_else(|| session.as_ref().and_then(|s| s.transcript_path.clone()));
        let locate = session.map(|session| crate::agent_context::ContextSession {
            agent: crate::agent_context::ContextAgent::Claude,
            id: session.session_ref.value.clone(),
            session,
        });
        if !self.submit_handoff_job(Job::Copy {
            run,
            paths,
            source,
            locate,
            home: crate::integration::home_dir().ok(),
        }) {
            tracing::warn!("agent handoff: the transcript was not copied again");
        }
    }

    /// The SessionStart(compact) text: the run's files, else a handoff saved
    /// in the last [`SAVED_FRESH`] for the same session (`session_id` from
    /// the hook; without one the run's or the saved handoff is offered).
    fn agent_handoff_injection(
        &self,
        terminal_id: &TerminalId,
        session_id: Option<&str>,
        now: Instant,
    ) -> Option<String> {
        if let Some(run) = self
            .agent_handoff
            .runs
            .get(terminal_id)
            .filter(|run| matches!(run.phase, Phase::Compacting { .. }))
            .filter(|run| !hook_session_differs(run, session_id))
        {
            let transcript =
                (run.transcript_saved != Some(false)).then_some(run.paths.transcript.as_path());
            return Some(handoff::injection(
                &run.paths.handoff,
                transcript,
                run.kind.unwrap_or(Kind::Mechanical),
            ));
        }
        let saved = self
            .agent_handoff
            .saved
            .get(terminal_id)
            .filter(|saved| now.saturating_duration_since(saved.at) < SAVED_FRESH)
            .filter(|saved| {
                session_id
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .is_none_or(|id| id == saved.session_id)
            })?;
        Some(handoff::injection(
            &saved.paths.handoff,
            saved.transcript.then_some(saved.paths.transcript.as_path()),
            saved.kind,
        ))
    }
}

/// The loop guard (pure): how much of [`HANDOFF_COOLDOWN`] is left for a
/// handoff `other_agent` (the coordinator or another agent, not the agent
/// itself or the user) asks for, `since_last` after the terminal's last
/// handoff or compaction; `None` lets it through. A run already on answers
/// `already_running` instead.
pub(crate) fn handoff_cooldown_left(
    other_agent: bool,
    running: bool,
    since_last: Option<Duration>,
) -> Option<Duration> {
    if !other_agent || running {
        return None;
    }
    let since = since_last?;
    (since < HANDOFF_COOLDOWN).then(|| HANDOFF_COOLDOWN - since)
}

/// Whether the pane's agent session is no longer the run's: a known session
/// with another id. A run without a session id, or a pane whose binding is
/// empty for the moment, has not changed.
fn session_changed(run: &Run, current: Option<crate::agent_resume::PersistedAgentSession>) -> bool {
    !run.session_id.is_empty()
        && current.is_some_and(|session| session.session_ref.value != run.session_id)
}

/// Whether a hook's `session_id` names another session than the run's
/// (an absent or empty one does not).
fn hook_session_differs(run: &Run, session_id: Option<&str>) -> bool {
    !run.session_id.is_empty()
        && session_id
            .map(str::trim)
            .is_some_and(|id| !id.is_empty() && id != run.session_id)
}

fn external_reason(auto: bool) -> &'static str {
    if auto {
        "Claude compacted the session on its own"
    } else {
        "the session was compacted with /compact"
    }
}

fn trigger_detail(trigger: &Trigger) -> String {
    match trigger {
        Trigger::Auto { percent } => format!("context at {percent}%"),
        Trigger::Manual { by } => format!("asked by {by}"),
        Trigger::External { auto: true } => "Claude's auto-compact".into(),
        Trigger::External { auto: false } => "a /compact herdr did not type".into(),
    }
}

fn checkpoint_kind_name(kind: crate::api::schema::notes::CheckpointKind) -> &'static str {
    use crate::api::schema::notes::CheckpointKind;
    match kind {
        CheckpointKind::Decision => "decision",
        CheckpointKind::Milestone => "milestone",
        CheckpointKind::Failure => "failure",
        CheckpointKind::Bookmark => "bookmark",
        CheckpointKind::Note | CheckpointKind::Unknown => "note",
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl App {
    /// Test seam: wait for the worker's jobs, then step every run once at
    /// the real clock.
    pub(crate) fn drive_agent_handoff_for_test(&mut self) {
        for _ in 0..300 {
            if self.agent_handoff.in_flight == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            let outcomes = self
                .agent_handoff
                .worker
                .as_ref()
                .map(files::Worker::drain)
                .unwrap_or_default();
            for outcome in outcomes {
                self.agent_handoff.in_flight = self.agent_handoff.in_flight.saturating_sub(1);
                self.apply_handoff_outcome(outcome, Instant::now());
            }
        }
        assert_eq!(
            self.agent_handoff.in_flight, 0,
            "the handoff worker answered"
        );
        self.agent_handoff.deadline = Some(Instant::now());
        self.handle_agent_handoff_tasks(Instant::now());
    }

    /// Test seam: every run's agent has been free for the settle time.
    pub(crate) fn settle_agent_handoff_for_test(&mut self) {
        for run in self.agent_handoff.runs.values_mut() {
            run.free_since = Some(Instant::now() - SETTLE - Duration::from_millis(1));
        }
    }
}
