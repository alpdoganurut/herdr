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
    // An idle agent's free input box (the typing guard fails closed).
    runtime.test_process_pty_bytes(crate::app::typing_guard::TEST_FREE_INPUT_BOX);
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
        peer_pid: None,
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
            peer_pid: None,
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

/// Fork (agent mute): a team member mutes a teammate's tab through the
/// agents path (`agents.set_quiet`, as the herdr_agents agents_mute tool
/// sends it); every client gets the `endpoint.tab-mutes.v1` push naming the
/// tab, the action log names the member, and an outsider is refused.
#[tokio::test]
async fn fork_smoke_agents_mute_of_a_teammate_tab_reaches_the_mutes_push() {
    use crate::api::schema::agents_model::AgentsSetQuietParams;
    use crate::server::headless::tab_mutes::{TabMutesPayload, TAB_MUTES_KIND};
    let (mut server, _input, panes) = crew_server();
    let dir = temp_dir("mute");
    server.app.agents_model.dir = Some(dir.clone());
    let (control, _render) = connect_test_shell(&mut server, 63, 80, 23);
    server.render_and_stream();
    while control.try_recv().is_ok() {}
    let (lead, fixer) = (public(&server, panes[0]), public(&server, panes[1]));
    let fixer_tab = server.app.public_tab_id(1, 1).unwrap();
    let result = public_api(
        &mut server,
        Method::AgentsSetQuiet(AgentsSetQuietParams {
            caller_pane: Some(lead.clone()),
            target: fixer,
            muted: Some(true),
            dnd: Some(true),
        }),
    );
    assert_eq!(result["result"]["tab"]["muted"], true, "{result}");
    assert_eq!(result["result"]["tab"]["dnd"], true, "{result}");
    server.render_and_stream();
    // The push can land a moment after the pass: read until the channel
    // stays quiet.
    let mut pushed = Vec::new();
    while let Ok(bytes) = control.recv_timeout(std::time::Duration::from_millis(200)) {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == TAB_MUTES_KIND {
                pushed.push(TabMutesPayload::decode(&data).unwrap().tab_ids);
            }
        }
    }
    assert_eq!(pushed, [vec![fixer_tab.clone()]], "one push naming the tab");
    let log = crate::agents_model::actions_log::read_tail(&dir, 1);
    assert_eq!(log[0].action, "set_quiet");
    assert_eq!(log[0].outcome, AgentsActionOutcome::Ok);
    assert_eq!(log[0].actor_pane.as_deref(), Some(lead.as_str()));
    assert_eq!(log[0].target_tab.as_deref(), Some(fixer_tab.as_str()));
    // A pane outside the team (the bucket's shell is the user; a plain
    // group's agent is not a teammate).
    let mut plain = crate::workspace::Workspace::test_new("plain");
    plain.test_add_tab(Some("other"));
    server.app.state.workspaces.push(plain);
    server.app.state.ensure_test_terminals();
    let outsider = server.app.state.workspaces[2].tabs[0].root_pane;
    let outsider_id = server.app.state.workspaces[2]
        .pane_state(outsider)
        .unwrap()
        .attached_terminal_id
        .clone();
    server
        .app
        .state
        .terminals
        .get_mut(&outsider_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    let outsider = server.app.public_pane_id(2, outsider).unwrap();
    let refused = public_api(
        &mut server,
        Method::AgentsSetQuiet(AgentsSetQuietParams {
            caller_pane: Some(outsider),
            target: fixer_tab,
            muted: Some(false),
            dnd: None,
        }),
    );
    assert_eq!(refused["error"]["code"], "outside_team", "{refused}");
    assert!(server.app.state.workspaces[1].tabs[1].muted);
    let _ = std::fs::remove_dir_all(dir);
    shutdown_test_runtimes(&mut server);
}

