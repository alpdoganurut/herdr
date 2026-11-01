//! Agent state times (fork): when each pane's agent entered its current
//! state, for the tabs sidebar's Active agents block and detail strip.
//!
//! The state-change block in `AppState::update_terminal_state_with_completion_policy`
//! (src/app/actions.rs) stamps `TerminalState::agent_state_since_unix_ms`
//! next to `last_agent_state_change_seq` and bumps
//! `AppState::agent_times_view_rev`; a respawn clears both. Once a state
//! change was seen, every move or close that can change a pane's public id
//! bumps the revision too. The render pass compares the revision per client
//! (one comparison, no allocation while nothing changed) and pushes the list
//! as `endpoint.agent-times.v1` (src/server/headless/agent_times.rs; no
//! bincode change).
//!
//! The time survives a restart and a live handoff: the pane snapshot saves
//! it with its state and agent (`PaneSnapshot.agent_state_since`, JSON; the
//! handoff manifest carries the same snapshot), restore parks it on the
//! terminal (`TerminalState::restored_agent_state_since`), and it is kept
//! when the agent comes back in that state: at once when restore already
//! knows the state (`AppState::note_restored_agent_times`, a fresh seq so
//! clients match it), else at the first state change, which takes it only
//! when it lands on the saved state and agent within `RESTORED_SINCE_GRACE`.
//! Any other first change stamps now. A saved time in the future is now.

use std::time::{Duration, Instant};

use crate::api::schema::EventKind;
use crate::detect::AgentState;
use crate::persist::PaneAgentStateSinceSnapshot;
use crate::terminal::state::RestoredAgentStateSince;
use crate::terminal::TerminalState;

use super::App;

/// How long a restored pane's saved state time waits for its agent's state.
pub(crate) const RESTORED_SINCE_GRACE: Duration = Duration::from_secs(120);

fn state_name(state: AgentState) -> Option<&'static str> {
    match state {
        AgentState::Idle => Some("idle"),
        AgentState::Working => Some("working"),
        AgentState::Blocked => Some("blocked"),
        AgentState::Unknown => None,
    }
}

fn parse_state(name: &str) -> Option<AgentState> {
    match name {
        "idle" => Some(AgentState::Idle),
        "working" => Some(AgentState::Working),
        "blocked" => Some(AgentState::Blocked),
        _ => None,
    }
}

/// The pane's state time as saved: its stamped one, else one a restore
/// brought that its agent has not answered yet (within the grace). `None`
/// without an agent state.
pub(crate) fn agent_state_since_snapshot(
    terminal: &TerminalState,
) -> Option<PaneAgentStateSinceSnapshot> {
    let stamped = terminal
        .last_agent_state_change_seq
        .and(terminal.agent_state_since_unix_ms)
        .and_then(|since_unix_ms| {
            Some(PaneAgentStateSinceSnapshot {
                state: state_name(terminal.state)?.to_string(),
                agent: terminal.effective_agent_label().map(str::to_string),
                since_unix_ms,
            })
        });
    stamped.or_else(|| {
        let restored = terminal
            .restored_agent_state_since
            .as_ref()
            .filter(|restored| restored.until > Instant::now())?;
        Some(PaneAgentStateSinceSnapshot {
            state: state_name(restored.state)?.to_string(),
            agent: restored.agent_label.clone(),
            since_unix_ms: restored.since_unix_ms,
        })
    })
}

/// A saved state time, parked on a restored terminal until `now + RESTORED_SINCE_GRACE`.
pub(crate) fn restored_agent_state_since(
    saved: &PaneAgentStateSinceSnapshot,
    now: Instant,
) -> Option<RestoredAgentStateSince> {
    Some(RestoredAgentStateSince {
        state: parse_state(&saved.state)?,
        agent_label: saved.agent.clone(),
        since_unix_ms: saved.since_unix_ms,
        until: now + RESTORED_SINCE_GRACE,
    })
}

