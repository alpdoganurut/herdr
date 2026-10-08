//! Fork: `tab.set_muted`, a tab's notification mute.
//!
//! A muted tab raises none of herdr's own automatic notifications: the
//! Finished / NeedsAttention toasts, their sounds, system and terminal
//! notifications, terminal bells, and idle / scheduled reminder alerts.
//! Agent cards (`agent.notify`, the `agents_notify` tool) still show and
//! ring, and the tab's status, the Active block and the sidebar status stay
//! as they are. The mute is shared session state (server-owned, persisted
//! in session.json with the tab, carried by a whole-tab `pane.move`, a live
//! handoff and close/reopen), like the pin.
//!
//! The server gates its own paths on `Tab.muted`
//! (`AppState::pane_notifications_muted`); clients learn the muted tab ids
//! from the `endpoint.tab-mutes.v1` push (`src/server/headless/tab_mutes.rs`)
//! to gate the client-side ones (toasts, reminders) and draw the mark, not
//! from the bincode shell snapshot, so `PROTOCOL_VERSION` is unchanged. The
//! push is a revision compare in the render pass
//! (`AppState::tab_mutes_view_rev`), 0 until the first mute.

use crate::api::schema::{EventKind, ResponseResult, TabSetMutedParams};
use crate::layout::PaneId;

use super::api::responses::{encode_error, encode_success};
use super::state::AppState;
use super::App;

impl App {
    pub(super) fn handle_tab_set_muted(&mut self, id: String, params: TabSetMutedParams) -> String {
        let tab_id = params.tab_id.as_str();
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(tab_id) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        if tab.muted != params.muted {
            tab.muted = params.muted;
            tracing::info!(
                event = "tab.set_muted",
                tab_id = %tab_id,
                muted = params.muted,
                "tab mute set"
            );
            self.schedule_session_save();
            self.bump_tab_mutes_view();
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    /// The mutes changed: clients get a fresh push.
    pub(crate) fn bump_tab_mutes_view(&mut self) {
        self.state.tab_mutes_view_rev = self.state.tab_mutes_view_rev.wrapping_add(1).max(1);
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    /// Moves and closes can change a tab's public id or drop it: once any
    /// mute existed, clients get the list again. O(1).
    pub(crate) fn note_tab_mutes_event(&mut self, event: &EventKind) {
        if self.state.tab_mutes_view_rev > 0
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
            self.state.tab_mutes_view_rev = self.state.tab_mutes_view_rev.wrapping_add(1).max(1);
        }
    }

    /// The public ids of every muted tab, in workspace and tab order (the
    /// push).
    pub(crate) fn tab_mute_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                if !tab.muted {
                    continue;
                }
                if let Some(id) = self.public_tab_id(ws_idx, tab_idx) {
                    ids.push(id);
                }
            }
        }
        ids
    }
}

impl AppState {
    /// Restored mutes (session restore, live handoff) reach clients on the
    /// first pass: revision 0 means "never had a mute".
    pub(crate) fn note_restored_mutes(&mut self) {
        if self.tab_mutes_view_rev == 0
            && self
                .workspaces
                .iter()
                .any(|workspace| workspace.tabs.iter().any(|tab| tab.muted))
        {
            self.tab_mutes_view_rev = 1;
        }
    }

    /// Whether the tab holding `pane_id` has its notifications muted. Free
    /// (one compare) while no tab was ever muted; otherwise one pass over
    /// the tabs, only on a notification-raising event.
    pub(crate) fn pane_notifications_muted(&self, pane_id: PaneId) -> bool {
        self.tab_mutes_view_rev > 0
            && self.workspaces.iter().any(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .any(|tab| tab.muted && tab.panes.contains_key(&pane_id))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        Method, PaneMoveDestination, PaneMoveParams, Request, SuccessResponse, TabInfo,
    };
    use crate::config::Config;
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
        app.state.workspaces = vec![Workspace::test_new("bucket"), Workspace::test_new("group")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    fn set_muted(app: &mut App, tab_id: &str, muted: bool) -> String {
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::TabSetMuted(TabSetMutedParams {
                tab_id: tab_id.into(),
                muted,
            }),
        })
    }