/// Fork (agent mute): a do-not-disturb a team member turned on through
/// `agents.set_quiet` holds other agents' messages but lets the
/// coordinator's (in its user's turn) in as without it; once the user turns
/// it on from a client (`tab.set_dnd`) it holds the coordinator too.
#[tokio::test]
async fn fork_smoke_the_coordinator_gets_through_an_agents_dnd_not_the_users() {
    use crate::api::schema::agents_model::AgentsSetQuietParams;
    let (mut server, mut input, panes) = crew_server();
    let (_control, _render) = connect_test_shell(&mut server, 64, 80, 23);
    server.app.coordinator.dir = temp_dir("dnd-coordinator");
    let coordinator_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let coordinator_terminal = server.app.state.workspaces[0]
        .pane_state(coordinator_pane)
        .unwrap()
        .attached_terminal_id
        .clone();
    server.app.state.coordinator_terminal_id = Some(coordinator_terminal.clone());
    {
        let terminal = server
            .app
            .state
            .terminals
            .get_mut(&coordinator_terminal)
            .unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let now = std::time::Instant::now();
        terminal.turn_mut().note_input(
            &InputSource::Client {
                submit: true,
                attach: false,
            },
            now,
        );
        terminal
            .turn_mut()
            .on_status_edge(EdgeStatus::Idle, EdgeStatus::Working, false, now, 1);
    }
    let coordinator = server.app.public_pane_id(0, coordinator_pane).unwrap();
    let (lead, fixer) = (public(&server, panes[0]), public(&server, panes[1]));
    let fixer_tab = server.app.public_tab_id(1, 1).unwrap();
    let send = |server: &mut HeadlessServer, from: &str, text: &str| {
        public_api(
            server,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: from.to_string(),
                to: fixer.clone(),
                text: text.into(),
                reply_to: None,
                urgent: false,
                quiet: false,
            }),
        )
    };
    let typed = |input: &mut tokio::sync::mpsc::Receiver<Bytes>| {
        let mut out = String::new();
        while let Ok(bytes) = input.try_recv() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    };
    let result = public_api(
        &mut server,
        Method::AgentsSetQuiet(AgentsSetQuietParams {
            caller_pane: Some(lead.clone()),
            target: fixer_tab.clone(),
            muted: None,
            dnd: Some(true),
        }),
    );
    assert_eq!(result["result"]["tab"]["dnd"], true, "{result}");
    assert!(server.app.state.workspaces[1].tabs[1].dnd_by_agent);

    let held = send(&mut server, &lead, "from the lead");
    assert_eq!(held["result"]["message"]["quiet"], true, "{held}");
    let through = send(&mut server, &coordinator, "run the tests");
    assert_eq!(through["result"]["message"]["outcome"], "sent", "{through}");
    let text = typed(&mut input);
    assert!(text.contains("run the tests"), "{text:?}");
    assert!(!text.contains("from the lead"), "{text:?}");

    // The user turns it on from a client: now it is the user's.
    let reply = client_api(
        &mut server,
        64,
        Method::TabSetDnd(crate::api::schema::TabSetDndParams {
            tab_id: fixer_tab,
            dnd: true,
        }),
    );
    assert_eq!(reply["result"]["tab"]["dnd"], true, "{reply}");
    assert!(!server.app.state.workspaces[1].tabs[1].dnd_by_agent);
    server.app.agents_model.limiter = Default::default();
    let held = send(&mut server, &coordinator, "and the lints");
    assert_eq!(held["result"]["message"]["quiet"], true, "{held}");
    assert!(!typed(&mut input).contains("and the lints"));
    let _ = std::fs::remove_dir_all(&server.app.coordinator.dir);
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
            urgent: false,
            quiet: false,
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

/// A Claude whose title spinner keeps it `working` while background agents
/// run after its turn ended: its Stop hook (`pane.report_turn` end) frees it
/// for a queued message after SETTLE, its next UserPromptSubmit holds the
/// next one back, and the displayed status stays `working` throughout.
#[tokio::test]
async fn fork_smoke_turn_hooks_free_a_working_claude_for_messages() {
    use crate::api::schema::{PaneReportTurnParams, TurnEvent};
    let (mut server, mut input, panes) = crew_server();
    server.app.coordinator.assume_shell_ready = true;
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Working);
    let fixer = public(&server, panes[1]);
    let lead = public(&server, panes[0]);
    let report = |server: &mut HeadlessServer, event: TurnEvent, prompt: &str, seq: u64| {
        let fixer = public(server, panes[1]);
        let reply = public_api(
            server,
            Method::PaneReportTurn(PaneReportTurnParams {
                pane_id: fixer,
                agent: "claude".into(),
                event,
                prompt_id: Some(prompt.into()),
                seq,
            }),
        );
        assert!(reply.get("error").is_none(), "{reply}");
    };
    report(&mut server, TurnEvent::Start, "p1", 10);
    let send = |server: &mut HeadlessServer, text: &str| {
        public_api(
            server,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead.clone(),
                to: fixer.clone(),
                text: text.into(),
                reply_to: None,
                urgent: false,
                quiet: false,
            }),
        )
    };
    let held = send(&mut server, "status?");
    assert_eq!(held["result"]["message"]["reason"], "working", "{held}");

    // The turn ends; the title still spins for a background agent.
    report(&mut server, TurnEvent::End, "p1", 20);
    let t0 = std::time::Instant::now();
    let now = crate::coordinator::now_unix();
    assert!(!server.app.message_queue_pass(t0, now), "settling");
    assert!(input.try_recv().is_err(), "nothing typed before SETTLE");
    assert!(server
        .app
        .message_queue_pass(t0 + crate::app::message_queue::SETTLE, now));
    let typed = tokio::time::timeout(std::time::Duration::from_secs(2), input.recv()).await;
    assert!(
        matches!(typed, Ok(Some(_))),
        "typed in after its turn ended"
    );
    assert_eq!(terminal(&mut server, panes[1]).state, AgentState::Working);

    // The message starts a turn (herdr's write, then its UserPromptSubmit):
    // the next message waits for that turn's Stop (the per-pair rate
    // limit is not what this test is about).
    server.app.agents_model.limiter = Default::default();
    let next = send(&mut server, "and now?");
    assert_eq!(next["result"]["message"]["outcome"], "queued", "{next}");
    report(&mut server, TurnEvent::Start, "p2", 30);
    let t1 = t0 + crate::app::message_queue::SETTLE * 3;
    server.app.message_queue.mark_due();
    server.app.message_queue_pass(t1, now);
    server
        .app
        .message_queue_pass(t1 + crate::app::message_queue::SETTLE, now);
    assert_eq!(server.app.message_queue.entries.len(), 1, "held for p2");
    shutdown_test_runtimes(&mut server);
}

