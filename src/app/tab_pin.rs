//! Fork (sidebar v3): `tab.set_pinned`, a tab's pin.
//!
//! A pinned tab is listed in clients' Pinned section (the tabs sidebar) and
//! stays in its group. The pin is shared session state (server-owned,
//! persisted in session.json with the tab, carried by a whole-tab
//! `pane.move`, a live handoff and close/reopen), like `important` and
//! `remind_every`.
//!
//! Clients learn the pinned tab ids from the `endpoint.tab-pins.v1` push
//! (`src/server/headless/tab_pins.rs`), not from the bincode shell
//! snapshot, so `PROTOCOL_VERSION` is unchanged. The push is a revision
//! compare in the render pass (`AppState::tab_pins_view_rev`), 0 until the
//! first pin.

use crate::api::schema::{EventKind, ResponseResult, TabSetPinnedParams};

use super::api::responses::{encode_error, encode_success};
use super::state::AppState;
use super::App;

impl App {
    pub(super) fn handle_tab_set_pinned(
        &mut self,
        id: String,
        params: TabSetPinnedParams,
    ) -> String {
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
        if tab.pinned != params.pinned {
            tab.pinned = params.pinned;
            tracing::info!(
                event = "tab.set_pinned",
                tab_id = %tab_id,
                pinned = params.pinned,
                "tab pin set"
            );
            self.schedule_session_save();
            self.bump_tab_pins_view();
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    /// The pins changed: clients get a fresh push.
    pub(crate) fn bump_tab_pins_view(&mut self) {
        self.state.tab_pins_view_rev = self.state.tab_pins_view_rev.wrapping_add(1).max(1);
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    /// Moves and closes can change a tab's public id or drop it: once any
    /// pin existed, clients get the list again. O(1).
    pub(crate) fn note_tab_pins_event(&mut self, event: &EventKind) {
        if self.state.tab_pins_view_rev > 0
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
            self.state.tab_pins_view_rev = self.state.tab_pins_view_rev.wrapping_add(1).max(1);
        }
    }

    /// The public ids of every pinned tab, in workspace and tab order (the
    /// push).
    pub(crate) fn tab_pin_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                if !tab.pinned {
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
    /// Restored pins (session restore, live handoff) reach clients on the
    /// first pass: revision 0 means "never had a pin".
    pub(crate) fn note_restored_pins(&mut self) {
        if self.tab_pins_view_rev == 0
            && self
                .workspaces
                .iter()
                .any(|workspace| workspace.tabs.iter().any(|tab| tab.pinned))
        {
            self.tab_pins_view_rev = 1;
        }
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

    fn set_pinned(app: &mut App, tab_id: &str, pinned: bool) -> String {
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::TabSetPinned(TabSetPinnedParams {
                tab_id: tab_id.into(),
                pinned,
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
    fn set_pinned_round_trips_persists_and_is_reported() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        assert_eq!(app.state.tab_pins_view_rev, 0);

        let tab = tab_info(&set_pinned(&mut app, &tab_id, true));
        assert!(tab.pinned);
        assert!(app.state.workspaces[0].tabs[0].pinned);
        assert_eq!(serde_json::to_value(&tab).unwrap()["pinned"], true);
        assert_eq!(app.state.tab_pins_view_rev, 1);
        assert_eq!(app.tab_pin_ids(), vec![tab_id.clone()]);

        // The same value again changes nothing.
        set_pinned(&mut app, &tab_id, true);
        assert_eq!(app.state.tab_pins_view_rev, 1);

        let tab = tab_info(&set_pinned(&mut app, &tab_id, false));
        assert!(!tab.pinned);
        // Absent, not false, when unpinned.
        assert!(serde_json::to_value(&tab).unwrap().get("pinned").is_none());
        assert_eq!(app.state.tab_pins_view_rev, 2);
        assert!(app.tab_pin_ids().is_empty());
    }

    #[test]
    fn unknown_tab_is_tab_not_found() {
        let mut app = app();
        let response = set_pinned(&mut app, "t_missing_9", true);
        assert!(response.contains("tab_not_found"), "{response}");
        assert_eq!(app.state.tab_pins_view_rev, 0);
    }

    #[test]
    fn pin_survives_a_whole_tab_pane_move() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_pinned(&mut app, &tab_id, true);
        let before = app.state.tab_pins_view_rev;
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
        assert!(moved.pinned, "the pin moved with the tab");
        assert!(
            !app.state.workspaces[0].tabs.iter().any(|tab| tab.pinned),
            "the tab left behind is not pinned"
        );
        assert_eq!(app.tab_pin_ids().len(), 1);
        assert!(
            app.state.tab_pins_view_rev > before,
            "the move re-sends the list (the public id changed)"
        );
    }

    #[test]
    fn a_move_bumps_the_pins_revision_once_any_pin_existed() {
        let mut app = app();
        app.note_tab_pins_event(&EventKind::TabMoved);
        assert_eq!(app.state.tab_pins_view_rev, 0, "nothing before a pin");
        let tab_id = app.public_tab_id(0, 0).unwrap();
        set_pinned(&mut app, &tab_id, true);
        let before = app.state.tab_pins_view_rev;
        app.note_tab_pins_event(&EventKind::TabMoved);
        assert_eq!(app.state.tab_pins_view_rev, before + 1);
        app.note_tab_pins_event(&EventKind::PaneFocused);
        assert_eq!(
            app.state.tab_pins_view_rev,
            before + 1,
            "focus is not a move"
        );
    }

    #[test]
    fn restored_pins_reach_clients_on_the_first_pass() {
        let mut state = AppState::test_new();
        state.note_restored_pins();
        assert_eq!(state.tab_pins_view_rev, 0, "nothing pinned");
        state.workspaces = vec![Workspace::test_new("bucket")];
        state.workspaces[0].tabs[0].pinned = true;
        state.note_restored_pins();
        assert_eq!(state.tab_pins_view_rev, 1);
        state.tab_pins_view_rev = 5;
        state.note_restored_pins();
        assert_eq!(state.tab_pins_view_rev, 5, "a live revision is kept");
    }
}
