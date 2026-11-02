//! Fork: `tab.set_dnd`, a tab's do-not-disturb.
//!
//! While a tab is in do-not-disturb, other agents cannot reach its agents:
//! every agent message to them is stored quietly (src/app/quiet_messages.rs;
//! the sender is answered "in DND, queued" at once), an urgent message is
//! refused (`dnd`), the queue types nothing in (a herdr notice, an older
//! sender's prompt and messages queued before wait) and herdr's coordinator
//! wake-ups are held. The user typing into the tab is unaffected. The
//! coordinator does not break through the user's do-not-disturb; one an
//! agent or the coordinator turned on (`agents.set_quiet`,
//! `Tab.dnd_by_agent`) lets the coordinator's messages in as without it
//! (urgent ones too) and holds everyone else's. Turning it on moves the
//! agent messages already queued for the tab into the quiet store (not the
//! coordinator's when an agent turned it on). Not a mute: notifications
//! are untouched (`tab.set_muted` is that).
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
        if !self.set_tab_dnd(ws_idx, tab_idx, params.dnd, false) {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        }
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
        };
        encode_success(id, ResponseResult::TabInfo { tab })
    }

    /// Turn the tab's do-not-disturb on or off; `by_agent` when an agent or
    /// the coordinator asks (`agents.set_quiet`), not the user. Records who
    /// turned it on (`Tab.dnd_by_agent`): an agent asking again keeps a
    /// user's do-not-disturb the user's; the user asking again makes an
    /// agent's the user's (the coordinator's queued messages then move into
    /// the quiet store too). `false` when the tab is gone.
    pub(crate) fn set_tab_dnd(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        dnd: bool,
        by_agent: bool,
    ) -> bool {
        let tab_id = self.public_tab_id(ws_idx, tab_idx).unwrap_or_default();
        let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        else {
            return false;
        };
        let was_on = tab.dnd;
        let was_by_agent = tab.dnd_by_agent;
        tab.dnd_by_agent = if !dnd {
            false
        } else if was_on {
            was_by_agent && by_agent
        } else {
            by_agent
        };
        let claimed_by_user = was_on && dnd && was_by_agent && !tab.dnd_by_agent;
        if was_on != dnd {
            tab.dnd = dnd;
            tracing::info!(
                event = "tab.set_dnd",
                tab_id = %tab_id,
                dnd,
                by_agent,
                "tab do-not-disturb set"
            );
            self.schedule_session_save();
            self.bump_quiet_view();
            if dnd {
                self.move_queued_into_quiet(ws_idx, tab_idx);
            } else {
                // What it stored for agents that do not take quiet messages
                // goes back to the typing queue; held queue entries
                // (notices, older senders) may go now.
                self.release_dnd_quiet(ws_idx, tab_idx);
                self.message_queue.mark_due();
            }
        } else if claimed_by_user {
            self.schedule_session_save();
            self.move_queued_into_quiet(ws_idx, tab_idx);
        }
        true
    }

    /// Whether `pane_id` of workspace `ws_idx` sits in a tab whose
    /// do-not-disturb holds what the coordinator sends (`from_coordinator`)
    /// or what anyone else sends: every do-not-disturb holds the others; the
    /// coordinator gets through one an agent turned on (`Tab.dnd_by_agent`),
    /// never the user's.
    pub(crate) fn pane_dnd_holds(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        from_coordinator: bool,
    ) -> bool {
        self.state.workspaces.get(ws_idx).is_some_and(|workspace| {
            workspace.tabs.iter().any(|tab| {
                tab.dnd
                    && tab.panes.contains_key(&pane_id)
                    && !(from_coordinator && tab.dnd_by_agent)
            })
        })
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
    use crate::api::schema::{Method, PaneMoveDestination, PaneMoveParams, Request, TabInfo};
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

    #[test]
    fn who_turned_it_on_is_recorded_and_the_user_takes_it_over() {
        let mut app = app();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let by_agent = |app: &App| app.state.workspaces[0].tabs[0].dnd_by_agent;

        assert!(app.set_tab_dnd(0, 0, true, true));
        assert!(app.state.workspaces[0].tabs[0].dnd && by_agent(&app));
        // The user asking again makes it the user's.
        set_dnd(&mut app, &tab_id, true);
        assert!(!by_agent(&app));
        // An agent asking again keeps it the user's.
        assert!(app.set_tab_dnd(0, 0, true, true));
        assert!(!by_agent(&app));
        // Off clears it, also when an agent turned it on.
        assert!(app.set_tab_dnd(0, 0, false, true));
        assert!(app.set_tab_dnd(0, 0, true, true));
        assert!(by_agent(&app));
        set_dnd(&mut app, &tab_id, false);
        assert!(!app.state.workspaces[0].tabs[0].dnd && !by_agent(&app));
        // The user turning it on is the user's.
        set_dnd(&mut app, &tab_id, true);
        assert!(!by_agent(&app));
        assert!(!app.set_tab_dnd(0, 9, true, true), "a missing tab");
    }

    #[test]
    fn an_agents_dnd_survives_a_whole_tab_pane_move() {
        let mut app = app();
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        app.state.workspaces.push(Workspace::test_new("other"));
        app.state.ensure_test_terminals();
        assert!(app.set_tab_dnd(0, 0, true, true));
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
        assert!(moved.dnd && moved.dnd_by_agent, "carried with the tab");
        assert!(
            !app.state.workspaces[0]
                .tabs
                .iter()
                .any(|tab| tab.dnd || tab.dnd_by_agent),
            "the tab left behind is not in do-not-disturb"
        );
    }
}