/// A message queued 11 minutes to a teammate that reads working while the
/// screen above its prompt box stays the same gets its sender one herdr
/// notice through the same delivery path (once per message, once per
/// target per 10 minutes); `agents.queued` shows its age and reason. A
/// screen that keeps changing above the prompt box (a live turn) never
/// does.
#[tokio::test]
async fn fork_smoke_a_stuck_queued_message_notifies_its_sender_once() {
    use crate::app::message_queue::{STUCK_SAMPLE, STUCK_SCREEN};
    let (mut server, _fixer_input, panes) = crew_server();
    server.app.coordinator.assume_shell_ready = true;
    let screen = "\r\n✻ Waiting\r\n─────\r\n❯ \r\n─────\r\n  footer".as_bytes();
    let (fixer_runtime, _fixer_rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80, 24, 0, screen, 4,
        );
    let fixer_terminal = server.app.state.workspaces[1]
        .pane_state(panes[1])
        .unwrap()
        .attached_terminal_id
        .clone();
    server
        .app
        .terminal_runtimes
        .insert(fixer_terminal, fixer_runtime);
    let (lead_runtime, mut lead_input) =
        crate::terminal::TerminalRuntime::test_with_channel(80, 24);
    // An idle agent's free input box (the typing guard fails closed).
    lead_runtime.test_process_pty_bytes(crate::app::typing_guard::TEST_FREE_INPUT_BOX);
    let lead_terminal = server.app.state.workspaces[1]
        .pane_state(panes[0])
        .unwrap()
        .attached_terminal_id
        .clone();
    server
        .app
        .terminal_runtimes
        .insert(lead_terminal, lead_runtime);
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Working);
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    let sent = public_api(
        &mut server,
        Method::AgentsSendMessage(AgentsSendMessageParams {
            caller_pane: lead.clone(),
            to: fixer,
            text: "status?".into(),
            reply_to: None,
            urgent: false,
            quiet: false,
        }),
    );
    let id = sent["result"]["message"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(sent["result"]["message"]["queue_age_s"], 0, "{sent}");
    // It has waited 11 minutes.
    server.app.message_queue.entries[0].message.unix -= 11 * 60;
    let now = crate::coordinator::now_unix();
    let t0 = std::time::Instant::now();
    server.app.message_queue.mark_due();
    server.app.message_queue_pass(t0, now);
    assert!(
        lead_input.try_recv().is_err(),
        "nothing before the screen held"
    );
    // Sampled every minute; the screen above the box never changed.
    let mut at = t0;
    while at < t0 + STUCK_SCREEN {
        at += STUCK_SAMPLE;
        server.app.message_queue_pass(at, now);
    }
    let notice = tokio::time::timeout(std::time::Duration::from_secs(2), lead_input.recv())
        .await
        .expect("a notice typed into the sender")
        .expect("bytes");
    let notice = String::from_utf8_lossy(&notice).to_string();
    assert!(notice.contains("herdr"), "{notice}");
    assert!(server.app.message_queue.entries[0].stuck_notified);
    let queued = public_api(
        &mut server,
        Method::AgentsQueued(crate::api::schema::agents_model::AgentsQueuedParams {
            caller_pane: lead.clone(),
        }),
    );
    let mine = &queued["result"]["messages"][0];
    assert_eq!(mine["id"], id.as_str(), "{queued}");
    assert_eq!(mine["reason"], "working", "{queued}");
    assert!(mine["age_s"].as_u64().unwrap() >= 11 * 60, "{queued}");
    assert_eq!(mine["notified"], true, "{queued}");

    // Once per message and per target: another 15 minutes bring nothing.
    while lead_input.try_recv().is_ok() {}
    while at < t0 + STUCK_SCREEN * 3 {
        at += STUCK_SAMPLE;
        server.app.message_queue_pass(at, now);
    }
    assert!(lead_input.try_recv().is_err(), "one notice per message");
    shutdown_test_runtimes(&mut server);
}

/// An urgent message from a lead acting in its user's turn interrupts a
/// working teammate (one Esc, logged as `interrupt`) and goes in, marked
/// URGENT, once the interrupted turn reads idle and settled; without the
/// user's turn it is refused.
#[tokio::test]
async fn fork_smoke_urgent_message_interrupts_a_working_teammate() {
    let (mut server, mut input, panes) = crew_server();
    server.app.coordinator.assume_shell_ready = true;
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Working);
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    let urgent = |server: &mut HeadlessServer| {
        public_api(
            server,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead.clone(),
                to: fixer.clone(),
                text: "stop the deploy".into(),
                reply_to: None,
                urgent: true,
                quiet: false,
            }),
        )
    };
    let refused = urgent(&mut server);
    assert_eq!(refused["error"]["code"], "urgent_not_allowed", "{refused}");
    assert!(input.try_recv().is_err());

    // The lead's user asks for it.
    let now = std::time::Instant::now();
    let lead_terminal = terminal(&mut server, panes[0]);
    lead_terminal.turn_mut().note_input(
        &InputSource::Client {
            submit: true,
            attach: false,
        },
        now,
    );
    lead_terminal
        .turn_mut()
        .on_status_edge(EdgeStatus::Idle, EdgeStatus::Working, false, now, 1);
    let sent = urgent(&mut server);
    assert_eq!(sent["result"]["message"]["outcome"], "queued", "{sent}");
    let esc = tokio::time::timeout(std::time::Duration::from_secs(2), input.recv())
        .await
        .expect("an Esc")
        .expect("bytes");
    assert_eq!(&esc[..], b"\x1b");

    // The interrupted turn reads idle: in it goes after SETTLE.
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    server.app.message_queue.mark_due();
    let t0 = std::time::Instant::now();
    let unix = crate::coordinator::now_unix();
    server.app.message_queue_pass(t0, unix);
    assert!(server
        .app
        .message_queue_pass(t0 + crate::app::message_queue::SETTLE, unix));
    let typed = tokio::time::timeout(std::time::Duration::from_secs(2), input.recv())
        .await
        .expect("typed in")
        .expect("bytes");
    let typed = String::from_utf8_lossy(&typed).to_string();
    assert!(typed.contains("URGENT"), "{typed:?}");
    shutdown_test_runtimes(&mut server);
}

