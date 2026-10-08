//! Agent context use (fork): how full each Claude / Codex pane agent's
//! context window is, read from the agent's own session file
//! ([`crate::agent_context`] has the parsing and the evidence).
//!
//! Never per frame: a slow server tick (every `PROBE_INTERVAL`) lists the
//! panes whose read is due: the agent's state changed since the last read
//! (a turn finished, it blocked, it started working), or the last read is
//! `WORKING_REREAD` old while it works (`IDLE_REREAD` otherwise, for late
//! writes and compactions). A background thread resolves each session's
//! file, stats it and reads its tail only when the size or mtime changed.
//! The tick applies the results to `TerminalState::agent_context` and bumps
//! `AppState::agent_context_view_rev` only when a value changed; the render
//! pass pushes the list as `endpoint.agent-context.v1`
//! (src/server/headless/agent_context.rs; no bincode change), and
//! `agent.list` / `agent.get` carry it as `AgentInfo.context`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::App;
use crate::agent_context::{ContextAgent, ContextCache, ContextSession, ContextUsage};
use crate::api::schema::EventKind;
use crate::detect::AgentState;
use crate::terminal::TerminalId;

/// How often the tick looks for due panes.
const PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// A working agent's file is read at most this often.
const WORKING_REREAD: Duration = Duration::from_secs(30);
/// Any other state's file is checked this often (a stat while unchanged).
const IDLE_REREAD: Duration = Duration::from_secs(120);

/// One pane's context use, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentContextPane {
    /// The pane's public id.
    pub pane_id: String,
    /// Tokens in the context after the agent's last request.
    pub used: u64,
    /// The context window in tokens.
    pub window: u64,
}

/// What the tick last read for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadMark {
    session: (ContextAgent, String),
    state_change_seq: Option<u64>,
    at: Instant,
}

#[derive(Default)]
pub(crate) struct AgentContextProbe {
    deadline: Option<Instant>,
    thread: Option<std::thread::JoinHandle<ProbeOutcome>>,
    /// Session files by session id, handed to and back from the thread.
    cache: Option<ContextCache>,
    marks: HashMap<TerminalId, ReadMark>,
}

pub(crate) struct ProbeRequest {
    terminal_id: TerminalId,
    session: ContextSession,
}

/// One read: the pane, the session it was read for (agent and id) and the
/// usage found, if any.
pub(crate) type ProbeResult = (
    TerminalId,
    (ContextAgent, String),
    Option<crate::agent_context::TranscriptUsage>,
);

pub(crate) struct ProbeOutcome {
    cache: ContextCache,
    results: Vec<ProbeResult>,
}

impl App {
    pub(crate) fn next_agent_context_probe_deadline(&self) -> Option<Instant> {
        self.agent_context.deadline
    }

    /// The tick: apply a finished read, then start the next one when a pane
    /// is due. Returns whether pane state changed.
    pub(crate) fn handle_agent_context_probe(&mut self, now: Instant) -> bool {
        let mut changed = self.reap_agent_context_probe();
        if self
            .agent_context
            .deadline
            .is_some_and(|deadline| now < deadline)
        {
            return changed;
        }
        self.agent_context.deadline = Some(now + PROBE_INTERVAL);
        if self.agent_context.thread.is_some() {
            return changed;
        }
        let (requests, cleared) = self.agent_context_requests(now);
        changed |= cleared;
        if requests.is_empty() {
            return changed;
        }
        let marks: Vec<(TerminalId, ReadMark)> = requests
            .iter()
            .map(|request| {
                let seq = self
                    .state
                    .terminals
                    .get(&request.terminal_id)
                    .and_then(|terminal| terminal.last_agent_state_change_seq);
                (
                    request.terminal_id.clone(),
                    ReadMark {
                        session: (request.session.agent, request.session.id.clone()),
                        state_change_seq: seq,
                        at: now,
                    },
                )
            })
            .collect();
        let home = crate::integration::home_dir().ok();
        let codex_root = crate::integration::codex_dir()
            .ok()
            .map(|dir| dir.join("sessions"));
        let cache = self.agent_context.cache.take().unwrap_or_default();
        match std::thread::Builder::new()
            .name("herdr-agent-context".into())
            .spawn(move || run_agent_context_probe(requests, cache, home, codex_root))
        {
            // Marked only once the read runs, so a failed spawn retries at
            // the next tick.
            Ok(thread) => {
                self.agent_context.thread = Some(thread);
                self.agent_context.marks.extend(marks);
            }
            Err(err) => tracing::warn!(err = %err, "failed to spawn agent context thread"),
        }
        changed
    }