/// Takes the parked time (always: only the first change may use it) and
/// answers it, clamped to `now_unix_ms`, when `state` and `agent_label`
/// are the saved ones and the grace has not run out.
pub(crate) fn take_restored_since(
    terminal: &mut TerminalState,
    state: AgentState,
    agent_label: Option<&str>,
    now_unix_ms: u64,
    now: Instant,
) -> Option<u64> {
    let restored = terminal.restored_agent_state_since.take()?;
    (restored.until > now
        && restored.state == state
        && restored.agent_label.as_deref() == agent_label)
        .then_some(restored.since_unix_ms.min(now_unix_ms))
}

impl super::state::AppState {
    /// After a restore or live handoff: every terminal whose agent state is
    /// already the saved one keeps its saved time under a fresh state change
    /// seq (the one clients match the time by), and clients get the list on
    /// the first pass. The others wait for their first state change. O(terminals), once.
    pub(crate) fn note_restored_agent_times(&mut self) {
        let now_unix_ms = crate::codex_sessions::now_unix_ms();
        let now = Instant::now();
        for terminal in self.terminals.values_mut() {
            let state = terminal.state;
            let Some(restored) = terminal.restored_agent_state_since.as_ref() else {
                continue;
            };
            if restored.state != state
                || restored.agent_label.as_deref() != terminal.effective_agent_label()
            {
                continue;
            }
            let agent_label = restored.agent_label.clone();
            let Some(since) =
                take_restored_since(terminal, state, agent_label.as_deref(), now_unix_ms, now)
            else {
                continue;
            };
            self.next_agent_state_change_seq += 1;
            terminal.last_agent_state_change_seq = Some(self.next_agent_state_change_seq);
            terminal.agent_state_since_unix_ms = Some(since);
            self.agent_times_view_rev = self.agent_times_view_rev.wrapping_add(1).max(1);
        }
    }
}

/// One pane's last agent state change, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTimePane {
    /// The pane's public id.
    pub pane_id: String,
    /// The `state_change_seq` the time belongs to (`AgentInfo.state_change_seq`).
    pub state_change_seq: u64,
    /// Wall-clock milliseconds (server clock) of that change.
    pub since_unix_ms: u64,
    /// Fork (autosuspend): the pane's agent is parked by herdr for idling
    /// (the detail strip says so). Absent when false; older clients ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_suspended: bool,
}

