//! `tab.set_reminder` and `tab.set_remind`: a tab's reminders.
//!
//! Two independent reminders per tab: `important` (clients remind while its
//! agent sits finished, unseen, or blocked) and a scheduled reminder
//! `remind_every` (every 5, 10 or 30 minutes, 1 or 6 hours, or daily).
//! Both are shared session state (server-owned, persisted in session.json
//! with the tab, carried by a whole-tab `pane.move`, like the color tag).
//! The reminders themselves are timed and raised by clients.

use crate::api::schema::{
    ResponseResult, TabRemindEvery, TabRemindInterval, TabSetRemindParams, TabSetReminderParams,
};

use super::api::responses::{encode_error, encode_success};
use super::App;

impl App {
    /// The boolean form of `important`.
    pub(super) fn handle_tab_set_remind(
        &mut self,
        id: String,
        params: TabSetRemindParams,
    ) -> String {
        self.set_tab_reminders(id, &params.tab_id, Some(params.remind), None)
    }

    pub(super) fn handle_tab_set_reminder(
        &mut self,
        id: String,
        params: TabSetReminderParams,
    ) -> String {
        self.set_tab_reminders(
            id,
            &params.tab_id,
            params.important,
            params.every.map(TabRemindEvery::interval),
        )
    }

    /// Apply the given changes (`None` leaves a reminder as it is).
    fn set_tab_reminders(
        &mut self,
        id: String,
        tab_id: &str,
        important: Option<bool>,
        every: Option<Option<TabRemindInterval>>,
    ) -> String {
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
        let important = important.unwrap_or(tab.important);
        let every = every.unwrap_or(tab.remind_every);
        if tab.important != important || tab.remind_every != every {
            tab.important = important;
            tab.remind_every = every;
            tracing::info!(
                event = "tab.set_reminder",
                tab_id = %tab_id,
                important,
                every = every.map_or("off", TabRemindInterval::name),
                "tab reminders set"
            );
            self.schedule_session_save();
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
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

    fn set_reminder(
        app: &mut App,
        tab_id: &str,
        important: Option<bool>,
        every: Option<TabRemindEvery>,
    ) -> String {
        app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::TabSetReminder(TabSetReminderParams {
                tab_id: tab_id.into(),
                important,
                every,
            }),
        })
    }

    fn tab_info(response: &str) -> crate::api::schema::TabInfo {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        let ResponseResult::TabInfo { tab } = success.result else {
            panic!("expected tab info, got {response}");
        };
        tab
    }

    #[test]
    fn set_remind_marks_the_tab_important_and_false_clears_it() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let tab = tab_info(&set_remind(&mut app, &tab_id, true));
        assert!(tab.important);
        assert!(app.state.workspaces[0].tabs[0].important);
        let json = serde_json::to_value(&tab).unwrap();
        assert_eq!(json["important"], true);
        assert!(json.get("remind_every").is_none());

        let tab = tab_info(&set_remind(&mut app, &tab_id, false));
        assert!(!tab.important);
        assert!(!app.state.workspaces[0].tabs[0].important);
        // Absent, not false, when off.
        let json = serde_json::to_value(&tab).unwrap();
        assert!(json.get("important").is_none());
    }

    #[test]
    fn set_reminder_changes_either_reminder_alone() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();

        let tab = tab_info(&set_reminder(
            &mut app,
            &tab_id,
            None,
            Some(TabRemindEvery::Daily),
        ));
        assert!(!tab.important);
        assert_eq!(tab.remind_every, Some(TabRemindInterval::Daily));
        assert_eq!(serde_json::to_value(&tab).unwrap()["remind_every"], "daily");

        let tab = tab_info(&set_reminder(&mut app, &tab_id, Some(true), None));
        assert!(tab.important);
        assert_eq!(tab.remind_every, Some(TabRemindInterval::Daily), "kept");

        let tab = tab_info(&set_reminder(
            &mut app,
            &tab_id,
            None,
            Some(TabRemindEvery::M30),
        ));
        assert!(tab.important, "kept");
        assert_eq!(tab.remind_every, Some(TabRemindInterval::M30));

        let tab = tab_info(&set_reminder(
            &mut app,
            &tab_id,
            Some(false),
            Some(TabRemindEvery::Off),
        ));
        assert!(!tab.important);
        assert_eq!(tab.remind_every, None);
        let json = serde_json::to_value(&tab).unwrap();
        assert!(json.get("remind_every").is_none());

        let response = set_reminder(&mut app, "t_missing_9", Some(true), None);
        assert!(response.contains("tab_not_found"), "{response}");
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
        assert!(tab_info(&response).important, "{response}");
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

    fn mark_both(app: &mut App, tab_id: &str) {
        set_reminder(app, tab_id, Some(true), Some(TabRemindEvery::H6));
    }

    #[test]
    fn reminders_survive_a_whole_tab_move_to_another_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        mark_both(&mut app, &tab_id);
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
        assert!(moved.important);
        assert_eq!(moved.remind_every, Some(TabRemindInterval::H6));
        assert!(app.state.workspaces[0]
            .tabs
            .iter()
            .all(|tab| !tab.important && tab.remind_every.is_none()));
    }

    #[test]
    fn reminders_survive_a_whole_tab_move_into_a_new_group() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        mark_both(&mut app, &tab_id);

        let response = move_first_tab(
            &mut app,
            PaneMoveDestination::NewWorkspace {
                label: Some("fresh".into()),
                tab_label: Some("moved".into()),
            },
        );

        assert!(response.contains("\"changed\":true"), "{response}");
        let created = &app.state.workspaces.last().unwrap().tabs[0];
        assert!(created.important);
        assert_eq!(created.remind_every, Some(TabRemindInterval::H6));
    }

    #[test]
    fn a_pane_split_out_of_a_marked_tab_starts_unmarked() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let split = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        mark_both(&mut app, &tab_id);
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
        assert!(app.state.workspaces[0].tabs[0].important);
        assert!(!app.state.workspaces[0].tabs[1].important);
        assert_eq!(app.state.workspaces[0].tabs[1].remind_every, None);
    }
}
