//! Fork smoke tests for the agents model (agents v2). FORK.md section 10
//! lists them by name; the names carry `fork_smoke` so the sync gate's
//! `-E 'test(fork_smoke)'` runs them.

use super::*;
use crate::agents_model::turn::EdgeStatus;
use crate::agents_model::InputSource;
use crate::api::schema::agents_model::{
    AgentsActionOutcome, AgentsCloseTabParams, AgentsSendMessageParams, AgentsSetMetaParams,
};
use crate::api::schema::{Method, Request, TeamMakeParams};
use crate::detect::{Agent, AgentState};
use crate::server::headless::teams::{TeamsPayload, TEAMS_KIND};

/// The bucket, and a team group `crew` with an agent tab (`lead`), an agent
/// tab (`fixer`, with a test runtime that records input) and a shell tab.
fn crew_server() -> (
    HeadlessServer,
    tokio::sync::mpsc::Receiver<Bytes>,
    Vec<crate::layout::PaneId>,
) {
    let mut server = test_headless_server();
    let mut crew = crate::workspace::Workspace::test_new("crew");
    crew.test_add_tab(Some("fixer"));
    crew.test_add_tab(Some("shell"));
    let fixer = crew.tabs[1].root_pane;
    let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
        80, 24, 0, b"FIXER", 4,
    );
    crew.insert_test_runtime(fixer, runtime);
    crew.switch_tab(1);
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("bucket"), crew];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    let panes: Vec<_> = (0..3)
        .map(|tab| server.app.state.workspaces[1].tabs[tab].root_pane)
        .collect();
    for pane in &panes[..2] {
        terminal(&mut server, *pane).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    }
    let workspace_id = server.app.state.workspaces[1].id.clone();
    let made = public_api(
        &mut server,
        Method::TeamMake(TeamMakeParams {
            workspace_id,
            purpose: None,
            caller_pane: None,
        }),
    );
    assert_eq!(made["result"]["type"], "team_reply", "{made}");
    (server, input, panes)
}

fn terminal(
    server: &mut HeadlessServer,
    pane: crate::layout::PaneId,
) -> &mut crate::terminal::TerminalState {
    let id = server.app.state.workspaces[1]
        .pane_state(pane)
        .unwrap()
        .attached_terminal_id
        .clone();
    server.app.state.terminals.get_mut(&id).unwrap()
}

fn public(server: &HeadlessServer, pane: crate::layout::PaneId) -> String {
    server.app.public_pane_id(1, pane).unwrap()
}

fn public_api(server: &mut HeadlessServer, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "agents-model-smoke".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    serde_json::from_str(&response_rx.recv().expect("api response")).expect("json response")
}

fn client_api(server: &mut HeadlessServer, client_id: u64, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        client_id,
        crate::api::ApiRequestMessage {
            request: Request {
                id: "agents-model-smoke-client".into(),
                method,
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    );
    serde_json::from_str(&response_rx.recv().expect("client response")).expect("json response")
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-agents-model-smoke-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn fork_smoke_agents_set_meta_from_a_client_reaches_the_team_push() {
    let (mut server, _input, panes) = crew_server();
    let (control, _render) = connect_test_shell(&mut server, 61, 80, 23);
    server.render_and_stream();
    while control.try_recv().is_ok() {}
    let fixer = public(&server, panes[1]);
    let result = client_api(
        &mut server,
        61,
        Method::AgentsSetMeta(AgentsSetMetaParams {
            caller_pane: None,
            target: fixer,
            role: Some("fixer".into()),
            note: Some("owns the bug".into()),
        }),
    );
    assert_eq!(result["result"]["meta"]["role"], "fixer", "{result}");
    server.render_and_stream();
    let mut roles = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == TEAMS_KIND {
                let payload = TeamsPayload::decode(&data).unwrap();
                roles.extend(
                    payload.teams[0]
                        .members
                        .iter()
                        .filter_map(|member| member.role.clone()),
                );
            }
        }
    }
    assert_eq!(roles, ["fixer"]);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agents_close_of_a_teammate_tab_needs_the_users_turn_and_is_logged() {
    let (mut server, _input, panes) = crew_server();
    let dir = temp_dir("close");
    server.app.agents_model.dir = Some(dir.clone());
    let lead = public(&server, panes[0]);
    let shell_tab = server.app.public_tab_id(1, 2).unwrap();
    let close = |server: &mut HeadlessServer| {
        public_api(
            server,
            Method::AgentsCloseTab(AgentsCloseTabParams {
                caller_pane: lead.clone(),
                target: shell_tab.clone(),
                ..Default::default()
            }),
        )
    };
    let refused = close(&mut server);
    assert_eq!(refused["error"]["code"], "non_user_turn", "{refused}");
    let now = std::time::Instant::now();
    let turn = terminal(&mut server, panes[0]).turn_mut();
    turn.note_input(
        &InputSource::Client {
            submit: true,
            attach: false,
        },
        now,
    );
    turn.on_status_edge(EdgeStatus::Idle, EdgeStatus::Working, false, now, 1);
    let closed = close(&mut server);
    assert_eq!(closed["result"]["close"]["outcome"], "closed", "{closed}");
    assert_eq!(server.app.state.workspaces[1].tabs.len(), 2);
    let log = crate::agents_model::actions_log::read_tail(&dir, 10);
    assert_eq!(log[0].outcome, AgentsActionOutcome::Ok);
    assert_eq!(log[0].action, "close_tab");
    assert_eq!(log[1].outcome, AgentsActionOutcome::Denied);
    // A user action (no caller) is logged as the user's.
    let tab_id = server.app.public_tab_id(1, 1).unwrap();
    let renamed = public_api(
        &mut server,
        Method::TabRename(crate::api::schema::TabRenameParams {
            tab_id,
            label: "fixer-2".into(),
        }),
    );
    assert!(renamed.get("result").is_some(), "{renamed}");
    let log = crate::agents_model::actions_log::read_tail(&dir, 1);
    assert_eq!(log[0].action, "rename_tab");
    assert_eq!(
        log[0].actor,
        crate::api::schema::agents_model::AgentActorKind::User
    );
    let _ = std::fs::remove_dir_all(dir);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_client_typing_in_an_agent_holds_messages_back() {
    let (mut server, mut input, panes) = crew_server();
    let (_control, _render) = connect_test_shell(&mut server, 62, 80, 23);
    let fixer = public(&server, panes[1]);
    server.handle_server_event(ServerEvent::ClientShellPaneInput {
        client_id: 62,
        pane_id: fixer.clone(),
        events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
            "half a thought".into(),
        )],
    });
    assert_eq!(
        input.try_recv().expect("typed into the pane"),
        Bytes::from_static(b"half a thought")
    );
    let lead = public(&server, panes[0]);
    let held = public_api(
        &mut server,
        Method::AgentsSendMessage(AgentsSendMessageParams {
            caller_pane: lead,
            to: fixer,
            text: "status?".into(),
            reply_to: None,
        }),
    );
    // Queued, not refused: typed in once the user is quiet (the queue's
    // typing guard).
    let message = &held["result"]["message"];
    assert_eq!(message["outcome"], "queued", "{held}");
    assert_eq!(message["reason"], "its user is typing in it", "{held}");
    assert!(input.try_recv().is_err(), "nothing typed over the draft");
    shutdown_test_runtimes(&mut server);
}
