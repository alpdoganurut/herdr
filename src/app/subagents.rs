//! `pane.report_subagent`: Claude Code subagents starting and stopping under a
//! pane's agent, reported by the Claude hook asset's SubagentStart and
//! SubagentStop hooks.
//!
//! The set of running subagents is a runtime fact on the pane's terminal
//! (`TerminalState::record_subagent`), never persisted. Agent records carry
//! the count while the agent works (`AgentInfo.subagents`), and clients show it.

use crate::api::schema::{PaneReportSubagentParams, ResponseResult, SubagentEvent};

use super::api::responses::{encode_error, encode_success};
use super::App;

/// Longest subagent id accepted; Claude's ids are short hex strings.
const MAX_SUBAGENT_ID_LEN: usize = 128;

impl App {
    pub(super) fn handle_pane_report_subagent(
        &mut self,
        id: String,
        params: PaneReportSubagentParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        let subagent_id = params.subagent_id.trim();
        if subagent_id.is_empty() || subagent_id.len() > MAX_SUBAGENT_ID_LEN {
            return encode_error(
                id,
                "invalid_subagent_id",
                format!("subagent_id must be 1 to {MAX_SUBAGENT_ID_LEN} bytes"),
            );
        }
        let Some(agent) = crate::detect::parse_agent_label(params.agent.trim()) else {
            return encode_error(id, "invalid_agent", "unknown agent label");
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone());
        let Some(terminal) = terminal_id.and_then(|id| self.state.terminals.get_mut(&id)) else {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        };
        // A late report from an agent the pane no longer runs is dropped.
        if terminal.effective_known_agent() == Some(agent) {
            terminal.record_subagent(params.event == SubagentEvent::Start, subagent_id);
        }
        encode_success(id, ResponseResult::Ok {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request};
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
        app.state.workspaces = vec![Workspace::test_new("subagents")];
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

    fn report(app: &mut App, event: SubagentEvent, subagent_id: &str) -> String {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::PaneReportSubagent(PaneReportSubagentParams {
                pane_id,
                agent: "claude".into(),
                event,
                subagent_id: subagent_id.into(),
            }),
        })
    }

    fn agent_subagents(app: &App) -> u32 {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        app.agent_info(0, pane).map_or(0, |agent| agent.subagents)
    }

    fn working_claude() -> App {
        let mut app = app();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        app
    }

    #[test]
    fn start_and_stop_count_subagents_while_the_agent_works() {
        let mut app = working_claude();
        for id in ["a1", "a2", "a2"] {
            let response = report(&mut app, SubagentEvent::Start, id);
            assert!(!response.contains("error"), "{response}");
        }
        assert_eq!(agent_subagents(&app), 2, "a repeated start counts once");
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let json = serde_json::to_value(app.agent_info(0, pane).unwrap()).unwrap();
        assert_eq!(json["subagents"], 2);

        report(&mut app, SubagentEvent::Stop, "a1");
        report(&mut app, SubagentEvent::Stop, "unknown");
        assert_eq!(agent_subagents(&app), 1);
        report(&mut app, SubagentEvent::Stop, "a2");
        assert_eq!(agent_subagents(&app), 0);
        let json = serde_json::to_value(app.agent_info(0, pane).unwrap()).unwrap();
        assert!(json.get("subagents").is_none(), "absent when zero");
    }

    #[test]
    fn the_set_is_capped() {
        let mut app = working_claude();
        for index in 0..crate::terminal::state::SUBAGENT_LIMIT + 10 {
            report(&mut app, SubagentEvent::Start, &format!("s{index}"));
        }
        assert_eq!(
            agent_subagents(&app),
            crate::terminal::state::SUBAGENT_LIMIT as u32
        );
    }

    #[test]
    fn the_agent_stopping_clears_the_set() {
        let mut app = working_claude();
        report(&mut app, SubagentEvent::Start, "a1");
        report(&mut app, SubagentEvent::Start, "a2");
        // A permission prompt keeps them.
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 2);
        // The turn ends: idle clears, and working again starts from none.
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 0);
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 0);
    }

    #[test]
    fn exit_suspend_and_release_clear_the_set() {
        let mut app = working_claude();
        report(&mut app, SubagentEvent::Start, "a1");
        terminal(&mut app).clear_agent_runtime_identity_after_respawn();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 0, "exit");

        report(&mut app, SubagentEvent::Start, "a1");
        assert_eq!(agent_subagents(&app), 1);
        terminal(&mut app).begin_agent_suspend(
            crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id(
                    "0f0e2c7a-1b2c-4d5e-8f90-a1b2c3d4e5f6",
                )
                .expect("valid session id"),
                transcript_path: None,
            },
            std::time::Instant::now(),
        );
        assert_eq!(agent_subagents(&app), 0, "suspend");

        let mut app = working_claude();
        report(&mut app, SubagentEvent::Start, "a1");
        terminal(&mut app).set_detected_state(None, AgentState::Unknown);
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 0, "agent gone");
    }

    #[test]
    fn reports_for_another_agent_or_bad_ids_change_nothing() {
        let mut app = working_claude();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: Method::PaneReportSubagent(PaneReportSubagentParams {
                pane_id,
                agent: "codex".into(),
                event: SubagentEvent::Start,
                subagent_id: "a1".into(),
            }),
        });
        assert!(!response.contains("error"), "{response}");
        assert_eq!(agent_subagents(&app), 0);

        let response = report(&mut app, SubagentEvent::Start, "  ");
        assert!(response.contains("invalid_subagent_id"), "{response}");
        let response = report(&mut app, SubagentEvent::Start, &"x".repeat(200));
        assert!(response.contains("invalid_subagent_id"), "{response}");
        assert_eq!(agent_subagents(&app), 0);
    }
}
