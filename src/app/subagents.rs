//! `pane.report_subagent`: Claude Code subagents under a pane's agent,
//! reported by the Claude hook asset: `start` (SubagentStart) adds one,
//! `stop` (SubagentStop) removes one, and `snapshot` (the main agent's Stop,
//! from its `background_tasks`) replaces the set with every subagent still
//! running, background ones included.
//!
//! The set is a runtime fact on the pane's terminal
//! (`TerminalState::record_subagent`, `replace_subagents`), never persisted.
//! It outlives the main turn: agent records carry the count whatever the
//! agent's status (`AgentInfo.subagents`, 0 while suspended), clients show
//! it, and suspend/restart refuse while it is not zero. It never makes the
//! agent `working`: an agent whose own turn ended is idle while its
//! background subagents run.

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
        let valid =
            |subagent_id: &str| !subagent_id.is_empty() && subagent_id.len() <= MAX_SUBAGENT_ID_LEN;
        let subagent_id = params.subagent_id.trim();
        if params.event != SubagentEvent::Snapshot && !valid(subagent_id) {
            return encode_error(
                id,
                "invalid_subagent_id",
                format!("subagent_id must be 1 to {MAX_SUBAGENT_ID_LEN} bytes"),
            );
        }
        let snapshot_ids: Vec<&str> = params.subagent_ids.iter().map(|id| id.trim()).collect();
        if snapshot_ids.iter().any(|subagent_id| !valid(subagent_id)) {
            return encode_error(
                id,
                "invalid_subagent_id",
                format!("every subagent id must be 1 to {MAX_SUBAGENT_ID_LEN} bytes"),
            );
        }
        let Some(agent) = crate::detect::parse_agent_label(params.agent.trim()) else {
            return encode_error(id, "invalid_agent", "unknown agent label");
        };
        if self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .is_none()
        {
            return encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            );
        }
        // Through the effective state, so a report that also clears the set
        // (another agent, another session) gets the usual event handling.
        // The count never changes the agent's status: background subagents
        // are not a turn.
        let previous_toast = self.state.toast.clone();
        let event = match params.event {
            SubagentEvent::Start => Some((true, subagent_id)),
            SubagentEvent::Stop => Some((false, subagent_id)),
            SubagentEvent::Snapshot => None,
        };
        let update = self.state.update_terminal_state(pane_id, |terminal| {
            // A late report from an agent the pane no longer runs is dropped.
            if terminal.effective_known_agent() != Some(agent) {
                return None;
            }
            terminal.report_subagents_with_mutation(event, snapshot_ids)
        });
        if let Some(update) = update {
            self.refresh_new_herdr_toast_context_for_update(&update, &previous_toast);
            self.emit_pane_state_update(&update);
        }
        self.sync_toast_deadline(previous_toast);
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
                subagent_ids: Vec::new(),
            }),
        })
    }

    fn snapshot(app: &mut App, ids: &[&str]) -> String {
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        app.handle_api_request(Request {
            id: "req".into(),
            method: Method::PaneReportSubagent(PaneReportSubagentParams {
                pane_id,
                agent: "claude".into(),
                event: SubagentEvent::Snapshot,
                subagent_id: String::new(),
                subagent_ids: ids.iter().map(|id| id.to_string()).collect(),
            }),
        })
    }

    fn detect(app: &mut App, agent: Option<Agent>, state: AgentState) {
        terminal(app).set_detected_state(agent, state);
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
    fn an_idle_agent_keeps_its_subagents_once_it_sends_snapshots() {
        let mut app = working_claude();
        snapshot(&mut app, &[]);
        report(&mut app, SubagentEvent::Start, "a1");
        report(&mut app, SubagentEvent::Start, "a2");
        detect(&mut app, Some(Agent::Claude), AgentState::Blocked);
        detect(&mut app, Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 2);
        // The turn ends with background agents out: the count stays, in
        // any state, and the next turn keeps it.
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 2);
        detect(&mut app, Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 2);
    }

    #[test]
    fn start_while_idle_survives_detection() {
        let mut app = working_claude();
        snapshot(&mut app, &[]);
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        report(&mut app, SubagentEvent::Start, "a1");
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 1);
    }

    #[test]
    fn a_snapshot_replaces_the_set_wholesale() {
        let mut app = working_claude();
        report(&mut app, SubagentEvent::Start, "a1");
        report(&mut app, SubagentEvent::Start, "a2");
        let response = snapshot(&mut app, &["a2", "b1", "b2"]);
        assert!(!response.contains("error"), "{response}");
        assert_eq!(agent_subagents(&app), 3);
        // The server takes the ids as sent (the asset does the filtering).
        snapshot(&mut app, &["internal-helper"]);
        assert_eq!(agent_subagents(&app), 1);
        snapshot(&mut app, &[]);
        assert_eq!(agent_subagents(&app), 0);
        // Capped, and bad ids are rejected whole.
        let many: Vec<String> = (0..100).map(|index| format!("s{index}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        snapshot(&mut app, &many);
        assert_eq!(
            agent_subagents(&app),
            crate::terminal::state::SUBAGENT_LIMIT as u32
        );
        let response = snapshot(&mut app, &["ok", " "]);
        assert!(response.contains("invalid_subagent_id"), "{response}");
        assert_eq!(
            agent_subagents(&app),
            crate::terminal::state::SUBAGENT_LIMIT as u32
        );
    }

    #[test]
    fn before_any_snapshot_an_idle_agent_drops_its_subagents() {
        // A Claude Code without background_tasks on Stop: the old rule.
        let mut app = working_claude();
        report(&mut app, SubagentEvent::Start, "a1");
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 0);
    }

    #[test]
    fn a_transient_unknown_keeps_them_and_a_label_change_clears_them() {
        let mut app = working_claude();
        snapshot(&mut app, &["a1"]);
        detect(&mut app, Some(Agent::Claude), AgentState::Unknown);
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 1, "same label: a blip");
        detect(&mut app, None, AgentState::Unknown);
        detect(&mut app, Some(Agent::Claude), AgentState::Working);
        assert_eq!(agent_subagents(&app), 0, "the agent went away");
        // And the snapshot flag went with it: the old rule again.
        report(&mut app, SubagentEvent::Start, "a2");
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        assert_eq!(agent_subagents(&app), 0);
    }

    #[test]
    fn a_new_session_clears_them() {
        use crate::api::schema::PaneReportAgentSessionParams;
        let mut app = working_claude();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, pane).unwrap();
        let session = |app: &mut App, id: &str, seq: u64, source: &str| {
            app.handle_api_request(Request {
                id: "req".into(),
                method: Method::PaneReportAgentSession(PaneReportAgentSessionParams {
                    pane_id: pane_id.clone(),
                    source: "herdr:claude".into(),
                    agent: "claude".into(),
                    seq: Some(seq),
                    agent_session_id: Some(id.into()),
                    agent_session_path: None,
                    session_start_source: Some(source.into()),
                }),
            })
        };
        session(
            &mut app,
            "0f0e2c7a-1b2c-4d5e-8f90-a1b2c3d4e5f6",
            1,
            "startup",
        );
        snapshot(&mut app, &["a1", "a2"]);
        assert_eq!(agent_subagents(&app), 2);
        // The same session again (a compact) keeps them; /clear starts a
        // new one.
        session(
            &mut app,
            "0f0e2c7a-1b2c-4d5e-8f90-a1b2c3d4e5f6",
            2,
            "compact",
        );
        assert_eq!(agent_subagents(&app), 2);
        let response = session(&mut app, "1a2b3c4d-1b2c-4d5e-8f90-a1b2c3d4e5f6", 3, "clear");
        assert!(!response.contains("error"), "{response}");
        let persisted = terminal(&mut app)
            .persistable_agent_session()
            .map(|session| session.session_ref.value);
        assert_eq!(
            persisted.as_deref(),
            Some("1a2b3c4d-1b2c-4d5e-8f90-a1b2c3d4e5f6"),
            "the new session was taken"
        );
        assert_eq!(agent_subagents(&app), 0);
    }

    /// The Planning session of 2026-09-28: six background agents launched at
    /// 05:34:28, the main turn ending four seconds later with all six still
    /// running, hand-back turns in between, and the last Stop at 05:59:15
    /// reporting none.
    #[test]
    fn replay_of_a_background_agent_session() {
        let mut app = working_claude();
        snapshot(&mut app, &[]);
        let ids = ["b1", "b2", "b3", "b4", "b5", "b6"];
        for id in ids {
            report(&mut app, SubagentEvent::Start, id);
        }
        assert_eq!(agent_subagents(&app), 6);
        // 05:34:33 main Stop: turn ends, snapshot still lists the six.
        detect(&mut app, Some(Agent::Claude), AgentState::Idle);
        snapshot(&mut app, &ids);
        assert_eq!(agent_subagents(&app), 6, "idle with six agents out");
        // Hand-backs: each wakes a short main turn; SubagentStop fires at a
        // subagent's turn ends (it may be resumed), and each Stop snapshot
        // shrinks the list.
        for remaining in (0..6).rev() {
            detect(&mut app, Some(Agent::Claude), AgentState::Working);
            report(&mut app, SubagentEvent::Stop, ids[remaining]);
            detect(&mut app, Some(Agent::Claude), AgentState::Idle);
            snapshot(&mut app, &ids[..remaining]);
            assert_eq!(agent_subagents(&app), remaining as u32);
        }
        // 05:59:15: the final Stop with background_tasks [] -> none.
        assert_eq!(agent_subagents(&app), 0);
    }

    fn root_pane(app: &App) -> crate::layout::PaneId {
        app.state.workspaces[0].tabs[0].root_pane
    }

    /// A detection pass through the app (completion and seen tracking).
    fn observe(app: &mut App, state: AgentState) -> Vec<crate::app::actions::PaneStateUpdate> {
        let pane_id = root_pane(app);
        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id,
                agent: Some(Agent::Claude),
                state,
                visible_blocker: state == AgentState::Blocked,
                visible_working: state == AgentState::Working,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            })
    }

    fn status(app: &App) -> crate::api::schema::AgentStatus {
        app.agent_info(0, root_pane(app))
            .map(|agent| agent.agent_status)
            .expect("agent")
    }

    fn completion(app: &mut App) -> Option<u64> {
        terminal(app).last_agent_completion_seq
    }

    /// A Claude that sends snapshots, working with background agents out.
    fn claude_with_background_agents(ids: &[&str]) -> App {
        let mut app = app();
        observe(&mut app, AgentState::Working);
        snapshot(&mut app, &[]);
        for id in ids {
            report(&mut app, SubagentEvent::Start, id);
        }
        app
    }

    #[test]
    fn background_agents_do_not_keep_the_agent_working_past_its_turn() {
        use crate::api::schema::AgentStatus;
        let mut app = claude_with_background_agents(&["b1", "b2"]);
        // The main turn ends (the prompt is back) with both agents out: the
        // agent finishes now, and the count stays.
        let updates = observe(&mut app, AgentState::Idle);
        assert_eq!(updates.len(), 1, "the turn's own finish: {updates:?}");
        snapshot(&mut app, &["b1", "b2"]);
        assert!(matches!(
            status(&app),
            AgentStatus::Idle | AgentStatus::Done
        ));
        assert_eq!(terminal(&mut app).state, AgentState::Idle);
        assert_eq!(terminal(&mut app).active_subagent_count(), 2);
        let finished = completion(&mut app);
        assert!(finished.is_some(), "finished with the turn");
        // A hand-back turn works and finishes like any other.
        observe(&mut app, AgentState::Working);
        assert_eq!(status(&app), AgentStatus::Working);
        observe(&mut app, AgentState::Idle);
        snapshot(&mut app, &["b2"]);
        assert!(matches!(
            status(&app),
            AgentStatus::Idle | AgentStatus::Done
        ));
        assert_eq!(terminal(&mut app).active_subagent_count(), 1);
    }

    #[test]
    fn the_last_agent_ending_changes_no_status() {
        let mut app = claude_with_background_agents(&["b1"]);
        observe(&mut app, AgentState::Idle);
        snapshot(&mut app, &["b1"]);
        let finished = completion(&mut app);
        assert!(finished.is_some(), "the turn finished");

        // The last agent reports back: the Stop snapshot lists none. No
        // transition and no second completion.
        let pane_id = root_pane(&app);
        let update = app.state.update_terminal_state(pane_id, |terminal| {
            terminal.report_subagents_with_mutation(None, [])
        });
        assert!(update.is_none(), "{update:?}");
        assert_eq!(terminal(&mut app).active_subagent_count(), 0);
        assert_eq!(completion(&mut app), finished, "one completion");
    }

    #[test]
    fn detection_alone_decides_with_live_background_agents() {
        use crate::api::schema::AgentStatus;
        let mut app = claude_with_background_agents(&["b1"]);
        observe(&mut app, AgentState::Blocked);
        assert_eq!(status(&app), AgentStatus::Blocked);
        observe(&mut app, AgentState::Idle);
        assert!(
            matches!(status(&app), AgentStatus::Idle | AgentStatus::Done),
            "idle, not held working"
        );
        assert_eq!(terminal(&mut app).active_subagent_count(), 1);
    }

    #[test]
    fn without_snapshots_the_old_rule_stands() {
        use crate::api::schema::AgentStatus;
        let mut app = app();
        observe(&mut app, AgentState::Working);
        report(&mut app, SubagentEvent::Start, "a1");
        observe(&mut app, AgentState::Idle);
        assert!(matches!(
            status(&app),
            AgentStatus::Idle | AgentStatus::Done
        ));
        assert_eq!(agent_subagents(&app), 0);
        assert!(completion(&mut app).is_some(), "finished at the turn end");
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
                subagent_ids: Vec::new(),
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
