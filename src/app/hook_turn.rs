//! `pane.report_turn`: a turn of the pane's agent starting or ending, as its
//! own hooks see it (the Claude hook asset's `turn` action from
//! UserPromptSubmit, and its `stop` action from Stop).
//!
//! Claude keeps its title spinner after its turn ended while background
//! agents run, so its screen can read `working` for as long as they do. The
//! hooks know better: UserPromptSubmit fires for every turn (a typed prompt,
//! a task notification, a subagent's hand-back) and Stop at its end (not
//! after an Esc interrupt). The reports go into the terminal's turn tracker
//! (`TurnState::note_hook_turn`, runtime only, so a restart or a handoff
//! starts from "unknown"), and message delivery reads them
//! (`TurnState::hook_turn_ended`, src/app/message_queue.rs): a target whose
//! last reported turn ended counts as free whatever its displayed status.
//! The displayed status never reads them; a Claude whose screen says
//! `working` keeps saying so.

use std::time::Instant;

use crate::api::schema::{PaneReportTurnParams, ResponseResult, TurnEvent};

use super::api::responses::{encode_error, encode_success};
use super::App;

/// Longest prompt id kept; Claude's are UUIDs.
const MAX_PROMPT_ID_LEN: usize = 128;

impl App {
    pub(super) fn handle_pane_report_turn(
        &mut self,
        id: String,
        params: PaneReportTurnParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_reported_pane(&params.pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let Some(agent) = crate::detect::parse_agent_label(params.agent.trim()) else {
            return encode_error(id, "invalid_agent", "unknown agent label");
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let prompt = params
            .prompt_id
            .map(|prompt| prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty() && prompt.len() <= MAX_PROMPT_ID_LEN);
        let ended = params.event == TurnEvent::End;
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        // A late report from an agent the pane no longer runs is dropped.
        if terminal.effective_known_agent() != Some(agent) {
            return encode_success(id, ResponseResult::Ok {});
        }
        terminal
            .turn_mut()
            .note_hook_turn(!ended, prompt, params.seq, Instant::now());
        if ended {
            // The status may not change (the title spinner stays): look at
            // the queue now, or a message waits for the next status edge.
            self.message_queue.mark_due();
        }
        encode_success(id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{Method, PaneReportTurnParams, Request, TurnEvent};
    use crate::app::App;
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
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
        app.state.workspaces = vec![Workspace::test_new("turns")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    fn terminal(app: &mut App) -> &mut crate::terminal::TerminalState {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&terminal_id).unwrap()
    }

    fn report(app: &mut App, agent: &str, event: TurnEvent, prompt: &str, seq: u64) -> String {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::PaneReportTurn(PaneReportTurnParams {
                pane_id,
                agent: agent.into(),
                event,
                prompt_id: Some(prompt.into()),
                seq,
            }),
        })
    }

    #[test]
    fn reports_reach_the_turn_tracker_and_never_the_status() {
        let mut app = app();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        let response = report(&mut app, "claude", TurnEvent::Start, "p1", 10);
        assert!(!response.contains("error"), "{response}");
        assert!(terminal(&mut app).turn().hook_turn_ended().is_none());
        report(&mut app, "claude", TurnEvent::End, "p1", 20);
        assert!(terminal(&mut app).turn().hook_turn_ended().is_some());
        assert_eq!(
            terminal(&mut app).state,
            AgentState::Working,
            "delivery only: the displayed status stays"
        );
    }

    #[test]
    fn another_agents_report_and_unknown_panes_are_ignored() {
        let mut app = app();
        terminal(&mut app).set_detected_state(Some(Agent::Codex), AgentState::Working);
        let response = report(&mut app, "claude", TurnEvent::End, "p1", 20);
        assert!(!response.contains("error"), "{response}");
        assert!(terminal(&mut app).turn().hook_turn_ended().is_none());
        let missing = app.handle_api_request(Request {
            id: "req".into(),
            method: Method::PaneReportTurn(PaneReportTurnParams {
                pane_id: "w9:p9".into(),
                agent: "claude".into(),
                event: TurnEvent::End,
                prompt_id: None,
                seq: 0,
            }),
        });
        assert!(missing.contains("pane_not_found"), "{missing}");
    }

    #[test]
    fn the_request_round_trips_and_older_fields_default() {
        let request: Request = serde_json::from_str(
            r#"{"id":"t","method":"pane.report_turn","params":{"pane_id":"w1:p1","agent":"claude","event":"end"}}"#,
        )
        .unwrap();
        let Method::PaneReportTurn(params) = &request.method else {
            panic!("{request:?}");
        };
        assert_eq!(params.event, TurnEvent::End);
        assert_eq!(params.prompt_id, None);
        assert_eq!(params.seq, 0);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["method"], "pane.report_turn");
    }
}