/// A wait the lead started in its user's turn (`agents.report_wait`) that
/// outlived the turn (an async MCP task: the pane went idle) makes the turn
/// its end opens the user's: an urgent message from it is allowed, once.
#[tokio::test]
async fn fork_smoke_a_user_turn_wait_carries_urgent_authority_into_its_continuation() {
    use crate::api::schema::agents_model::AgentsReportWaitParams;
    let (mut server, mut input, panes) = crew_server();
    server.app.coordinator.assume_shell_ready = true;
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Working);
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    let urgent = |server: &mut HeadlessServer| {
        public_api(
            server,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead.clone(),
                to: fixer.clone(),
                text: "stop the deploy".into(),
                reply_to: None,
                urgent: true,
                quiet: false,
            }),
        )
    };
    let report = |server: &mut HeadlessServer, started: bool| {
        public_api(
            server,
            Method::AgentsReportWait(AgentsReportWaitParams {
                caller_pane: lead.clone(),
                started,
                seq: 1,
            }),
        )
    };
    // The lead's user asks; the lead starts a wait and its turn ends.
    let t0 = std::time::Instant::now();
    let lead_terminal = terminal(&mut server, panes[0]);
    lead_terminal.turn_mut().note_input(
        &InputSource::Client {
            submit: true,
            attach: false,
        },
        t0,
    );
    lead_terminal
        .turn_mut()
        .on_status_edge(EdgeStatus::Idle, EdgeStatus::Working, false, t0, 1);
    let started = report(&mut server, true);
    assert!(started.get("error").is_none(), "{started}");
    terminal(&mut server, panes[0]).turn_mut().on_status_edge(
        EdgeStatus::Working,
        EdgeStatus::Idle,
        false,
        std::time::Instant::now(),
        2,
    );
    // The wait returns; the lead starts a turn on its own.
    let ended = report(&mut server, false);
    assert!(ended.get("error").is_none(), "{ended}");
    let later = std::time::Instant::now() + std::time::Duration::from_secs(10);
    terminal(&mut server, panes[0]).turn_mut().on_status_edge(
        EdgeStatus::Idle,
        EdgeStatus::Working,
        false,
        later,
        3,
    );
    let sent = urgent(&mut server);
    assert_eq!(sent["result"]["message"]["outcome"], "queued", "{sent}");
    let esc = tokio::time::timeout(std::time::Duration::from_secs(2), input.recv())
        .await
        .expect("an Esc")
        .expect("bytes");
    assert_eq!(&esc[..], b"\x1b");

    // One turn only: the lead's next turn of its own is not its user's.
    let lead_terminal = terminal(&mut server, panes[0]);
    lead_terminal.turn_mut().on_status_edge(
        EdgeStatus::Working,
        EdgeStatus::Idle,
        false,
        later + std::time::Duration::from_secs(10),
        4,
    );
    lead_terminal.turn_mut().on_status_edge(
        EdgeStatus::Idle,
        EdgeStatus::Working,
        false,
        later + std::time::Duration::from_secs(20),
        5,
    );
    let lead_id = terminal(&mut server, panes[0]).id.clone();
    let turn = server.app.effective_turn(&lead_id).expect("a turn");
    assert!(!turn.user_turn(), "{turn:?}");
    shutdown_test_runtimes(&mut server);
}

/// Give a pane a runtime whose process (its shell or agent) is `pid`.
fn set_pane_pid(server: &mut HeadlessServer, pane: crate::layout::PaneId, pid: u32) {
    let id = server.app.state.workspaces[1]
        .pane_state(pane)
        .unwrap()
        .attached_terminal_id
        .clone();
    let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
    runtime.test_set_child_pid(pid);
    server.app.terminal_runtimes.insert(id, runtime);
}

/// A request sent over the socket by process `peer`.
fn api_from(server: &mut HeadlessServer, peer: Option<u32>, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "agents-model-smoke-peer".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        peer_pid: peer,
    });
    assert_eq!(server.app.agents_model.caller_process, None);
    serde_json::from_str(&response_rx.recv().expect("api response")).expect("json response")
}

fn actor(server: &mut HeadlessServer, peer: Option<u32>, caller_pane: &str) -> serde_json::Value {
    api_from(
        server,
        peer,
        Method::AgentsActor(crate::api::schema::agents_model::AgentsActorParams {
            caller_pane: caller_pane.into(),
            ..Default::default()
        }),
    )
}

