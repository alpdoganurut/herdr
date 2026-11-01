//! Fork: `tab.set_dnd`, a tab's do-not-disturb.
//!
//! While a tab is in do-not-disturb, other agents cannot reach its agents:
//! every agent message to them is stored quietly (src/app/quiet_messages.rs;
//! the sender is answered "in DND, queued" at once), an urgent message is
//! refused (`dnd`), the queue types nothing in (a herdr notice, an older
//! sender's prompt and messages queued before wait) and herdr's coordinator
//! wake-ups are held. The user typing into the tab is unaffected, and the
//! coordinator does not break through. Turning it on moves the agent
//! messages already queued for the tab into the quiet store. Not a mute:
//! notifications are untouched (`tab.set_muted` is that).
//!
//! Shared session state like the mute: server-owned, persisted in
//! session.json with the tab, carried by a whole-tab `pane.move`, a live
//! handoff and close/reopen. Clients learn the tabs from the
//! `endpoint.quiet.v1` push (src/server/headless/quiet.rs), not from the
//! bincode shell snapshot, so `PROTOCOL_VERSION` is unchanged.

use crate::api::schema::{ResponseResult, TabSetDndParams};

use super::api::responses::{encode_error, encode_success};
use super::App;

/// Why the queue holds a message for a tab in do-not-disturb.
pub(crate) const DND_WAIT_REASON: &str = "do not disturb";

impl App {
    pub(super) fn handle_tab_set_dnd(&mut self, id: String, params: TabSetDndParams) -> String {
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
        if tab.dnd != params.dnd {
            tab.dnd = params.dnd;
            tracing::info!(
                event = "tab.set_dnd",
                tab_id = %tab_id,
                dnd = params.dnd,
                "tab do-not-disturb set"
            );
            self.schedule_session_save();
            self.bump_quiet_view();
            if params.dnd {
                self.move_queued_into_quiet(ws_idx, tab_idx);
            } else {
                // Held queue entries (notices, older senders) may go now.
                self.message_queue.mark_due();
            }
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    /// The public ids of every tab in do-not-disturb, in workspace and tab
    /// order (the push).
    pub(crate) fn tab_dnd_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                if !tab.dnd {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request, TabInfo};
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
        app.state.workspaces = vec![Workspace::test_new("dnd")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    fn set_dnd(app: &mut App, tab_id: &str, dnd: bool) -> String {
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::TabSetDnd(TabSetDndParams {
                tab_id: tab_id.into(),
                dnd,
            }),
        })
    }

    fn tab_info(response: &str) -> TabInfo {
        let value: serde_json::Value = serde_json::from_str(response).unwrap();
        serde_json::from_value(value["result"]["tab"].clone()).unwrap()
    }

    #[test]
    fn set_dnd_round_trips_persists_and_is_pushed() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        assert_eq!(app.quiet.view_rev, 0);
        assert!(app.tab_dnd_ids().is_empty());

        let tab = tab_info(&set_dnd(&mut app, &tab_id, true));
        assert!(tab.dnd);
        assert!(app.state.workspaces[0].tabs[0].dnd);
        assert_eq!(app.tab_dnd_ids(), vec![tab_id.clone()]);
        assert_eq!(app.quiet.view_rev, 1);
        // The same value again changes nothing.
        set_dnd(&mut app, &tab_id, true);
        assert_eq!(app.quiet.view_rev, 1);

        let tab = tab_info(&set_dnd(&mut app, &tab_id, false));
        assert!(!tab.dnd);
        assert!(app.tab_dnd_ids().is_empty());
        assert_eq!(app.quiet.view_rev, 2);
        let off = serde_json::to_value(&tab).unwrap();
        assert!(off.get("dnd").is_none(), "absent when off: {off}");
    }

    #[test]
    fn an_unknown_tab_is_refused() {
        let mut app = app();
        let response = set_dnd(&mut app, "t_missing_9", true);
        assert!(response.contains("tab_not_found"), "{response}");
        assert_eq!(app.quiet.view_rev, 0);
    }
}
