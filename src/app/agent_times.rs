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
//! bincode change). The time is runtime only: it restarts at the first state
//! change after a server restart.

use crate::api::schema::EventKind;

use super::App;

/// One pane's last agent state change, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentTimePane {
    /// The pane's public id.
    pub pane_id: String,
    /// The `state_change_seq` the time belongs to (`AgentInfo.state_change_seq`).
    pub state_change_seq: u64,
    /// Wall-clock milliseconds (server clock) of that change.
    pub since_unix_ms: u64,
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
                let Some((state_change_seq, since_unix_ms)) = self
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| {
                        Some((
                            terminal.last_agent_state_change_seq?,
                            terminal.agent_state_since_unix_ms?,
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

    #[test]
    fn a_respawn_clears_the_time_with_the_seq() {
        let mut terminal = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            "/tmp".into(),
        );
        terminal.last_agent_state_change_seq = Some(3);
        terminal.agent_state_since_unix_ms = Some(42);
        terminal.clear_agent_runtime_identity_after_respawn();
        assert_eq!(terminal.last_agent_state_change_seq, None);
        assert_eq!(terminal.agent_state_since_unix_ms, None);
    }
}