/// A pid no live process of the test has (nothing in the test's ancestry).
const UNRELATED_PID: u32 = 0x7fff_fff0;

#[tokio::test]
async fn fork_smoke_agents_stale_caller_pane_is_found_by_its_process() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], me);
    let fixer = public(&server, panes[1]);

    // An id that no longer resolves (a tab moved by an older herdr).
    let found = actor(&mut server, Some(me), "w9:p99");
    assert_eq!(
        found["result"]["actor"]["pane_id"],
        fixer.as_str(),
        "{found}"
    );
    assert_eq!(found["result"]["actor"]["kind"], "agent", "{found}");

    // An id that now names another pane: the process wins.
    let lead = public(&server, panes[0]);
    let found = actor(&mut server, Some(me), &lead);
    assert_eq!(
        found["result"]["actor"]["pane_id"],
        fixer.as_str(),
        "{found}"
    );

    // The browser and MCP startup check resolve the same way.
    let resolved = api_from(
        &mut server,
        Some(me),
        Method::BrowserResolveCaller(crate::api::schema::BrowserCaller {
            pane_id: "w9:p99".into(),
        }),
    );
    assert_eq!(
        resolved["result"]["actor"]["pane_id"],
        fixer.as_str(),
        "{resolved}"
    );
    assert_eq!(resolved["result"]["actor"]["shell_pid"], me, "{resolved}");
}

#[tokio::test]
async fn fork_smoke_agents_backgrounded_claude_session_is_found_by_its_session_id() {
    const OLD: &str = "62766cda-e2c3-4fc3-bb20-0a9094158ef2";
    const NEW: &str = "8ac93ea8-c090-43aa-a1e4-58a2abbee34c";
    let project = crate::agent_session_chain::test_support::Project::new("smoke-bg-session");
    project.session_file(OLD, Some(NEW));
    project.session_file(NEW, None);
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], UNRELATED_PID - 1);
    terminal(&mut server, panes[0]).set_persisted_agent_session(project.session(OLD));
    let lead = public(&server, panes[0]);
    let by_session = |server: &mut HeadlessServer, peer: u32| {
        api_from(
            server,
            Some(peer),
            Method::AgentsActor(crate::api::schema::agents_model::AgentsActorParams {
                claude_session: Some(NEW.into()),
                ..Default::default()
            }),
        )
    };

    // Claude's daemon runs the forked session outside every pane: its
    // session's lineage names the lead's pane, as an agent of its team.
    let found = by_session(&mut server, me);
    let actor = &found["result"]["actor"];
    assert_eq!(actor["pane_id"], lead.as_str(), "{found}");
    assert_eq!(actor["kind"], "agent");
    assert_eq!(actor["by_session"], true);
    assert_eq!(actor["team"]["label"], "crew");

    // A process inside another pane cannot claim the lead by its session.
    set_pane_pid(&mut server, panes[1], me);
    let refused = by_session(&mut server, me);
    assert_eq!(refused["error"]["code"], "caller_unresolved", "{refused}");
}

#[tokio::test]
async fn fork_smoke_agents_stale_caller_without_its_process_is_refused() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], UNRELATED_PID + 1);
    let lead = public(&server, panes[0]);

    // No pane runs the caller: refused as before.
    let refused = actor(&mut server, Some(me), "w9:p99");
    assert_eq!(refused["error"]["code"], "caller_unresolved", "{refused}");
    // A resolving id keeps its own pane (the client's ancestry check
    // refuses a process not started there).
    let kept = actor(&mut server, Some(me), &lead);
    assert_eq!(kept["result"]["actor"]["pane_id"], lead.as_str(), "{kept}");

    // No socket peer (a client request, a test): the id alone, as before.
    set_pane_pid(&mut server, panes[1], me);
    let refused = actor(&mut server, None, "w9:p99");
    assert_eq!(refused["error"]["code"], "caller_unresolved", "{refused}");
    let kept = actor(&mut server, None, &lead);
    assert_eq!(kept["result"]["actor"]["pane_id"], lead.as_str(), "{kept}");
}

#[cfg(unix)]
#[tokio::test]
async fn fork_smoke_agents_stale_caller_matching_several_panes_is_refused() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    let chain = crate::platform::process_ancestry(me);
    let parent = *chain.get(1).expect("the test process has a parent");
    set_pane_pid(&mut server, panes[0], me);
    set_pane_pid(&mut server, panes[1], parent);

    let refused = actor(&mut server, Some(me), "w9:p99");
    assert_eq!(refused["error"]["code"], "caller_unresolved", "{refused}");
    // An id naming one of them stays that pane.
    let fixer = public(&server, panes[1]);
    let kept = actor(&mut server, Some(me), &fixer);
    assert_eq!(kept["result"]["actor"]["pane_id"], fixer.as_str(), "{kept}");
}

const SESSION: &str = "5b21611c-f122-4167-be9a-d57b522a9979";