    fn reap_agent_context_probe(&mut self) -> bool {
        if !self
            .agent_context
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return false;
        }
        let Some(thread) = self.agent_context.thread.take() else {
            return false;
        };
        match thread.join() {
            Ok(outcome) => {
                self.agent_context.cache = Some(outcome.cache);
                self.apply_agent_context_results(outcome.results)
            }
            Err(_) => {
                tracing::warn!("agent context thread panicked");
                false
            }
        }
    }

    /// The live Claude / Codex agent's session of a terminal.
    fn context_session(terminal: &crate::terminal::TerminalState) -> Option<ContextSession> {
        let agent = ContextAgent::from_agent(terminal.effective_known_agent()?)?;
        let session = terminal.persistable_agent_session()?;
        if session.agent != agent.key() {
            return None;
        }
        Some(ContextSession {
            agent,
            id: session.session_ref.value.clone(),
            session,
        })
    }

    /// The panes whose read is due, and whether values of panes without a
    /// live session were cleared. Forgets marks and cached files of panes
    /// and sessions that are gone. O(terminals).
    pub(crate) fn agent_context_requests(&mut self, now: Instant) -> (Vec<ProbeRequest>, bool) {
        let mut requests = Vec::new();
        let mut live = Vec::new();
        let mut cleared = false;
        let marks = &self.agent_context.marks;
        for (terminal_id, terminal) in self.state.terminals.iter_mut() {
            let Some(session) = Self::context_session(terminal) else {
                if terminal.agent_context.take().is_some() {
                    cleared = true;
                }
                continue;
            };
            live.push((terminal_id.clone(), (session.agent, session.id.clone())));
            // A new session: the old one's value no longer applies.
            if marks
                .get(terminal_id)
                .is_some_and(|mark| mark.session.0 != session.agent || mark.session.1 != session.id)
                && terminal.agent_context.take().is_some()
            {
                cleared = true;
            }
            let due = marks.get(terminal_id).is_none_or(|mark| {
                let reread = if terminal.state == AgentState::Working {
                    WORKING_REREAD
                } else {
                    IDLE_REREAD
                };
                mark.session.0 != session.agent
                    || mark.session.1 != session.id
                    || mark.state_change_seq != terminal.last_agent_state_change_seq
                    || now.saturating_duration_since(mark.at) >= reread
            });
            if due {
                requests.push(ProbeRequest {
                    terminal_id: terminal_id.clone(),
                    session,
                });
            }
        }
        self.agent_context
            .marks
            .retain(|terminal_id, _| live.iter().any(|(live, _)| live == terminal_id));
        if let Some(cache) = self.agent_context.cache.as_mut() {
            let sessions: Vec<_> = live.into_iter().map(|(_, session)| session).collect();
            cache.retain_sessions(&sessions);
        }
        if cleared {
            self.bump_agent_context_view_rev();
        }
        (requests, cleared)
    }

    /// Store each read's value with the window resolved from `[agents]
    /// context_window`; bumps the revision when any value changed.
    pub(crate) fn apply_agent_context_results(&mut self, results: Vec<ProbeResult>) -> bool {
        let mut changed = false;
        for (terminal_id, (agent, session_id), usage) in results {
            let value = usage.as_ref().and_then(|usage| {
                crate::agent_context::resolve_usage(
                    agent,
                    usage,
                    &self.agents_config.context_window,
                )
            });
            let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
                continue;
            };
            // The pane's agent or session changed while the thread read.
            if !Self::context_session(terminal)
                .is_some_and(|session| session.agent == agent && session.id == session_id)
            {
                continue;
            }
            if terminal.agent_context != value {
                terminal.agent_context = value;
                changed = true;
            }
        }
        if changed {
            self.bump_agent_context_view_rev();
        }
        changed
    }

    fn bump_agent_context_view_rev(&mut self) {
        self.state.agent_context_view_rev =
            self.state.agent_context_view_rev.wrapping_add(1).max(1);
    }

    /// Moves and closes can change a pane's public id or drop it: once a
    /// value was pushed, clients get the list again. O(1).
    pub(crate) fn note_agent_context_event(&mut self, event: &EventKind) {
        if self.state.agent_context_view_rev > 0
            && matches!(
                event,
                EventKind::PaneMoved
                    | EventKind::PaneClosed
                    | EventKind::PaneExited
                    | EventKind::TabMoved
                    | EventKind::TabClosed
                    | EventKind::WorkspaceMoved
                    | EventKind::WorkspaceClosed
            )
        {
            self.bump_agent_context_view_rev();
        }
    }

    /// Every pane with a known context use, sorted by public pane id (the push).
    pub(crate) fn agent_context_panes(&self) -> Vec<AgentContextPane> {
        let mut panes = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (pane_id, pane) in workspace.tabs.iter().flat_map(|tab| tab.panes.iter()) {
                let Some(usage) = self
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| terminal.agent_context)
                else {
                    continue;
                };
                if let Some(public) = self.public_pane_id(ws_idx, *pane_id) {
                    panes.push(AgentContextPane {
                        pane_id: public,
                        used: usage.used,
                        window: usage.window,
                    });
                }
            }
        }
        panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        panes
    }
}