    fn tab_info(response: &str) -> TabInfo {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("expected tab info, got {response}");
        };
        tab
    }

    #[test]
    fn set_muted_round_trips_persists_and_is_reported() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        assert_eq!(app.state.tab_mutes_view_rev, 0);
        assert!(!app.state.pane_notifications_muted(pane));

        let tab = tab_info(&set_muted(&mut app, &tab_id, true));
        assert!(tab.muted);
        assert!(app.state.workspaces[0].tabs[0].muted);
        assert_eq!(serde_json::to_value(&tab).unwrap()["muted"], true);
        assert_eq!(app.state.tab_mutes_view_rev, 1);
        assert_eq!(app.tab_mute_ids(), vec![tab_id.clone()]);
        assert!(app.state.pane_notifications_muted(pane));
        let other = app.state.workspaces[1].tabs[0].root_pane;
        assert!(!app.state.pane_notifications_muted(other));

        // The same value again changes nothing.
        set_muted(&mut app, &tab_id, true);
        assert_eq!(app.state.tab_mutes_view_rev, 1);

        let tab = tab_info(&set_muted(&mut app, &tab_id, false));
        assert!(!tab.muted);
        // Absent, not false, when not muted.
        assert!(serde_json::to_value(&tab).unwrap().get("muted").is_none());
        assert_eq!(app.state.tab_mutes_view_rev, 2);
        assert!(app.tab_mute_ids().is_empty());
        assert!(!app.state.pane_notifications_muted(pane));
    }

    #[test]
    fn unknown_tab_is_tab_not_found() {
        let mut app = app();
        let response = set_muted(&mut app, "t_missing_9", true);
        assert!(response.contains("tab_not_found"), "{response}");
        assert_eq!(app.state.tab_mutes_view_rev, 0);
    }

    #[test]
    fn mute_survives_a_whole_tab_pane_move() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_muted(&mut app, &tab_id, true);
        let before = app.state.tab_mutes_view_rev;
        let group_id = app.public_workspace_id(1);
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let response = app.handle_api_request(Request {
            id: "move".into(),
            method: Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(group_id),
                    label: Some("moved".into()),
                },
                focus: true,
            }),
        });
        assert!(response.contains("\"changed\":true"), "{response}");
        let moved = app.state.workspaces[1]
            .tabs
            .iter()
            .find(|tab| tab.custom_name.as_deref() == Some("moved"))
            .expect("moved tab in the group");
        assert!(moved.muted, "the mute moved with the tab");
        assert!(
            !app.state.workspaces[0].tabs.iter().any(|tab| tab.muted),
            "the tab left behind is not muted"
        );
        assert_eq!(app.tab_mute_ids().len(), 1);
        assert!(
            app.state.pane_notifications_muted(pane),
            "the moved pane is still muted"
        );
        assert!(
            app.state.tab_mutes_view_rev > before,
            "the move re-sends the list (the public id changed)"
        );
    }

    #[test]
    fn a_move_bumps_the_mutes_revision_once_any_mute_existed() {
        let mut app = app();
        app.note_tab_mutes_event(&EventKind::TabMoved);
        assert_eq!(app.state.tab_mutes_view_rev, 0, "nothing before a mute");
        let tab_id = app.public_tab_id(0, 0).unwrap();
        set_muted(&mut app, &tab_id, true);
        let before = app.state.tab_mutes_view_rev;
        app.note_tab_mutes_event(&EventKind::TabMoved);
        assert_eq!(app.state.tab_mutes_view_rev, before + 1);
        app.note_tab_mutes_event(&EventKind::PaneFocused);
        assert_eq!(
            app.state.tab_mutes_view_rev,
            before + 1,
            "focus is not a move"
        );
    }

    #[test]
    fn restored_mutes_reach_clients_on_the_first_pass() {
        let mut state = AppState::test_new();
        state.note_restored_mutes();
        assert_eq!(state.tab_mutes_view_rev, 0, "nothing muted");
        state.workspaces = vec![Workspace::test_new("bucket")];
        state.workspaces[0].tabs[0].muted = true;
        state.note_restored_mutes();
        assert_eq!(state.tab_mutes_view_rev, 1);
        let pane = state.workspaces[0].tabs[0].root_pane;
        assert!(state.pane_notifications_muted(pane), "restored mutes gate");
        state.tab_mutes_view_rev = 5;
        state.note_restored_mutes();
        assert_eq!(state.tab_mutes_view_rev, 5, "a live revision is kept");
    }
}