/// A Claude `SessionStart` hook report for `pane_id`, sent by `peer`.
fn report_session(
    server: &mut HeadlessServer,
    peer: Option<u32>,
    pane_id: &str,
) -> serde_json::Value {
    api_from(
        server,
        peer,
        Method::PaneReportAgentSession(crate::api::schema::PaneReportAgentSessionParams {
            pane_id: pane_id.into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            seq: Some(1),
            agent_session_id: Some(SESSION.into()),
            agent_session_path: None,
            session_start_source: Some("resume".into()),
            resume_argv: None,
        }),
    )
}

fn reported_session(server: &mut HeadlessServer, pane: crate::layout::PaneId) -> Option<String> {
    terminal(server, pane)
        .persistable_agent_session()
        .map(|session| session.session_ref.value)
}

#[tokio::test]
async fn fork_smoke_agents_stale_hook_report_is_found_by_its_process() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], me);

    // The hook's HERDR_PANE_ID names no pane any more (a tab moved by an
    // older herdr): the pane running the hook takes the report.
    let landed = report_session(&mut server, Some(me), "w9:p99");
    assert_eq!(landed["result"]["type"], "ok", "{landed}");
    assert_eq!(
        reported_session(&mut server, panes[1]).as_deref(),
        Some(SESSION)
    );
    assert_eq!(reported_session(&mut server, panes[0]), None);

    // Self-heal: the stale id is now an alias of that pane, live and in its
    // meta, so it resolves without a peer and survives a restart.
    assert_eq!(
        server.app.parse_pane_id("w9:p99"),
        Some((1, panes[1])),
        "the stale id resolves directly"
    );
    assert!(terminal(&mut server, panes[1])
        .agent_meta()
        .public_aliases
        .iter()
        .any(|alias| alias == "w9:p99"));
    let metadata = api_from(
        &mut server,
        None,
        Method::PaneReportMetadata(
            serde_json::from_value(serde_json::json!({
                "pane_id": "w9:p99",
                "source": "herdr:claude",
                "agent": "claude",
                "title": "healed",
            }))
            .expect("metadata params"),
        ),
    );
    assert_eq!(metadata["result"]["type"], "ok", "{metadata}");
    let captured = crate::persist::capture(
        &server.app.state.workspaces,
        &server.app.state.terminals,
        &server.app.terminal_runtimes,
        Some(1),
        0,
    );
    let json = serde_json::to_string(&captured).expect("session snapshot encodes");
    let snapshot: crate::persist::SessionSnapshot =
        serde_json::from_str(&json).expect("session snapshot decodes");
    let aliases: Vec<String> = snapshot.workspaces[1]
        .tabs
        .iter()
        .flat_map(|tab| tab.panes.values())
        .filter_map(|pane| pane.agent_meta.as_ref())
        .flat_map(|meta| meta.public_aliases.clone())
        .collect();
    assert_eq!(
        aliases,
        vec!["w9:p99".to_string()],
        "persisted with the pane"
    );
}

#[tokio::test]
async fn fork_smoke_agents_stale_hook_report_without_its_process_is_refused() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], UNRELATED_PID + 1);

    // No pane runs the reporter: refused as before, and no alias is made.
    let refused = report_session(&mut server, Some(me), "w9:p99");
    assert_eq!(refused["error"]["code"], "pane_not_found", "{refused}");
    assert_eq!(server.app.parse_pane_id("w9:p99"), None);
    // No socket peer (an in-process or client request): the id alone.
    set_pane_pid(&mut server, panes[1], me);
    let refused = report_session(&mut server, None, "w9:p99");
    assert_eq!(refused["error"]["code"], "pane_not_found", "{refused}");
    // In-process delegation never looks at the peer either.
    server.app.agents_model.caller_process = Some(me);
    server.app.agents_model.delegating = 1;
    assert_eq!(server.app.resolve_reported_pane("w9:p99"), None);
    server.app.agents_model.delegating = 0;
    server.app.agents_model.caller_process = None;
    assert_eq!(reported_session(&mut server, panes[1]), None);
}

#[tokio::test]
async fn fork_smoke_agents_hook_report_with_a_good_id_is_not_redirected() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    set_pane_pid(&mut server, panes[1], me);

    // The id names the reporter's own pane: kept.
    let kept = report_session(&mut server, Some(me), &fixer);
    assert_eq!(kept["result"]["type"], "ok", "{kept}");
    assert_eq!(
        reported_session(&mut server, panes[1]).as_deref(),
        Some(SESSION)
    );

    // The id names a pane whose process is unknown: nothing proves it is
    // not the reporter's, so it is kept.
    server.app.terminal_runtimes.remove(
        &server.app.state.workspaces[1]
            .pane_state(panes[0])
            .unwrap()
            .attached_terminal_id
            .clone(),
    );
    let kept = report_session(&mut server, Some(me), &lead);
    assert_eq!(kept["result"]["type"], "ok", "{kept}");
    assert_eq!(
        reported_session(&mut server, panes[0]).as_deref(),
        Some(SESSION)
    );
    // A live id never becomes an alias.
    assert!(server.app.state.public_pane_id_aliases.is_empty());
}