/// `AgentInfo.context` for a terminal's value.
pub(crate) fn agent_context_info(
    usage: Option<ContextUsage>,
) -> Option<crate::api::schema::AgentContextInfo> {
    usage.map(|usage| crate::api::schema::AgentContextInfo {
        used_tokens: usage.used,
        window_tokens: usage.window,
        percent: usage.percent(),
    })
}

fn run_agent_context_probe(
    requests: Vec<ProbeRequest>,
    mut cache: ContextCache,
    home: Option<PathBuf>,
    codex_root: Option<PathBuf>,
) -> ProbeOutcome {
    let results = requests
        .into_iter()
        .map(|request| {
            let usage = cache.usage(&request.session, |session| {
                crate::agent_context::locate_session_file(
                    session,
                    home.as_deref(),
                    codex_root.as_deref(),
                )
            });
            (
                request.terminal_id,
                (request.session.agent, request.session.id),
                usage,
            )
        })
        .collect();
    ProbeOutcome { cache, results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_context::TranscriptUsage;
    use crate::workspace::Workspace;

    fn app_with_pane() -> (App, TerminalId) {
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let workspace = Workspace::test_new("context");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane)
            .expect("root pane terminal")
            .clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        (app, terminal_id)
    }

    fn run_claude(app: &mut App, terminal_id: &TerminalId, session_id: &str) {
        let terminal = app.state.terminals.get_mut(terminal_id).expect("terminal");
        terminal.set_detected_agent_process_at(crate::detect::Agent::Claude, Instant::now());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id(session_id).unwrap(),
            transcript_path: None,
        });
    }

    fn usage(used: u64) -> Option<TranscriptUsage> {
        Some(TranscriptUsage {
            used,
            model: Some("claude-opus-5-5".into()),
            window: None,
        })
    }

    #[test]
    fn a_pane_is_due_once_then_again_on_a_state_change_or_a_session_change() {
        let (mut app, terminal_id) = app_with_pane();
        let now = Instant::now();
        assert!(
            app.agent_context_requests(now).0.is_empty(),
            "a plain shell"
        );
        run_claude(&mut app, &terminal_id, "s-1");
        let (requests, _) = app.agent_context_requests(now);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].session.id, "s-1");
        app.agent_context.marks.insert(
            terminal_id.clone(),
            ReadMark {
                session: (ContextAgent::Claude, "s-1".into()),
                state_change_seq: app.state.terminals[&terminal_id].last_agent_state_change_seq,
                at: now,
            },
        );
        assert!(app.agent_context_requests(now).0.is_empty(), "just read");
        assert_eq!(
            app.agent_context_requests(now + IDLE_REREAD).0.len(),
            1,
            "an idle pane is checked again after a while"
        );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .last_agent_state_change_seq = Some(99);
        assert_eq!(app.agent_context_requests(now).0.len(), 1, "a state change");
        app.agent_context
            .marks
            .get_mut(&terminal_id)
            .unwrap()
            .state_change_seq = Some(99);
        run_claude(&mut app, &terminal_id, "s-2");
        assert_eq!(app.agent_context_requests(now).0.len(), 1, "a new session");
    }

    #[test]
    fn results_bump_the_revision_only_when_a_value_changes() {
        let (mut app, terminal_id) = app_with_pane();
        run_claude(&mut app, &terminal_id, "s-1");
        assert_eq!(app.state.agent_context_view_rev, 0);
        assert!(app.apply_agent_context_results(vec![(
            terminal_id.clone(),
            (ContextAgent::Claude, "s-1".into()),
            usage(164_000)
        )]));
        let rev = app.state.agent_context_view_rev;
        assert!(rev >= 1);
        assert_eq!(
            app.state.terminals[&terminal_id].agent_context,
            Some(ContextUsage {
                used: 164_000,
                window: 200_000
            })
        );
        let public = app
            .public_pane_id(0, app.state.workspaces[0].tabs[0].root_pane)
            .unwrap();
        assert_eq!(
            app.agent_context_panes(),
            vec![AgentContextPane {
                pane_id: public,
                used: 164_000,
                window: 200_000,
            }]
        );
        let info = app
            .agent_info(0, app.state.workspaces[0].tabs[0].root_pane)
            .expect("an agent pane");
        let context = info.context.expect("AgentInfo.context");
        assert_eq!(
            (context.used_tokens, context.window_tokens, context.percent),
            (164_000, 200_000, 82)
        );
        // The same value again: no push.
        assert!(!app.apply_agent_context_results(vec![(
            terminal_id.clone(),
            (ContextAgent::Claude, "s-1".into()),
            usage(164_000)
        )]));
        assert_eq!(app.state.agent_context_view_rev, rev);
        // A result for an agent or a session the pane no longer runs is
        // dropped.
        assert!(!app.apply_agent_context_results(vec![(
            terminal_id.clone(),
            (ContextAgent::Claude, "s-0".into()),
            usage(1)
        )]));
        assert!(!app.apply_agent_context_results(vec![(
            terminal_id.clone(),
            (ContextAgent::Codex, "s-1".into()),
            usage(1)
        )]));
        // Moves re-send; other events do not.
        app.note_agent_context_event(&EventKind::PaneMoved);
        assert_eq!(app.state.agent_context_view_rev, rev + 1);
        app.note_agent_context_event(&EventKind::PaneAgentStatusChanged);
        assert_eq!(app.state.agent_context_view_rev, rev + 1);
    }

    #[test]
    fn a_configured_window_wins_and_a_pane_without_a_session_is_cleared() {
        let (mut app, terminal_id) = app_with_pane();
        run_claude(&mut app, &terminal_id, "s-1");
        app.agents_config
            .context_window
            .insert("claude".into(), 1_000_000);
        app.apply_agent_context_results(vec![(
            terminal_id.clone(),
            (ContextAgent::Claude, "s-1".into()),
            usage(164_000),
        )]);
        assert_eq!(
            app.state.terminals[&terminal_id]
                .agent_context
                .map(|usage| usage.window),
            Some(1_000_000)
        );
        let rev = app.state.agent_context_view_rev;
        // The agent leaves: the value goes and clients hear of it.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .clear_agent_runtime_identity_after_respawn();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("x").unwrap(),
                transcript_path: None,
            });
        let (requests, cleared) = app.agent_context_requests(Instant::now());
        assert!(requests.is_empty());
        assert!(cleared);
        assert_eq!(app.state.terminals[&terminal_id].agent_context, None);
        assert_eq!(app.state.agent_context_view_rev, rev + 1);
        assert!(app.agent_context_panes().is_empty());
    }

    #[test]
    fn the_worker_reads_a_claude_transcript_under_home() {
        let home = std::env::temp_dir().join(format!(
            "herdr-agent-context-home-{}-{}",
            std::process::id(),
            crate::codex_sessions::now_unix_ms()
        ));
        let project = home.join(".claude").join("projects").join("-tmp-repo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("s-9.jsonl"),
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":1,\"cache_read_input_tokens\":185999,\"output_tokens\":0}}}\n",
        )
        .unwrap();
        let (mut app, terminal_id) = app_with_pane();
        run_claude(&mut app, &terminal_id, "s-9");
        let (requests, _) = app.agent_context_requests(Instant::now());
        let outcome =
            run_agent_context_probe(requests, ContextCache::default(), Some(home.clone()), None);
        let _ = std::fs::remove_dir_all(&home);
        assert!(app.apply_agent_context_results(outcome.results));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .agent_context
                .map(ContextUsage::percent),
            Some(93)
        );
    }
}
