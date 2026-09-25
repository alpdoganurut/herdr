//! `tab.set_remind`: the idle reminder mark on a tab.
//!
//! The mark is shared session state (server-owned, persisted in session.json
//! with the tab, carried by a whole-tab `pane.move`, like the color tag).
//! The reminders themselves are timed and raised by clients.

use crate::api::schema::{ResponseResult, TabSetRemindParams};

use super::api::responses::{encode_error, encode_success};
use super::App;

impl App {
    pub(super) fn handle_tab_set_remind(
        &mut self,
        id: String,
        params: TabSetRemindParams,
    ) -> String {
        let Some((ws_idx, tab_idx)) = self.parse_tab_id(&params.tab_id) else {
            return encode_error(
                id,
                "tab_not_found",
                format!("tab {} not found", params.tab_id),
            );
        };
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return encode_error(
                id,
                "tab_not_found",
                format!("tab {} not found", params.tab_id),
            );
        };
        if tab.remind != params.remind {
            tab.remind = params.remind;
            tracing::info!(
                event = "tab.set_remind",
                tab_id = %params.tab_id,
                remind = params.remind,
                "tab idle reminder mark set"
            );
            self.schedule_session_save();
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(
                id,
                "tab_not_found",
                format!("tab {} not found", params.tab_id),
            );
        };
        encode_success(id, ResponseResult::TabInfo { tab })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{PaneMoveDestination, PaneMoveParams, SuccessResponse};
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

    fn set_remind(app: &mut App, tab_id: &str, remind: bool) -> String {
        app.handle_tab_set_remind(
            "req".into(),
            TabSetRemindParams {
                tab_id: tab_id.into(),
                remind,
            },
        )
    }

    fn tab_info(response: &str) -> crate::api::schema::TabInfo {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("expected tab info, got {response}");
        };
        tab
    }

    #[test]
    fn set_remind_marks_the_tab_and_false_clears_it() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let tab = tab_info(&set_remind(&mut app, &tab_id, true));
        assert!(tab.remind);
        assert!(app.state.workspaces[0].tabs[0].remind);
        let json = serde_json::to_value(&tab).unwrap();
        assert_eq!(json["remind"], true);

        let tab = tab_info(&set_remind(&mut app, &tab_id, false));
        assert!(!tab.remind);
        assert!(!app.state.workspaces[0].tabs[0].remind);
        // Absent, not false, when the tab is unmarked.
        let json = serde_json::to_value(&tab).unwrap();
        assert!(json.get("remind").is_none());
    }

    #[test]
    fn set_remind_rejects_unknown_tabs() {
        let mut app = app();
        let response = set_remind(&mut app, "t_missing_9", true);
        assert!(response.contains("tab_not_found"), "{response}");
    }

    #[test]
    fn set_remind_is_dispatched_by_the_api() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::TabSetRemind(TabSetRemindParams {
                tab_id,
                remind: true,
            }),
        });
        assert!(tab_info(&response).remind, "{response}");
    }

    fn move_first_tab(app: &mut App, destination: PaneMoveDestination) -> String {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneMove(PaneMoveParams {
                pane_id,
                destination,
                focus: true,
            }),
        })
    }

    #[test]
    fn mark_survives_a_whole_tab_move_to_another_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_remind(&mut app, &tab_id, true);
        let group_id = app.public_workspace_id(1);

        let response = move_first_tab(
            &mut app,
            PaneMoveDestination::NewTab {
                workspace_id: Some(group_id),
                label: Some("moved".into()),
            },
        );

        assert!(response.contains("\"changed\":true"), "{response}");
        let moved = app.state.workspaces[1]
            .tabs
            .iter()
            .find(|tab| tab.custom_name.as_deref() == Some("moved"))
            .expect("moved tab in the group");
        assert!(moved.remind);
        assert!(app.state.workspaces[0].tabs.iter().all(|tab| !tab.remind));
    }

    #[test]
    fn mark_survives_a_whole_tab_move_into_a_new_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_remind(&mut app, &tab_id, true);

        let response = move_first_tab(
            &mut app,
            PaneMoveDestination::NewWorkspace {
                label: Some("fresh".into()),
                tab_label: Some("moved".into()),
            },
        );

        assert!(response.contains("\"changed\":true"), "{response}");
        assert!(app.state.workspaces.last().unwrap().tabs[0].remind);
    }

    #[test]
    fn a_pane_split_out_of_a_marked_tab_starts_unmarked() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let split = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        set_remind(&mut app, &tab_id, true);
        let pane_id = app.public_pane_id(0, split).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: None,
                },
                focus: false,
            }),
        });

        assert!(response.contains("\"changed\":true"), "{response}");
        assert!(app.state.workspaces[0].tabs[0].remind);
        assert!(!app.state.workspaces[0].tabs[1].remind);
    }
}