#[tokio::test]
async fn fork_smoke_agents_hook_report_naming_another_process_pane_is_kept() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    set_pane_pid(&mut server, panes[0], UNRELATED_PID);
    set_pane_pid(&mut server, panes[1], me);
    let lead = public(&server, panes[0]);

    // An id that names a live pane is used as given, even when that pane
    // runs another known process than the reporter's.
    let landed = report_session(&mut server, Some(me), &lead);
    assert_eq!(landed["result"]["type"], "ok", "{landed}");
    assert_eq!(
        reported_session(&mut server, panes[0]).as_deref(),
        Some(SESSION)
    );
    assert_eq!(reported_session(&mut server, panes[1]), None);
    assert!(server.app.state.public_pane_id_aliases.is_empty());
    assert_eq!(server.app.parse_pane_id(&lead), Some((1, panes[0])));
}

/// The lead's request delegates a method in-process for another pane (as
/// `agents.open_tab` reads `team.context` for the pane it opened): the
/// lead's process proves nothing about that pane, so it stays that pane.
#[tokio::test]
async fn fork_smoke_agents_delegated_caller_is_not_redirected_to_the_requester() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    // The lead's pane runs the requesting process; the new pane runs its
    // own shell.
    set_pane_pid(&mut server, panes[0], me);
    set_pane_pid(&mut server, panes[1], UNRELATED_PID);

    // Mid-request (the lead's peer is recorded): a delegated call for the
    // new pane.
    server.app.agents_model.caller_process = Some(me);
    let delegated = server.app.call_method(Method::AgentsActor(
        crate::api::schema::agents_model::AgentsActorParams {
            caller_pane: fixer.clone(),
            ..Default::default()
        },
    ));
    let Ok(crate::api::schema::ResponseResult::AgentsActor { actor }) = delegated else {
        panic!("delegated actor: {delegated:?}");
    };
    assert_eq!(actor.pane_id, fixer, "the delegated caller keeps its pane");
    // The outer request still has its own peer after the delegation.
    assert_eq!(server.app.agents_model.caller_process, Some(me));
    let resolved = server.app.resolve_caller_pane(&lead);
    assert_eq!(resolved, Some((1, panes[0])));
    server.app.agents_model.caller_process = None;
}

/// Interleaved requests: the lead's and the new agent's, each from its own
/// process, each resolve to their own pane; a request without a peer after
/// them finds no leftover process.
#[tokio::test]
async fn fork_smoke_agents_interleaved_callers_keep_their_own_panes() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    let lead = public(&server, panes[0]);
    let fixer = public(&server, panes[1]);
    set_pane_pid(&mut server, panes[0], me);
    set_pane_pid(&mut server, panes[1], UNRELATED_PID);

    for _ in 0..3 {
        let found = actor(&mut server, Some(me), &lead);
        assert_eq!(
            found["result"]["actor"]["pane_id"],
            lead.as_str(),
            "{found}"
        );
        // The new agent's process is not one this test can be (gone, or
        // not yet known): no proof against its own valid id.
        let found = actor(&mut server, Some(UNRELATED_PID + 7), &fixer);
        assert_eq!(
            found["result"]["actor"]["pane_id"],
            fixer.as_str(),
            "{found}"
        );
        let found = actor(&mut server, None, &fixer);
        assert_eq!(
            found["result"]["actor"]["pane_id"],
            fixer.as_str(),
            "{found}"
        );
    }
    // Deferred work after the lead's request (a launch settling, a queued
    // message) runs with no peer left over: the new pane stays itself.
    let _ = actor(&mut server, Some(me), &lead);
    assert_eq!(server.app.agents_model.caller_process, None);
    assert_eq!(server.app.resolve_caller_pane(&fixer), Some((1, panes[1])));
}

/// A valid id whose pane's process is not known yet (a fresh pane) is kept
/// even when the peer runs under another pane: nothing proves it is not the
/// caller's.
#[tokio::test]
async fn fork_smoke_agents_valid_caller_with_an_unknown_pane_process_is_kept() {
    let (mut server, _input, panes) = crew_server();
    let me = std::process::id();
    let fixer = public(&server, panes[1]);
    set_pane_pid(&mut server, panes[0], me);
    server.app.terminal_runtimes.remove(
        &server.app.state.workspaces[1]
            .pane_state(panes[1])
            .unwrap()
            .attached_terminal_id
            .clone(),
    );
    let kept = actor(&mut server, Some(me), &fixer);
    assert_eq!(kept["result"]["actor"]["pane_id"], fixer.as_str(), "{kept}");
}

/// A freshly opened agent (the server's launch record still pending) whose
/// agent herdr already detects is an agent caller: it may message others
/// during its first turn.
#[tokio::test]
async fn fork_smoke_agents_a_freshly_launched_detected_agent_is_an_agent_caller() {
    let (mut server, _input, panes) = crew_server();
    let fixer = public(&server, panes[1]);
    let now = std::time::Instant::now();
    terminal(&mut server, panes[1]).set_detected_state(None, AgentState::Unknown);
    terminal(&mut server, panes[1]).begin_managed_agent(
        "fixer".into(),
        Agent::Claude,
        now,
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(600),
    );
    // Not detected yet: the launch alone does not make it an agent.
    let shell = actor(&mut server, None, &fixer);
    assert_eq!(shell["result"]["actor"]["live"], false, "{shell}");
    // Detected while its kickoff turn runs: an agent.
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Claude), AgentState::Working);
    assert!(terminal(&mut server, panes[1]).managed_agent_launch_pending());
    let agent = actor(&mut server, None, &fixer);
    assert_eq!(agent["result"]["actor"]["live"], true, "{agent}");
    assert_eq!(agent["result"]["actor"]["kind"], "agent", "{agent}");
    // Another agent detected than the one launched: not this launch's.
    terminal(&mut server, panes[1]).set_detected_state(Some(Agent::Codex), AgentState::Working);
    let other = actor(&mut server, None, &fixer);
    assert_eq!(other["result"]["actor"]["live"], false, "{other}");
}