impl App {
    /// Moves and closes can change a pane's public id or drop it: once any
    /// state change was stamped, clients get the list again. O(1).
    pub(crate) fn note_agent_times_event(&mut self, event: &EventKind) {
        if self.state.agent_times_view_rev > 0
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
            self.state.agent_times_view_rev = self.state.agent_times_view_rev.wrapping_add(1);
        }
    }

    /// Every pane with a stamped state change, sorted by public pane id (the push).
    pub(crate) fn agent_times(&self) -> Vec<AgentTimePane> {
        let mut panes = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (pane_id, pane) in workspace.tabs.iter().flat_map(|tab| tab.panes.iter()) {
                let Some((state_change_seq, since_unix_ms, auto_suspended)) = self
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| {
                        Some((
                            terminal.last_agent_state_change_seq?,
                            terminal.agent_state_since_unix_ms?,
                            terminal
                                .suspended_agent
                                .as_ref()
                                .is_some_and(|record| record.auto),
                        ))
                    })
                else {
                    continue;
                };
                if let Some(public) = self.public_pane_id(ws_idx, *pane_id) {
                    panes.push(AgentTimePane {
                        pane_id: public,
                        state_change_seq,
                        since_unix_ms,
                        auto_suspended,
                    });
                }
            }
        }
        panes.sort_by(|left, right| left.pane_id.cmp(&right.pane_id));
        panes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState, AgentVoice};
    use crate::events::AppEvent;
    use crate::layout::PaneId;
    use crate::workspace::Workspace;

    fn app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("times")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    fn pane(app: &App) -> PaneId {
        app.state.workspaces[0].tabs[0].root_pane
    }

    fn report(app: &mut App, agent: Option<Agent>, state: AgentState) {
        let pane_id = pane(app);
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
            visible_blocker: false,
            visible_working: matches!(state, AgentState::Working),
            voice: AgentVoice::Off,
            process_exited: false,
            observed_at: std::time::Instant::now(),
        });
    }

    fn terminal(app: &App) -> &crate::terminal::TerminalState {
        let pane = &app.state.workspaces[0].tabs[0].panes[&pane(app)];
        &app.state.terminals[&pane.attached_terminal_id]
    }

    #[tokio::test]
    async fn a_state_change_stamps_since_and_bumps_the_revision() {
        let mut app = app();
        assert_eq!(app.state.agent_times_view_rev, 0);
        assert!(app.agent_times().is_empty());

        let before = crate::codex_sessions::now_unix_ms();
        report(&mut app, Some(Agent::Claude), AgentState::Working);
        let after = crate::codex_sessions::now_unix_ms();
        let rev = app.state.agent_times_view_rev;
        assert!(rev >= 1, "a state change bumps the revision");
        let seq = terminal(&app)
            .last_agent_state_change_seq
            .expect("a state change seq");
        let since = terminal(&app)
            .agent_state_since_unix_ms
            .expect("a stamped since");
        assert!((before..=after).contains(&since));
        let public = app.public_pane_id(0, pane(&app)).unwrap();
        assert_eq!(
            app.agent_times(),
            vec![AgentTimePane {
                pane_id: public,
                state_change_seq: seq,
                since_unix_ms: since,
                auto_suspended: false,
            }]
        );
        let info = app.agent_info(0, pane(&app)).expect("an agent pane");
        assert_eq!(info.state_change_seq, seq, "keyed like the agent row");

        // The same state again is no change: no stamp, no push.
        report(&mut app, Some(Agent::Claude), AgentState::Working);
        assert_eq!(app.state.agent_times_view_rev, rev);
        assert_eq!(terminal(&app).agent_state_since_unix_ms, Some(since));
    }

    #[tokio::test]
    async fn a_move_bumps_the_revision_only_once_a_change_was_seen() {
        let mut app = app();
        app.note_agent_times_event(&EventKind::PaneMoved);
        assert_eq!(app.state.agent_times_view_rev, 0);
        report(&mut app, Some(Agent::Claude), AgentState::Working);
        let rev = app.state.agent_times_view_rev;
        app.note_agent_times_event(&EventKind::PaneMoved);
        assert_eq!(app.state.agent_times_view_rev, rev + 1);
        app.note_agent_times_event(&EventKind::PaneAgentStatusChanged);
        assert_eq!(app.state.agent_times_view_rev, rev + 1);
    }

    fn park(app: &mut App, state: AgentState, since_unix_ms: u64) {
        let pane = &app.state.workspaces[0].tabs[0].panes[&pane(app)];
        let id = pane.attached_terminal_id.clone();
        app.state
            .terminals
            .get_mut(&id)
            .expect("terminal")
            .restored_agent_state_since = Some(RestoredAgentStateSince {
            state,
            agent_label: Some(crate::detect::agent_label(Agent::Claude).to_string()),
            since_unix_ms,
            until: Instant::now() + RESTORED_SINCE_GRACE,
        });
    }

    fn terminal_mut(app: &mut App) -> &mut crate::terminal::TerminalState {
        let pane = &app.state.workspaces[0].tabs[0].panes[&pane(app)];
        let id = pane.attached_terminal_id.clone();
        app.state.terminals.get_mut(&id).expect("terminal")
    }

    const HOUR_MS: u64 = 3_600_000;

    #[tokio::test]
    async fn a_restored_time_is_kept_when_the_first_change_lands_on_its_state() {
        let mut app = app();
        let saved = crate::codex_sessions::now_unix_ms() - 2 * HOUR_MS;
        park(&mut app, AgentState::Idle, saved);

        report(&mut app, Some(Agent::Claude), AgentState::Idle);
        let seq = terminal(&app)
            .last_agent_state_change_seq
            .expect("a state change seq");
        assert_eq!(terminal(&app).agent_state_since_unix_ms, Some(saved));
        assert_eq!(terminal(&app).restored_agent_state_since, None);
        assert!(app.state.agent_times_view_rev >= 1);
        let info = app.agent_info(0, pane(&app)).expect("an agent pane");
        assert_eq!(info.state_change_seq, seq);
        assert_eq!(app.agent_times()[0].since_unix_ms, saved);
        assert_eq!(app.agent_times()[0].state_change_seq, seq);

        // The next change is a real one: now.
        let before = crate::codex_sessions::now_unix_ms();
        report(&mut app, Some(Agent::Claude), AgentState::Working);
        let since = terminal(&app).agent_state_since_unix_ms.expect("a since");
        assert!(since >= before);
    }

    #[tokio::test]
    async fn a_restored_time_is_dropped_when_the_state_differs() {
        let mut app = app();
        let saved = crate::codex_sessions::now_unix_ms() - 2 * HOUR_MS;
        park(&mut app, AgentState::Idle, saved);

        let before = crate::codex_sessions::now_unix_ms();
        report(&mut app, Some(Agent::Claude), AgentState::Working);
        let after = crate::codex_sessions::now_unix_ms();
        let since = terminal(&app).agent_state_since_unix_ms.expect("a since");
        assert!((before..=after).contains(&since));
        assert_eq!(terminal(&app).restored_agent_state_since, None);

        // Consumed by the first change: a later idle is not the saved one.
        report(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert!(terminal(&app).agent_state_since_unix_ms >= Some(before));
    }

    #[tokio::test]
    async fn a_restored_time_needs_the_same_agent_and_the_grace() {
        let mut app = app();
        let saved = crate::codex_sessions::now_unix_ms() - 2 * HOUR_MS;
        park(&mut app, AgentState::Idle, saved);
        let before = crate::codex_sessions::now_unix_ms();
        report(&mut app, Some(Agent::Codex), AgentState::Idle);
        assert!(terminal(&app).agent_state_since_unix_ms >= Some(before));

        let mut app = self::app();
        park(&mut app, AgentState::Idle, saved);
        if let Some(restored) = terminal_mut(&mut app).restored_agent_state_since.as_mut() {
            restored.until = Instant::now() - Duration::from_secs(1);
        }
        let before = crate::codex_sessions::now_unix_ms();
        report(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert!(terminal(&app).agent_state_since_unix_ms >= Some(before));
    }

    #[tokio::test]
    async fn a_restored_time_in_the_future_is_now() {
        let mut app = app();
        park(
            &mut app,
            AgentState::Idle,
            crate::codex_sessions::now_unix_ms() + 10 * HOUR_MS,
        );
        report(&mut app, Some(Agent::Claude), AgentState::Idle);
        let after = crate::codex_sessions::now_unix_ms();
        let since = terminal(&app).agent_state_since_unix_ms.expect("a since");
        assert!(since <= after);
    }

    #[tokio::test]
    async fn a_settled_restored_state_keeps_its_time_under_a_fresh_seq() {
        let mut app = app();
        let saved = crate::codex_sessions::now_unix_ms() - 2 * HOUR_MS;
        // Restore already knows the state (a resumed agent, a hook status).
        let _ = terminal_mut(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        assert_eq!(terminal(&app).state, AgentState::Idle);
        assert_eq!(terminal(&app).last_agent_state_change_seq, None);
        park(&mut app, AgentState::Idle, saved);

        app.state.note_restored_agent_times();
        let seq = terminal(&app)
            .last_agent_state_change_seq
            .expect("a fresh seq");
        assert_eq!(seq, app.state.next_agent_state_change_seq);
        assert_eq!(terminal(&app).agent_state_since_unix_ms, Some(saved));
        assert_eq!(terminal(&app).restored_agent_state_since, None);
        assert_eq!(app.state.agent_times_view_rev, 1);
        let info = app.agent_info(0, pane(&app)).expect("an agent pane");
        assert_eq!(info.state_change_seq, seq, "clients match the time by it");
        assert_eq!(
            app.agent_times()[0],
            AgentTimePane {
                pane_id: app.public_pane_id(0, pane(&app)).unwrap(),
                state_change_seq: seq,
                since_unix_ms: saved,
                auto_suspended: false,
            }
        );
        // Detection confirming the state changes nothing.
        report(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(terminal(&app).agent_state_since_unix_ms, Some(saved));
    }

    #[tokio::test]
    async fn an_unsettled_restored_state_waits_for_its_first_change() {
        let mut app = app();
        let saved = crate::codex_sessions::now_unix_ms() - 2 * HOUR_MS;
        park(&mut app, AgentState::Blocked, saved);
        app.state.note_restored_agent_times();
        assert_eq!(app.state.agent_times_view_rev, 0, "nothing to push yet");
        assert!(terminal(&app).restored_agent_state_since.is_some());
        report(&mut app, Some(Agent::Claude), AgentState::Blocked);
        assert_eq!(terminal(&app).agent_state_since_unix_ms, Some(saved));
    }

    #[tokio::test]
    async fn the_saved_time_is_the_stamped_one_else_the_parked_one() {
        let mut app = app();
        assert_eq!(agent_state_since_snapshot(terminal(&app)), None);
        let saved = crate::codex_sessions::now_unix_ms() - HOUR_MS;
        park(&mut app, AgentState::Working, saved);
        // A second restart before the agent answered keeps the parked time.
        let parked = agent_state_since_snapshot(terminal(&app)).expect("parked");
        assert_eq!(
            parked,
            PaneAgentStateSinceSnapshot {
                state: "working".into(),
                agent: Some(crate::detect::agent_label(Agent::Claude).to_string()),
                since_unix_ms: saved,
            }
        );
        assert_eq!(
            restored_agent_state_since(&parked, Instant::now()).map(|r| (r.state, r.since_unix_ms)),
            Some((AgentState::Working, saved))
        );
        report(&mut app, Some(Agent::Claude), AgentState::Blocked);
        let stamped = agent_state_since_snapshot(terminal(&app)).expect("stamped");
        assert_eq!(stamped.state, "blocked");
        assert_eq!(
            Some(stamped.since_unix_ms),
            terminal(&app).agent_state_since_unix_ms
        );
        // A restore whose grace ran out saves nothing.
        let mut expired = self::app();
        park(&mut expired, AgentState::Working, saved);
        if let Some(restored) = terminal_mut(&mut expired)
            .restored_agent_state_since
            .as_mut()
        {
            restored.until = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(agent_state_since_snapshot(terminal(&expired)), None);
        // A state this build does not know restores no time.
        let unknown = PaneAgentStateSinceSnapshot {
            state: "dreaming".into(),
            ..stamped
        };
        assert_eq!(restored_agent_state_since(&unknown, Instant::now()), None);
    }

    #[test]
    fn a_respawn_clears_the_time_with_the_seq() {
        let mut terminal = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            "/tmp".into(),
        );
        terminal.last_agent_state_change_seq = Some(3);
        terminal.agent_state_since_unix_ms = Some(42);
        terminal.restored_agent_state_since = Some(RestoredAgentStateSince {
            state: AgentState::Idle,
            agent_label: None,
            since_unix_ms: 7,
            until: Instant::now() + RESTORED_SINCE_GRACE,
        });
        terminal.clear_agent_runtime_identity_after_respawn();
        assert_eq!(terminal.last_agent_state_change_seq, None);
        assert_eq!(terminal.agent_state_since_unix_ms, None);
        assert_eq!(terminal.restored_agent_state_since, None);
    }
}
