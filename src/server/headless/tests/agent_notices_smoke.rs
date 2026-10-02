//! Fork smoke tests for agent cards (`agent.notify` → `endpoint.agent-notices.v1`)
//! and the Agents settings methods. FORK.md section 10 lists them by name; the
//! names carry `fork_smoke` so the sync gate's `-E 'test(fork_smoke)'` runs them.

use super::*;
use crate::agent_resume::AgentSessionRef;
use crate::api::schema::{
    AgentNoticeDismissParams, AgentNoticeKind, AgentNotifyParams, EmptyParams, Method,
    PaneMoveDestination, PaneMoveParams, PaneTarget, Request,
};
use crate::detect::{Agent, AgentState};
use crate::server::headless::agent_notices::{AgentNoticesPayload, AGENT_NOTICES_KIND};

/// Two workspaces: `first` with tabs `home` (a shell) and `work` (Claude
/// agent `api` with a session), `second` with one shell tab.
fn notices_server() -> (HeadlessServer, String) {
    let mut server = test_headless_server();
    let mut first = crate::workspace::Workspace::test_new("first");
    first.test_add_tab(Some("work"));
    first.switch_tab(0);
    server.app.state.workspaces = vec![first, crate::workspace::Workspace::test_new("second")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let agent_pane = server.app.state.workspaces[0].tabs[1].root_pane;
    let terminal_id = server.app.state.workspaces[0].tabs[1].panes[&agent_pane]
        .attached_terminal_id
        .clone();
    let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
    terminal
        .set_agent_session_ref(
            "herdr:claude".into(),
            "claude".into(),
            AgentSessionRef::id("notices-smoke-session"),
            Some(1),
        )
        .expect("session ref accepted");
    terminal.set_agent_name("api".into());
    let pane_id = server.app.public_pane_id(0, agent_pane).unwrap();
    (server, pane_id)
}

/// A public API request (an agent's CLI or MCP server), as the socket delivers it.
fn public_api(server: &mut HeadlessServer, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "notices-smoke".into(),
            method,
        },
        respond_to,
        response_write_complete: None,
        stream_active: None,
    });
    serde_json::from_str(&response_rx.recv().expect("api response")).expect("json response")
}

/// A request from client shell `client_id` (its own user input).
fn client_api(server: &mut HeadlessServer, client_id: u64, method: Method) -> serde_json::Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        client_id,
        crate::api::ApiRequestMessage {
            request: Request {
                id: "notices-smoke-client".into(),
                method,
            },
            respond_to,
            response_write_complete: None,
            stream_active: None,
        },
    );
    serde_json::from_str(&response_rx.recv().expect("client response")).expect("json response")
}

fn notify(server: &mut HeadlessServer, pane: &str, title: &str) -> serde_json::Value {
    public_api(
        server,
        Method::AgentNotify(AgentNotifyParams {
            caller_pane: pane.into(),
            kind: AgentNoticeKind::Question,
            title: title.into(),
            body: Some("details\nmore".into()),
        }),
    )
}

/// Every card payload in the control stream so far, in order.
fn notice_payloads(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<AgentNoticesPayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == AGENT_NOTICES_KIND {
                payloads.push(AgentNoticesPayload::decode(&data).expect("payload decodes"));
            }
        }
    }
    payloads
}

fn listed(server: &mut HeadlessServer) -> serde_json::Value {
    public_api(server, Method::AgentNotices(EmptyParams::default()))["result"]["notices"].clone()
}

