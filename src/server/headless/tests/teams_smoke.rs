//! Fork smoke tests for teams (`team.*` → `endpoint.teams.v1`). FORK.md
//! section 10 lists them by name; the names carry `fork_smoke` so the sync
//! gate's `-E 'test(fork_smoke)'` runs them.

use super::*;
use crate::api::schema::{
    Method, PaneAgentState, PaneReportAgentParams, Request, TeamMakeParams, TeamSetRoleParams,
};
use crate::detect::{Agent, AgentState};
use crate::server::headless::teams::{TeamsPayload, TEAMS_KIND};

/// The ungrouped bucket and a group `demo` with two tabs, each running a
/// Claude agent.
fn teams_server() -> (HeadlessServer, String, Vec<String>) {
    let mut server = test_headless_server();
    let mut group = crate::workspace::Workspace::test_new("demo");
    group.test_add_tab(None);
    group.switch_tab(0);
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("bucket"), group];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    let mut panes = Vec::new();
    for tab_idx in 0..2 {
        let pane = server.app.state.workspaces[1].tabs[tab_idx].root_pane;
        let terminal_id = server.app.state.workspaces[1].tabs[tab_idx].panes[&pane]
            .attached_terminal_id
            .clone();
        server
            .app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        panes.push(server.app.public_pane_id(1, pane).unwrap());
    }
    let group_id = server.app.state.workspaces[1].id.clone();
    (server, group_id, panes)
}

/// A request from client shell `client_id` (its own user input).
fn client_api(server: &mut HeadlessServer, client_id: u64, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        client_id,
        crate::api::ApiRequestMessage {
            request: Request {
                id: "teams-smoke-client".into(),
                method,
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    );
    serde_json::from_str(&response_rx.recv().expect("client response")).expect("json response")
}

/// A public API request (an agent's CLI or MCP server).
fn public_api(server: &mut HeadlessServer, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "teams-smoke".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    serde_json::from_str(&response_rx.recv().expect("api response")).expect("json response")
}

/// Every team payload in the control stream so far, in order.
fn team_payloads(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<TeamsPayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == TEAMS_KIND {
                payloads.push(TeamsPayload::decode(&data).expect("payload decodes"));
            }
        }
    }
    payloads
}

#[tokio::test]
async fn fork_smoke_team_make_and_roles_reach_every_client_but_status_does_not() {
    let (mut server, group_id, panes) = teams_server();
    let (first, _first_render) = connect_test_shell(&mut server, 51, 80, 23);
    let (second, _second_render) = connect_test_shell(&mut server, 52, 80, 23);
    server.render_and_stream();
    assert!(
        team_payloads(&first).is_empty() && team_payloads(&second).is_empty(),
        "a server without teams sends nothing"
    );

    // The first client makes the team: both clients get it.
    let made = client_api(
        &mut server,
        51,
        Method::TeamMake(TeamMakeParams {
            workspace_id: group_id.clone(),
            purpose: Some("fix the demo greeting".into()),
            caller_pane: None,
        }),
    );
    assert_eq!(made["result"]["type"], "team_reply", "{made}");
    server.render_and_stream();
    for control in [&first, &second] {
        let payloads = team_payloads(control);
        assert_eq!(payloads.len(), 1, "one payload per client");
        let team = &payloads[0].teams[0];
        assert_eq!(team.workspace_id, group_id);
        assert_eq!(team.purpose.as_deref(), Some("fix the demo greeting"));
        assert_eq!(team.members.len(), 2);
        assert!(
            team.members
                .iter()
                .all(|member| member.status.is_none() && member.status_since_unix.is_none()),
            "the push carries no status"
        );
        assert_eq!(payloads[0].boot_id, server.client_shell_boot_id);
    }
    // Nothing changed: no payload.
    server.render_and_stream();
    assert!(team_payloads(&first).is_empty());

    // The second client sets a role: the first follows.
    let role = client_api(
        &mut server,
        52,
        Method::TeamSetRole(TeamSetRoleParams {
            pane_id: panes[1].clone(),
            role: Some("reviewer".into()),
            caller_pane: None,
        }),
    );
    assert_eq!(role["result"]["renamed"], true, "{role}");
    server.render_and_stream();
    let followed = team_payloads(&first);
    assert_eq!(followed.len(), 1);
    let reviewer = followed[0].teams[0]
        .members
        .iter()
        .find(|member| member.pane_id == panes[1])
        .expect("the reviewer");
    assert_eq!(reviewer.role.as_deref(), Some("reviewer"));
    assert_eq!(reviewer.name.as_deref(), Some("reviewer"));
    assert!(followed[0].revision > 0);
    let _ = team_payloads(&second);

    // A member's status flip changes nothing a teams payload carries.
    let reported = public_api(
        &mut server,
        Method::PaneReportAgent(PaneReportAgentParams {
            pane_id: panes[0].clone(),
            source: "custom:teams-smoke".into(),
            agent: "claude".into(),
            state: PaneAgentState::Working,
            message: None,
            seq: Some(1),
            agent_session_id: None,
            agent_session_path: None,
        }),
    );
    assert!(reported.get("error").is_none(), "{reported}");
    let flipped = server.app.team_info(1, true).expect("the team");
    assert_eq!(
        flipped.members[0].status,
        Some(crate::api::schema::AgentStatus::Working),
        "the status did flip"
    );
    server.render_and_stream();
    assert!(
        team_payloads(&first).is_empty() && team_payloads(&second).is_empty(),
        "a status flip sends no teams payload"
    );

    // A client attaching later gets the list on its first pass.
    let (late, _late_render) = connect_test_shell(&mut server, 53, 80, 23);
    server.render_and_stream();
    let seeded = team_payloads(&late);
    assert_eq!(seeded.len(), 1);
    assert_eq!(seeded[0].teams[0].members.len(), 2);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_restored_teams_reach_clients_on_the_first_pass() {
    // A team that came back from session.json or a live handoff: no team
    // event fires, yet clients must be sent it.
    let (mut server, group_id, _panes) = teams_server();
    let mut team = crate::workspace::team::Team::new(Some("fix calendar sync".into()), None, 1);
    let member = server.app.state.workspaces[1].tabs[0].root_pane;
    team.join(member, Some("fixer".into()), 1);
    server.app.state.workspaces[1].team = Some(team);
    server.app.state.teams_view_rev = 0;
    server.app.state.rebuild_team_index();
    server.app.state.assert_invariants_for_test();
    let (client, _render) = connect_test_shell(&mut server, 54, 80, 23);
    server.render_and_stream();
    let payloads = team_payloads(&client);
    assert_eq!(payloads.len(), 1, "the restored team is pushed");
    assert_eq!(payloads[0].teams[0].workspace_id, group_id);
    assert_eq!(
        payloads[0].teams[0].purpose.as_deref(),
        Some("fix calendar sync")
    );
    shutdown_test_runtimes(&mut server);
}
