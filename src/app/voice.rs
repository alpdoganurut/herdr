//! Voice mode (fork): an agent's voice session as its screen shows it.
//!
//! The detector reads the fact from the agent's manifest (signal rules,
//! `signal = "voice"`, src/detect/manifest.rs) on every screen scan and
//! sends it with `AppEvent::StateChanged`; it never changes the agent's
//! status (a listening Codex stays idle). The server keeps it on the
//! pane's terminal (`TerminalState::agent_voice`, runtime only), reports it
//! as `AgentInfo.voice` (`live` / `muted`, absent when off), pushes the
//! panes in voice mode to client shells as `endpoint.voice.v1`
//! (src/server/headless/voice.rs; no bincode change) and holds automatic
//! typing into such a pane: agent messages queue with reason `voice mode`,
//! coordinator wake-ups are held, and a guarded `agent.prompt` answers
//! `user_typing` (src/app/message_queue.rs, src/app/coordinator.rs,
//! src/app/api/agents.rs).
//!
//! `AppState::voice_view_rev` is bumped by every voice change and, once a
//! voice mode was seen, by every move or close that can change a pane's
//! public id; the render pass compares it per client (one comparison, no
//! allocation while nothing changed).

use crate::api::schema::{AgentVoiceMode, EventKind};
use crate::detect::AgentVoice;
use crate::layout::PaneId;

use super::App;

/// One pane in voice mode, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VoicePane {
    /// The pane's public id.
    pub pane_id: String,
    pub voice: AgentVoiceMode,
}

impl App {
    /// The voice mode a pane reports now (`Off` for a pane without an agent).
    pub(crate) fn pane_voice(&self, pane_id: PaneId) -> AgentVoice {
        self.find_pane(pane_id)
            .and_then(|(_, pane)| self.state.terminals.get(&pane.attached_terminal_id))
            .map_or(AgentVoice::Off, |terminal| terminal.agent_voice())
    }

    /// Apply the detector's voice report for `pane_id`. `before` is the
    /// value the pane reported before the event was applied (an agent that
    /// went away reports `Off` from then on).
    pub(crate) fn apply_agent_voice(
        &mut self,
        pane_id: PaneId,
        before: AgentVoice,
        voice: AgentVoice,
    ) {
        let Some(terminal_id) = self
            .find_pane(pane_id)
            .map(|(_, pane)| pane.attached_terminal_id.clone())
        else {
            return;
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return;
        };
        let changed = terminal.set_agent_voice(voice);
        if changed || terminal.agent_voice() != before {
            self.note_voice_changed();
        }
    }

    /// A pane's reported voice mode changed: clients get a fresh push, held
    /// messages and wake-ups are looked at again.
    pub(crate) fn note_voice_changed(&mut self) {
        self.state.voice_view_rev = self.state.voice_view_rev.wrapping_add(1).max(1);
        self.message_queue.mark_due();
        self.mark_coordinator_input_dirty();
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    /// Moves and closes can change a pane's public id or drop it: once any
    /// voice mode was seen, clients get the list again. O(1).
    pub(crate) fn note_voice_event(&mut self, event: &EventKind) {
        if self.state.voice_view_rev > 0
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
            self.state.voice_view_rev = self.state.voice_view_rev.wrapping_add(1);
        }
    }

    /// Every pane in voice mode, in workspace and pane order (the push).
    pub(crate) fn voice_panes(&self) -> Vec<VoicePane> {
        let mut panes = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (pane_id, pane) in workspace.tabs.iter().flat_map(|tab| tab.panes.iter()) {
                let Some(voice) = self
                    .state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .and_then(|terminal| AgentVoiceMode::from_detected(terminal.agent_voice()))
                else {
                    continue;
                };
                if let Some(public) = self.public_pane_id(ws_idx, *pane_id) {
                    panes.push(VoicePane {
                        pane_id: public,
                        voice,
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
    use crate::detect::{Agent, AgentState};
    use crate::events::AppEvent;
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
        app.state.workspaces = vec![Workspace::test_new("voice")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    fn pane(app: &App) -> PaneId {
        app.state.workspaces[0].tabs[0].root_pane
    }

    fn report(app: &mut App, agent: Option<Agent>, voice: AgentVoice, process_exited: bool) {
        let pane_id = pane(app);
        app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent,
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            voice,
            process_exited,
            observed_at: std::time::Instant::now(),
        });
    }

    fn info_voice(app: &App) -> (Option<AgentVoiceMode>, serde_json::Value) {
        let info = app.agent_info(0, pane(app)).expect("an agent pane");
        let json = serde_json::to_value(&info).unwrap();
        (info.voice, json["voice"].clone())
    }

    #[tokio::test]
    async fn fork_smoke_a_voice_report_reaches_the_agent_info_and_the_push_without_touching_the_status(
    ) {
        let mut app = app();
        report(&mut app, Some(Agent::Codex), AgentVoice::Off, false);
        assert_eq!(app.state.voice_view_rev, 0, "no voice mode yet, no push");
        assert_eq!(info_voice(&app), (None, serde_json::Value::Null));
        assert!(app.voice_panes().is_empty());

        report(&mut app, Some(Agent::Codex), AgentVoice::Live, false);
        let live_rev = app.state.voice_view_rev;
        assert!(live_rev >= 1);
        assert_eq!(app.pane_voice(pane(&app)), AgentVoice::Live);
        assert_eq!(
            info_voice(&app),
            (Some(AgentVoiceMode::Live), "live".into())
        );
        let info = app.agent_info(0, pane(&app)).unwrap();
        assert_eq!(
            info.agent_status,
            crate::api::schema::AgentStatus::Idle,
            "a listening agent stays idle"
        );
        let public = app.public_pane_id(0, pane(&app)).unwrap();
        assert_eq!(
            app.voice_panes(),
            vec![VoicePane {
                pane_id: public,
                voice: AgentVoiceMode::Live
            }]
        );

        // The same report again changes nothing (no push, no wake-ups).
        report(&mut app, Some(Agent::Codex), AgentVoice::Live, false);
        assert_eq!(app.state.voice_view_rev, live_rev);

        report(&mut app, Some(Agent::Codex), AgentVoice::Muted, false);
        assert!(app.state.voice_view_rev > live_rev);
        assert_eq!(
            info_voice(&app),
            (Some(AgentVoiceMode::Muted), "muted".into())
        );

        // The agent exits: voice off, and clients hear about it.
        let muted_rev = app.state.voice_view_rev;
        report(&mut app, None, AgentVoice::Muted, true);
        assert!(app.state.voice_view_rev > muted_rev);
        assert_eq!(app.pane_voice(pane(&app)), AgentVoice::Off);
        assert!(app.voice_panes().is_empty());
    }

    #[tokio::test]
    async fn a_moved_or_closed_pane_refreshes_the_push_only_once_voice_was_seen() {
        let mut app = app();
        app.note_voice_event(&EventKind::PaneClosed);
        assert_eq!(app.state.voice_view_rev, 0);
        report(&mut app, Some(Agent::Codex), AgentVoice::Live, false);
        let rev = app.state.voice_view_rev;
        app.note_voice_event(&EventKind::PaneMoved);
        assert_eq!(app.state.voice_view_rev, rev + 1);
        app.note_voice_event(&EventKind::PaneAgentStatusChanged);
        assert_eq!(
            app.state.voice_view_rev,
            rev + 1,
            "status never refreshes it"
        );
    }
}