#[tokio::test]
async fn fork_smoke_agent_notify_reaches_every_client_and_a_dismiss_fans_out() {
    let (mut server, pane) = notices_server();
    let (first, _first_render) = connect_test_shell(&mut server, 31, 80, 23);
    let (second, _second_render) = connect_test_shell(&mut server, 32, 80, 23);
    server.render_and_stream();
    assert!(
        notice_payloads(&first).is_empty() && notice_payloads(&second).is_empty(),
        "a server without cards sends nothing"
    );

    // The sender comes from the pane record; the reply names the card.
    let shown = notify(&mut server, &pane, "Need a decision");
    assert_eq!(shown["result"]["type"], "agent_notify", "{shown}");
    assert_eq!(shown["result"]["outcome"], "shown");
    let card_id = shown["result"]["id"].as_str().unwrap().to_string();
    let notices = listed(&mut server);
    assert_eq!(notices[0]["name"], "api", "{notices}");
    assert_eq!(notices[0]["agent"], "claude");
    assert_eq!(notices[0]["kind"], "question");
    assert_eq!(notices[0]["body"], "details\nmore");
    assert_eq!(
        notices[0]["tab_id"],
        server.app.public_tab_id(0, 1).unwrap()
    );

    server.render_and_stream();
    for control in [&first, &second] {
        let payloads = notice_payloads(control);
        assert_eq!(payloads.len(), 1, "one payload per client");
        assert!(payloads[0].initial, "the first payload seeds the client");
        assert_eq!(payloads[0].notices.len(), 1);
        assert_eq!(payloads[0].notices[0].id, card_id);
        assert_eq!(payloads[0].notices[0].tab_label.as_deref(), Some("work"));
    }
    // Nothing changed: no payload.
    server.render_and_stream();
    assert!(notice_payloads(&first).is_empty());

    // The same text again is deduped (same id); a second card from the same
    // agent within 20 s is rate-limited.
    let _ = server.app.render_dirty.take();
    let again = notify(&mut server, &pane, "Need a decision");
    assert_eq!(again["result"]["outcome"], "deduped", "{again}");
    assert_eq!(again["result"]["id"], card_id.as_str());
    let limited = notify(&mut server, &pane, "Another thing");
    assert_eq!(limited["error"]["code"], "rate_limited", "{limited}");
    let unknown = notify(&mut server, "w9:p9", "who");
    assert_eq!(unknown["error"]["code"], "pane_not_found", "{unknown}");
    let empty = notify(&mut server, &pane, " \u{1b}[2J ");
    assert_eq!(empty["error"]["code"], "invalid_params", "{empty}");
    // Duplicates and refusals change nothing a client sees and request no render.
    assert!(!server.app.render_dirty.is_pending());
    server.render_and_stream();
    assert!(notice_payloads(&second).is_empty());
    assert!(notice_payloads(&first).is_empty());

    // The second client dismisses; the first follows.
    let dismissed = client_api(
        &mut server,
        32,
        Method::AgentNoticeDismiss(AgentNoticeDismissParams {
            ids: vec![card_id],
            all: false,
        }),
    );
    assert_eq!(dismissed["result"]["type"], "agent_notices", "{dismissed}");
    assert_eq!(dismissed["result"]["notices"], serde_json::json!([]));
    server.render_and_stream();
    let cleared = notice_payloads(&first);
    assert_eq!(cleared.len(), 1);
    assert!(cleared[0].notices.is_empty() && !cleared[0].initial);

    // A client attaching later gets the list as its first (initial) payload.
    let (late, _late_render) = connect_test_shell(&mut server, 33, 80, 23);
    server.render_and_stream();
    let seeded = notice_payloads(&late);
    assert_eq!(seeded.len(), 1);
    assert!(seeded[0].initial && seeded[0].notices.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agent_notice_is_cleared_only_by_the_users_own_visit() {
    let (mut server, pane) = notices_server();
    let home_tab = server.app.public_tab_id(0, 0).unwrap();
    let work_tab = server.app.public_tab_id(0, 1).unwrap();
    let (_control, _render) = connect_test_shell(&mut server, 41, 80, 23);
    assert!(server.focus_shell_client_on_tab(41, &home_tab));
    notify(&mut server, &pane, "look at me");

    // API-driven focus (an agent's `herdr agent focus`, agents_open_tab) does
    // not count as the user seeing the card.
    server.app.state.switch_workspace_tab(0, 1);
    server.focus_all_shell_clients_on_default_target();
    assert_eq!(
        server.shell_tab_id_for_client(41).as_deref(),
        Some(work_tab.as_str())
    );
    assert_eq!(listed(&mut server).as_array().unwrap().len(), 1);
    // Re-focusing the tab the client is already on is not a visit either.
    assert!(server.focus_shell_client_on_tab(41, &work_tab));
    assert_eq!(listed(&mut server).as_array().unwrap().len(), 1);

    // The user moves away (nothing seen), then into the card's tab: seen.
    assert!(server.focus_shell_client_on_tab(41, &home_tab));
    assert_eq!(listed(&mut server).as_array().unwrap().len(), 1);
    client_api(
        &mut server,
        41,
        Method::TabFocus(crate::api::schema::TabTarget {
            tab_id: work_tab.clone(),
        }),
    );
    assert_eq!(listed(&mut server), serde_json::json!([]));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agent_notice_is_seen_by_a_workspace_switch_into_its_tab() {
    let (mut server, pane) = notices_server();
    let work_tab = server.app.public_tab_id(0, 1).unwrap();
    let first_ws = server.app.public_workspace_id(0);
    let second_ws = server.app.public_workspace_id(1);
    let (_control, _render) = connect_test_shell(&mut server, 61, 80, 23);
    assert!(server.focus_shell_client_on_tab(61, &work_tab));
    let workspace_focus = |workspace_id: &str| {
        Method::WorkspaceFocus(crate::api::schema::WorkspaceTarget {
            workspace_id: workspace_id.to_owned(),
        })
    };
    client_api(&mut server, 61, workspace_focus(&second_ws));
    notify(&mut server, &pane, "over here");
    assert_eq!(listed(&mut server).as_array().unwrap().len(), 1);

    // Switching back to the workspace lands on the card's tab: seen.
    client_api(&mut server, 61, workspace_focus(&first_ws));
    assert_eq!(
        server.shell_tab_id_for_client(61).as_deref(),
        Some(work_tab.as_str())
    );
    assert_eq!(listed(&mut server), serde_json::json!([]));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agent_notice_follows_a_moved_pane_and_goes_with_it() {
    let (mut server, pane) = notices_server();
    let (control, _render) = connect_test_shell(&mut server, 51, 80, 23);
    notify(&mut server, &pane, "moving soon");
    server.render_and_stream();
    assert_eq!(notice_payloads(&control).len(), 1);

    let moved = public_api(
        &mut server,
        Method::PaneMove(PaneMoveParams {
            pane_id: pane.clone(),
            destination: PaneMoveDestination::NewWorkspace {
                label: None,
                tab_label: None,
            },
            focus: false,
        }),
    );
    let new_pane = moved["result"]["move_result"]["pane"]["pane_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{moved}"))
        .to_string();
    assert_ne!(new_pane, pane);
    server.render_and_stream();
    let followed = notice_payloads(&control);
    let last = followed.last().expect("a payload after the move");
    assert_eq!(last.notices[0].pane_id, new_pane);
    assert_eq!(
        last.notices[0].workspace_id.as_deref(),
        moved["result"]["move_result"]["pane"]["workspace_id"].as_str()
    );

    public_api(
        &mut server,
        Method::PaneClose(PaneTarget { pane_id: new_pane }),
    );
    assert_eq!(listed(&mut server), serde_json::json!([]));
    server.render_and_stream();
    let gone = notice_payloads(&control);
    assert!(gone
        .last()
        .is_some_and(|payload| payload.notices.is_empty()));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agent_notice_follows_a_renamed_tab_or_space() {
    use crate::api::schema::{TabRenameParams, WorkspaceRenameParams};
    let (mut server, pane) = notices_server();
    let (control, _render) = connect_test_shell(&mut server, 61, 80, 23);
    notify(&mut server, &pane, "which retry policy?");
    server.render_and_stream();
    assert_eq!(notice_payloads(&control).len(), 1);

    // Another tab's rename changes no card: nothing is sent.
    let home = server.app.public_tab_id(0, 0).expect("home tab");
    public_api(
        &mut server,
        Method::TabRename(TabRenameParams {
            tab_id: home,
            label: "shell".into(),
        }),
    );
    server.render_and_stream();
    assert!(notice_payloads(&control).is_empty());

    // The card's own tab and space: the new labels are sent.
    let work = server.app.public_tab_id(0, 1).expect("work tab");
    let renamed = public_api(
        &mut server,
        Method::TabRename(TabRenameParams {
            tab_id: work,
            label: "retry-policy".into(),
        }),
    );
    assert!(renamed.get("error").is_none(), "{renamed}");
    server.render_and_stream();
    let followed = notice_payloads(&control);
    let last = followed.last().expect("a payload after the tab rename");
    assert_eq!(last.notices[0].tab_label.as_deref(), Some("retry-policy"));

    let workspace_id = server.app.public_workspace_id(0);
    public_api(
        &mut server,
        Method::WorkspaceRename(WorkspaceRenameParams {
            workspace_id,
            label: "calendar".into(),
        }),
    );
    server.render_and_stream();
    let followed = notice_payloads(&control);
    let last = followed.last().expect("a payload after the space rename");
    assert_eq!(last.notices[0].workspace_label.as_deref(), Some("calendar"));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agents_settings_write_the_config_and_the_hook_fix_waits_for_confirm() {
    use crate::api::schema::{AgentsFixParams, AgentsSettingsSetParams};
    let temp = crate::agent_wrap::test_support::TempHome::new("agents-smoke");
    let zshrc = temp.env.zshrc.clone().expect("temp zshrc");
    // Nothing here may point at the real home.
    assert!(temp.contains(&zshrc), "{}", zshrc.display());
    assert!(temp.contains(&temp.env.shell_file));
    assert!(temp.contains(&temp.config_path));
    assert!(temp.contains(&crate::agent_wrap::instructions::default_file_path()));
    temp.write_config("[browser]\nwrap_agents = false\n");
    std::fs::write(&zshrc, "export KEEP=1\n").unwrap();
    let (mut server, pane) = notices_server();
    server.app.agents_setup_env = Some(temp.env.clone());
    server.app.reload_config();
    let (_control, _render) = connect_test_shell(&mut server, 31, 80, 23);

    let settings = public_api(&mut server, Method::AgentsSettings(EmptyParams::default()));
    let info = &settings["result"]["info"];
    assert_eq!(settings["result"]["type"], "agents_settings", "{settings}");
    assert_eq!(info["wrap"], false);
    assert_eq!(info["wrap_source"], "browser_legacy");
    assert_eq!(info["notices"], true);

    let set = |server: &mut HeadlessServer, key: &str, value: serde_json::Value| {
        public_api(
            server,
            Method::AgentsSettingsSet(AgentsSettingsSetParams {
                key: key.into(),
                value,
            }),
        )
    };
    // wrap: [agents] wrap written, the legacy key dropped, applied live.
    let on = set(&mut server, "wrap", serde_json::json!(true));
    assert_eq!(on["result"]["info"]["wrap"], true, "{on}");
    assert_eq!(on["result"]["info"]["wrap_source"], "agents");
    let text = std::fs::read_to_string(&temp.config_path).unwrap();
    assert!(text.contains("[agents]\nwrap = true"), "{text}");
    assert!(!text.contains("wrap_agents"), "{text}");
    for (key, value) in [("tools", true), ("instructions", true)] {
        let reply = set(&mut server, key, serde_json::json!(value));
        assert_eq!(reply["result"]["info"][key], value, "{reply}");
    }
    // instructions_file = "file": the file is seeded in the (temp) config dir.
    let file = set(&mut server, "instructions_file", serde_json::json!("file"));
    let seeded = crate::agent_wrap::instructions::default_file_path();
    assert!(seeded.is_file(), "{file}");
    assert_eq!(
        file["result"]["info"]["instructions_file"],
        seeded.display().to_string()
    );
    let reset = set(
        &mut server,
        "instructions_file",
        serde_json::json!("default"),
    );
    assert_eq!(reset["result"]["info"]["instructions_detail"], "built-in");
    // Bad requests write nothing.
    let before = std::fs::read_to_string(&temp.config_path).unwrap();
    for (key, value) in [
        ("wrap", serde_json::json!("yes")),
        ("teams", serde_json::json!(true)),
        ("instructions_file", serde_json::json!(3)),
    ] {
        let bad = set(&mut server, key, value);
        assert_eq!(bad["error"]["code"], "invalid_request", "{bad}");
    }
    assert_eq!(std::fs::read_to_string(&temp.config_path).unwrap(), before);
    // notices off: agent.notify is refused.
    set(&mut server, "notices", serde_json::json!(false));
    assert_eq!(
        notify(&mut server, &pane, "hello")["error"]["code"],
        "notices_off"
    );

    // The hook fix: refused without confirm, .zshrc untouched.
    let settings = public_api(&mut server, Method::AgentsSettings(EmptyParams::default()));
    let hook = &settings["result"]["info"]["checks"][0];
    assert_eq!(hook["id"], "shell_hook", "{settings}");
    assert_eq!(hook["state"], "missing");
    assert_eq!(hook["fixable"], true);
    assert!(settings["result"]["info"]["hook_preview"]
        .as_str()
        .is_some_and(|preview| preview.contains("# herdr+")));
    for ids in [vec![], vec!["shell_hook".to_string()]] {
        let refused = client_api(
            &mut server,
            31,
            Method::AgentsFix(AgentsFixParams {
                ids,
                confirm: false,
            }),
        );
        assert_eq!(refused["error"]["code"], "confirm_required", "{refused}");
    }
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), "export KEEP=1\n");
    assert!(!temp.env.shell_file.exists());
    let unknown = public_api(
        &mut server,
        Method::AgentsFix(AgentsFixParams {
            ids: vec!["zshrc".into()],
            confirm: true,
        }),
    );
    assert_eq!(unknown["error"]["code"], "invalid_request", "{unknown}");

    // Confirmed (inside the temp home): the line and the managed file appear.
    let fixed = public_api(
        &mut server,
        Method::AgentsFix(AgentsFixParams {
            ids: vec!["shell_hook".into()],
            confirm: true,
        }),
    );
    assert_eq!(fixed["result"]["type"], "agents_fix", "{fixed}");
    assert_eq!(fixed["result"]["results"][0]["state"], "ok", "{fixed}");
    let text = std::fs::read_to_string(&zshrc).unwrap();
    assert!(
        text.starts_with("export KEEP=1\n") && text.contains("# herdr+"),
        "{text}"
    );
    assert!(temp.env.shell_file.is_file());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn fork_smoke_agent_notice_goes_when_its_pane_process_exits() {
    let (mut server, pane) = notices_server();
    notify(&mut server, &pane, "about to exit");
    assert_eq!(listed(&mut server).as_array().unwrap().len(), 1);
    let (_, raw) = server.app.parse_pane_id(&pane).unwrap();
    server.handle_internal_event_with_forwarding(AppEvent::PaneDied {
        pane_id: raw,
        exit_reason: crate::platform::ChildExitReason::Exited,
    });
    assert!(
        server.app.parse_pane_id(&pane).is_none(),
        "the pane is gone"
    );
    assert!(server.app.agent_notices.is_empty(), "its card went with it");
    shutdown_test_runtimes(&mut server);
}