fn move_pane_to_new_tab(server: &mut HeadlessServer, pane_id: String, workspace_id: String) {
    let moved = public_api(
        server,
        Method::PaneMove(crate::api::schema::PaneMoveParams {
            pane_id,
            destination: crate::api::schema::PaneMoveDestination::NewTab {
                workspace_id: Some(workspace_id),
                label: None,
            },
            focus: false,
        }),
    );
    assert_eq!(moved["result"]["type"], "pane_move", "{moved}");
}

fn pane_index(server: &HeadlessServer, pane: crate::layout::PaneId) -> usize {
    server
        .app
        .state
        .workspaces
        .iter()
        .position(|ws| ws.pane_state(pane).is_some())
        .expect("live pane")
}

fn meta_aliases(server: &HeadlessServer, pane: crate::layout::PaneId) -> Vec<String> {
    let ws = &server.app.state.workspaces[pane_index(server, pane)];
    let terminal_id = &ws.pane_state(pane).unwrap().attached_terminal_id;
    server.app.state.terminals[terminal_id]
        .agent_meta()
        .public_aliases
        .clone()
}

#[tokio::test]
async fn fork_smoke_a_client_pane_focus_on_a_reused_workspace_id_focuses_the_live_pane() {
    let mut server = test_headless_server();
    let mut old = crate::workspace::Workspace::test_new("old");
    old.test_add_tab(Some("second"));
    let (first, second) = (old.tabs[0].root_pane, old.tabs[1].root_pane);
    let other = crate::workspace::Workspace::test_new("other");
    let stranger = other.tabs[0].root_pane;
    let old_id = old.id.clone();
    let other_id = other.id.clone();
    server.app.state.workspaces = vec![old, other];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(1);
    server.app.state.selected = 1;
    server.app.state.mode = crate::app::Mode::Terminal;
    let first_id = server.app.public_pane_id(0, first).unwrap();
    let second_id = server.app.public_pane_id(0, second).unwrap();
    assert_eq!(first_id, format!("{old_id}:p1"));

    // Both panes leave `old`, which closes; their old ids stay aliases.
    move_pane_to_new_tab(&mut server, first_id.clone(), other_id.clone());
    move_pane_to_new_tab(&mut server, second_id.clone(), other_id.clone());
    assert!(server.app.parse_workspace_id(&old_id).is_none());
    assert_eq!(meta_aliases(&server, second), vec![second_id.clone()]);
    // An alias still names its pane while no live pane has the id.
    assert_eq!(
        server.app.parse_pane_id(&first_id).map(|(_, p)| p),
        Some(first)
    );

    // A restart reuses the closed workspace's id for a new group.
    let mut reused = crate::workspace::Workspace::test_new("herd+");
    reused.id = old_id.clone();
    let reused_root = reused.tabs[0].root_pane;
    server.app.state.workspaces.push(reused);
    server.app.state.ensure_test_terminals();
    // The live id wins over the stale alias even before it is dropped.
    assert_eq!(
        server.app.parse_pane_id(&first_id).map(|(_, p)| p),
        Some(reused_root)
    );
    server
        .app
        .state
        .remove_alias_shadowed_by_new_pane(reused_root);
    assert!(meta_aliases(&server, first).is_empty());

    // A pane that arrives in the reused group takes `<old>:p2`: the moved
    // pane's alias goes, live and from its meta.
    let stranger_id = server.app.public_pane_id(0, stranger).unwrap();
    move_pane_to_new_tab(&mut server, stranger_id.clone(), old_id.clone());
    let reused_idx = server.app.parse_workspace_id(&old_id).unwrap();
    assert_eq!(
        server.app.public_pane_id(reused_idx, stranger).as_deref(),
        Some(second_id.as_str())
    );
    assert!(meta_aliases(&server, second).is_empty());
    // Only the stranger's own old id is an alias now.
    assert_eq!(
        server
            .app
            .state
            .public_pane_id_aliases
            .iter()
            .map(|(id, pane)| (id.clone(), *pane))
            .collect::<Vec<_>>(),
        vec![(stranger_id.clone(), stranger)]
    );
    server.app.state.assert_invariants_for_test();

    // A client's PaneFocus on the reused ids focuses the live panes.
    let (control, _) = connect_matching_test_shell(&mut server, 71);
    let _ = control.recv().expect("snapshot");
    for (public_id, pane) in [(&second_id, stranger), (&first_id, reused_root)] {
        let focused = client_api(
            &mut server,
            71,
            Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: public_id.clone(),
            }),
        );
        assert!(focused.get("error").is_none(), "{focused}");
        let target = server.shell_focus_target(71).expect("focus target");
        assert_eq!(target.pane_id, pane, "{public_id}");
        assert_eq!(target.workspace_index, reused_idx, "{public_id}");
    }
    shutdown_test_runtimes(&mut server);
}
