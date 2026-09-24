//! `tab.set_color`: a named color tag on a tab.
//!
//! The color is shared session state (server-owned, persisted in
//! session.json with the tab, carried by a whole-tab `pane.move`); clients
//! decide how each name is drawn.

use crate::api::schema::{ResponseResult, TabColor, TabSetColorParams};

use super::api::responses::{encode_error, encode_success};
use super::App;

impl App {
    pub(super) fn handle_tab_set_color(&mut self, id: String, params: TabSetColorParams) -> String {
        if params.color == Some(TabColor::Unknown) {
            return encode_error(
                id,
                "invalid_tab_color",
                "unknown tab color; expected red, orange, yellow, green, cyan, blue, purple or null",
            );
        }
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
        if tab.color != params.color {
            tab.color = params.color;
            tracing::info!(
                event = "tab.set_color",
                tab_id = %params.tab_id,
                color = params.color.map_or("none", TabColor::name),
                "tab color set"
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
    use crate::api::schema::{
        PaneMoveDestination, PaneMoveParams, SuccessResponse, TabColor, TabSetColorParams,
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

    fn set_color(app: &mut App, tab_id: &str, color: Option<TabColor>) -> String {
        app.handle_tab_set_color(
            "req".into(),
            TabSetColorParams {
                tab_id: tab_id.into(),
                color,
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
    fn set_color_stores_the_color_and_null_clears_it() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let tab = tab_info(&set_color(&mut app, &tab_id, Some(TabColor::Purple)));
        assert_eq!(tab.color, Some(TabColor::Purple));
        assert_eq!(
            app.state.workspaces[0].tabs[0].color,
            Some(TabColor::Purple)
        );
        let json = serde_json::to_value(&tab).unwrap();
        assert_eq!(json["color"], "purple");

        let tab = tab_info(&set_color(&mut app, &tab_id, None));
        assert_eq!(tab.color, None);
        assert_eq!(app.state.workspaces[0].tabs[0].color, None);
        // Absent, not null, when the tab has no color.
        let json = serde_json::to_value(&tab).unwrap();
        assert!(json.get("color").is_none());
    }

    #[test]
    fn set_color_rejects_unknown_tabs_and_unknown_colors() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let response = set_color(&mut app, "t_missing_9", Some(TabColor::Red));
        assert!(response.contains("tab_not_found"), "{response}");

        let response = set_color(&mut app, &tab_id, Some(TabColor::Unknown));
        assert!(response.contains("invalid_tab_color"), "{response}");
        assert_eq!(app.state.workspaces[0].tabs[0].color, None);
    }

    #[test]
    fn color_survives_a_whole_tab_move_to_another_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        // Keep the bucket non-empty so the move leaves a source behind.
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_color(&mut app, &tab_id, Some(TabColor::Green));
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let group_id = app.public_workspace_id(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneMove(PaneMoveParams {
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
        assert_eq!(moved.color, Some(TabColor::Green));
        assert!(app.state.workspaces[0]
            .tabs
            .iter()
            .all(|tab| tab.color.is_none()));
    }

    #[test]
    fn color_survives_a_whole_tab_move_into_a_new_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        set_color(&mut app, &tab_id, Some(TabColor::Orange));
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("fresh".into()),
                    tab_label: Some("moved".into()),
                },
                focus: true,
            }),
        });

        assert!(response.contains("\"changed\":true"), "{response}");
        let created = app.state.workspaces.last().unwrap();
        assert_eq!(created.tabs[0].color, Some(TabColor::Orange));
    }

    #[test]
    fn a_pane_split_out_of_a_colored_tab_starts_uncolored() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let split = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        set_color(&mut app, &tab_id, Some(TabColor::Blue));
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
        assert_eq!(app.state.workspaces[0].tabs[0].color, Some(TabColor::Blue));
        assert_eq!(app.state.workspaces[0].tabs[1].color, None);
    }
}
