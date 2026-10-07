//! Fork smoke tests, server side. FORK.md section 10 lists them by name and
//! the sync gate runs them with `-E 'test(fork_smoke)'`. Each one locks a fork
//! invariant that an upstream change could break without a merge conflict:
//! the fork hooks into upstream paths at specific points, and a parallel
//! upstream path merges cleanly and passes every upstream test.

use super::*;
use crate::agent_resume::AgentSessionRef;
use crate::api::schema::{AgentStatus, AgentSuspendParams, Method, Request};
use crate::detect::{Agent, AgentState};

const AGENT_NAME: &str = "reviewer";
const SESSION_ID: &str = "fork-smoke-session";

fn root_pane(server: &HeadlessServer) -> crate::layout::PaneId {
    server.app.state.workspaces[0].tabs[0].root_pane
}

fn root_terminal_id(server: &HeadlessServer) -> crate::terminal::TerminalId {
    let root = root_pane(server);
    server.app.state.workspaces[0].tabs[0].panes[&root]
        .attached_terminal_id
        .clone()
}

/// A headless server with one pane hosting a live, named Claude Code agent,
/// optionally with a known native session reference.
fn server_with_claude(
    session_id: Option<&str>,
) -> (HeadlessServer, tokio::sync::mpsc::Receiver<Bytes>) {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("fork-smoke")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    let terminal_id = root_terminal_id(&server);
    let terminal = server.app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
    if let Some(session_id) = session_id {
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                AgentSessionRef::id(session_id),
                Some(1),
            )
            .expect("session ref accepted");
    }
    terminal.set_agent_name(AGENT_NAME.into());
    let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
    server.app.terminal_runtimes.insert(terminal_id, runtime);
    (server, rx)
}

fn api(server: &mut HeadlessServer, method: Method) -> serde_json::Value {
    let response = server.app.handle_api_request(Request {
        id: "fork-smoke".into(),
        method,
    });
    serde_json::from_str(&response).expect("api response is json")
}

/// The agent process leaves the pane: the process-exit report, then detection
/// seeing a bare shell.
fn observe_agent_exit(server: &mut HeadlessServer) {
    let pane_id = root_pane(server);
    let observed_at = std::time::Instant::now();
    for (agent, state, process_exited, at) in [
        (Some(Agent::Claude), AgentState::Idle, true, observed_at),
        (
            None,
            AgentState::Unknown,
            false,
            observed_at + Duration::from_millis(10),
        ),
    ] {
        server.app.handle_internal_event(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited,
            observed_at: at,
        });
    }
}

/// Suspend the hosted agent over the API and let its process exit.
fn suspend_and_exit(server: &mut HeadlessServer) {
    let response = api(
        server,
        Method::AgentSuspend(AgentSuspendParams {
            target: AGENT_NAME.into(),
        }),
    );
    assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
    observe_agent_exit(server);
}

/// The agent, tab and workspace statuses of the one-pane snapshot, after the
/// JSON round trip the endpoint projection takes to the client.
fn client_statuses(server: &HeadlessServer) -> (AgentStatus, AgentStatus, AgentStatus) {
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let json = serde_json::to_string(&snapshot).expect("snapshot encodes");
    let decoded: protocol::ClientShellSnapshot =
        serde_json::from_str(&json).expect("snapshot decodes");
    assert_eq!(
        decoded.agents.len(),
        1,
        "the parked pane stays an agent row"
    );
    assert_eq!(decoded.agents[0].name.as_deref(), Some(AGENT_NAME));
    (
        decoded.agents[0].agent_status,
        decoded.tabs[0].agent_status,
        decoded.workspaces[0].agent_status,
    )
}

/// `AgentStatus::Suspended` reaches the client shell snapshot through the
/// real server projection, before and after the agent process exits, for the
/// agent row and both rollups; the JSON decode keeps the `suspended` arm the
/// fork added to `deserialize_client_shell_agent_status`.
#[tokio::test]
async fn suspended_status_reaches_the_client_shell_snapshot() {
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    let (agent, _, _) = client_statuses(&server);
    assert_ne!(agent, AgentStatus::Suspended, "control: a live agent");

    let response = api(
        &mut server,
        Method::AgentSuspend(AgentSuspendParams {
            target: AGENT_NAME.into(),
        }),
    );
    assert_eq!(response["result"]["type"], "agent_suspended", "{response}");
    assert_eq!(
        client_statuses(&server),
        (
            AgentStatus::Suspended,
            AgentStatus::Suspended,
            AgentStatus::Suspended
        ),
        "during the exit wait"
    );

    observe_agent_exit(&mut server);
    assert_eq!(
        client_statuses(&server),
        (
            AgentStatus::Suspended,
            AgentStatus::Suspended,
            AgentStatus::Suspended
        ),
        "after the agent process exited"
    );
    shutdown_test_runtimes(&mut server);
}

/// A suspended pane survives capture, the session.json round trip and a
/// restore with `resume_agents_on_restore` on as a parked pane: no resume
/// plan, the record and name kept, reported as suspended to clients, and
/// captured as suspended again.
#[tokio::test]
async fn session_restore_keeps_a_suspended_pane_parked() {
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    suspend_and_exit(&mut server);
    let captured = crate::persist::capture(
        &server.app.state.workspaces,
        &server.app.state.terminals,
        &server.app.terminal_runtimes,
        Some(0),
        0,
    );
    shutdown_test_runtimes(&mut server);
    let json = serde_json::to_string(&captured).expect("session snapshot encodes");
    let snapshot: crate::persist::SessionSnapshot =
        serde_json::from_str(&json).expect("session snapshot decodes");
    let saved = snapshot.workspaces[0].tabs[0]
        .panes
        .values()
        .next()
        .expect("saved pane");
    assert!(saved.suspended_agent.is_some(), "captured as suspended");

    let (events, _events_rx) = tokio::sync::mpsc::channel(32);
    let (workspaces, terminals, runtimes) = crate::persist::restore(
        &snapshot,
        None,
        24,
        80,
        0,
        crate::app::exiting_test_command(),
        crate::config::ShellModeConfig::NonLogin,
        true,
        events,
        Arc::new(tokio::sync::Notify::new()),
        Arc::new(crate::render_signal::RenderSignal::new()),
    );
    let root = workspaces[0].tabs[0].root_pane;
    let terminal_id = workspaces[0].tabs[0].panes[&root]
        .attached_terminal_id
        .clone();
    let terminal = &terminals[&terminal_id];
    assert!(
        terminal.pending_agent_resume_plan.is_none(),
        "a suspended pane is never relaunched on restore"
    );
    let record = terminal
        .suspended_agent
        .as_ref()
        .expect("suspended record reinstated");
    assert_eq!(record.session.session_ref.value, SESSION_ID);
    assert!(record.exit_observed());
    assert_eq!(terminal.agent_name.as_deref(), Some(AGENT_NAME));

    let mut restored = test_headless_server();
    restored.app.state.workspaces = workspaces;
    restored.app.state.terminals = terminals;
    restored.app.terminal_runtimes = crate::terminal::TerminalRuntimeRegistry::from(runtimes);
    restored.app.state.active = Some(0);
    restored.app.state.selected = 0;
    restored.app.state.mode = crate::app::Mode::Terminal;
    assert_eq!(client_statuses(&restored).0, AgentStatus::Suspended);
    let recaptured = crate::persist::capture(
        &restored.app.state.workspaces,
        &restored.app.state.terminals,
        &restored.app.terminal_runtimes,
        Some(0),
        0,
    );
    assert!(
        recaptured.workspaces[0].tabs[0]
            .panes
            .values()
            .all(|pane| pane.suspended_agent.is_some()),
        "a restored suspended pane is saved as suspended again"
    );
    shutdown_test_runtimes(&mut restored);
}

/// The transcript backup store relies on the upstream-owned Claude hook
/// asset forwarding Claude's `transcript_path` as `agent_session_path`. Runs
/// the shipped asset against a stand-in socket, then feeds its request to the
/// app and checks the path lands on the session the backup store copies.
#[cfg(unix)]
#[tokio::test]
async fn claude_hook_asset_reports_the_transcript_path_the_backup_store_uses() {
    use std::io::{Read as _, Write as _};

    let dir = std::env::temp_dir().join(format!("hfs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let asset = dir.join("herdr-agent-state.sh");
    fs::write(
        &asset,
        include_str!("../../../integration/assets/claude/herdr-agent-state.sh"),
    )
    .unwrap();
    let socket_path = dir.join("hook.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();

    let (mut server, _rx) = server_with_claude(None);
    let pane_id = server
        .app
        .public_pane_id(0, root_pane(&server))
        .expect("public pane id");
    let transcript = dir
        .join("claude-config")
        .join("projects")
        .join("-fork-smoke")
        .join(format!("{SESSION_ID}.jsonl"));
    let transcript_str = transcript.to_str().unwrap().to_owned();

    let mut child = std::process::Command::new("sh")
        .arg(&asset)
        .arg("session")
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", &pane_id)
        .env_remove("CURSOR_VERSION")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("run the claude hook asset");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": SESSION_ID,
                "transcript_path": transcript_str,
                "source": "startup",
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    assert!(child.wait().unwrap().success());

    listener.set_nonblocking(true).unwrap();
    let (mut stream, _) = listener
        .accept()
        .expect("the hook connected to HERDR_SOCKET_PATH (is python3 on PATH?)");
    stream.set_nonblocking(false).unwrap();
    let mut sent = String::new();
    stream.read_to_string(&mut sent).unwrap();
    let request: Request =
        serde_json::from_str(sent.lines().next().expect("one request line")).unwrap();
    let Method::PaneReportAgentSession(params) = &request.method else {
        panic!("unexpected hook request: {sent}");
    };
    assert_eq!(params.agent_session_id.as_deref(), Some(SESSION_ID));
    assert_eq!(
        params.agent_session_path.as_deref(),
        Some(transcript_str.as_str()),
        "the hook must forward transcript_path as agent_session_path"
    );

    let response = api(&mut server, request.method.clone());
    assert!(response.get("error").is_none(), "{response}");
    let session = server.app.state.terminals[&root_terminal_id(&server)]
        .persistable_agent_session()
        .expect("reported session is persistable");
    assert_eq!(session.session_ref.value, SESSION_ID);
    assert_eq!(
        session.transcript_path.as_deref(),
        Some(transcript.as_path())
    );
    let native = crate::agent_resume::native_transcript_locations_in(&session, &dir)
        .expect("the backup store resolves the native transcript");
    assert_eq!(native.file, transcript);

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// Run the shipped Claude hook asset with `action` and the hook JSON `input`
/// against a stand-in socket and return the request it sent.
#[cfg(unix)]
fn run_claude_hook_asset(
    dir: &std::path::Path,
    pane_id: &str,
    action: &str,
    input: serde_json::Value,
) -> Request {
    claude_hook_asset_request(dir, pane_id, action, input)
        .expect("the hook connected to HERDR_SOCKET_PATH (is python3 on PATH?)")
}

/// Run the shipped Claude hook asset and return the request it sent, if it
/// sent one (it exits before connecting when it has nothing to report).
#[cfg(unix)]
fn claude_hook_asset_request(
    dir: &std::path::Path,
    pane_id: &str,
    action: &str,
    input: serde_json::Value,
) -> Option<Request> {
    claude_hook_asset_requests(dir, pane_id, action, input)
        .into_iter()
        .next()
}

/// Every request the shipped Claude hook asset sent, in order (a Stop sends
/// `pane.report_turn`, then the subagent snapshot).
#[cfg(unix)]
fn claude_hook_asset_requests(
    dir: &std::path::Path,
    pane_id: &str,
    action: &str,
    input: serde_json::Value,
) -> Vec<Request> {
    use std::io::{Read as _, Write as _};

    let asset = dir.join("herdr-agent-state.sh");
    fs::write(
        &asset,
        include_str!("../../../integration/assets/claude/herdr-agent-state.sh"),
    )
    .unwrap();
    let socket_path = dir.join(format!("{action}-{}.sock", input["hook_event_name"]));
    let _ = fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let mut child = std::process::Command::new("sh")
        .arg(&asset)
        .arg(action)
        .env("HERDR_ENV", "1")
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", pane_id)
        .env_remove("CURSOR_VERSION")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("run the claude hook asset");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    listener.set_nonblocking(true).unwrap();
    let mut requests = Vec::new();
    while let Ok((mut stream, _)) = listener.accept() {
        stream.set_nonblocking(false).unwrap();
        let mut sent = String::new();
        stream.read_to_string(&mut sent).unwrap();
        requests
            .push(serde_json::from_str(sent.lines().next().expect("one request line")).unwrap());
    }
    requests
}

#[cfg(unix)]
fn client_subagents(server: &HeadlessServer) -> (u32, AgentStatus) {
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let json = serde_json::to_string(&snapshot).expect("snapshot encodes");
    let decoded: protocol::ClientShellSnapshot =
        serde_json::from_str(&json).expect("snapshot decodes");
    (decoded.agents[0].subagents, decoded.agents[0].agent_status)
}

/// Claude's SubagentStart / SubagentStop hooks, through the shipped asset's
/// `subagent` action and `pane.report_subagent`, reach the client shell
/// snapshot as the agent's running subagent count; the agent going idle
/// clears it.
#[cfg(unix)]
#[tokio::test]
async fn claude_subagent_hooks_reach_the_client_shell_snapshot() {
    let dir = std::env::temp_dir().join(format!("hfs-subagents-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    let terminal_id = root_terminal_id(&server);
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Working);
    let pane_id = server
        .app
        .public_pane_id(0, root_pane(&server))
        .expect("public pane id");
    assert_eq!(client_subagents(&server), (0, AgentStatus::Working));

    for (event, expected) in [("SubagentStart", 1), ("SubagentStop", 0)] {
        let request = run_claude_hook_asset(
            &dir,
            &pane_id,
            "subagent",
            serde_json::json!({
                "hook_event_name": event,
                "session_id": SESSION_ID,
                "agent_id": "a1b2c3",
                "agent_type": "general-purpose",
            }),
        );
        let Method::PaneReportSubagent(params) = &request.method else {
            panic!("unexpected hook request: {request:?}");
        };
        assert_eq!(params.subagent_id, "a1b2c3");
        assert_eq!(params.agent, "claude");
        let response = api(&mut server, request.method.clone());
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(client_subagents(&server).0, expected, "after {event}");
    }

    // Claude's internal helper agents (no agent_type) are not reported.
    for event in ["SubagentStart", "SubagentStop"] {
        assert!(claude_hook_asset_request(
            &dir,
            &pane_id,
            "subagent",
            serde_json::json!({"hook_event_name": event, "agent_id": "helper", "agent_type": ""}),
        )
        .is_none());
    }

    let request = run_claude_hook_asset(
        &dir,
        &pane_id,
        "subagent",
        serde_json::json!({
            "hook_event_name": "SubagentStart",
            "agent_id": "d4e5",
            "agent_type": "general-purpose",
        }),
    );
    api(&mut server, request.method);
    assert_eq!(client_subagents(&server).0, 1);

    // The main turn ends with the agent still out in the background: the
    // Stop hook snapshots background_tasks (subagents with a type only).
    let stop = |tasks: serde_json::Value| {
        serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": SESSION_ID,
            "background_tasks": tasks,
        })
    };
    let requests = claude_hook_asset_requests(
        &dir,
        &pane_id,
        "stop",
        stop(serde_json::json!([
            {"id": "d4e5", "type": "subagent", "status": "running", "agent_type": "general-purpose"},
            {"id": "helper", "type": "subagent", "status": "running"},
            {"id": "s1", "type": "shell", "status": "running", "command": "sleep 60"},
        ])),
    );
    assert_eq!(requests.len(), 2, "the turn's end, then the snapshot");
    assert!(matches!(requests[0].method, Method::PaneReportTurn(_)));
    let request = &requests[1];
    let Method::PaneReportSubagent(params) = &request.method else {
        panic!("unexpected hook request: {request:?}");
    };
    assert_eq!(params.event, crate::api::schema::SubagentEvent::Snapshot);
    assert_eq!(params.subagent_ids, ["d4e5"]);
    let response = api(&mut server, request.method.clone());
    assert!(response.get("error").is_none(), "{response}");
    server
        .app
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    let (subagents, status) = client_subagents(&server);
    assert_eq!(subagents, 1, "the count outlives the turn");
    assert!(
        matches!(status, AgentStatus::Idle | AgentStatus::Done),
        "background agents do not keep the agent working: {status:?}"
    );

    // A Stop without background_tasks (an older Claude Code) reports only
    // the turn's end; one from a subagent sends nothing.
    let requests = claude_hook_asset_requests(
        &dir,
        &pane_id,
        "stop",
        serde_json::json!({"hook_event_name": "Stop", "session_id": SESSION_ID}),
    );
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0].method, Method::PaneReportTurn(_)));
    let mut from_subagent = stop(serde_json::json!([]));
    from_subagent["agent_id"] = "d4e5".into();
    assert!(claude_hook_asset_request(&dir, &pane_id, "stop", from_subagent).is_none());

    // The last agent reports back: the next Stop lists none.
    for request in claude_hook_asset_requests(&dir, &pane_id, "stop", stop(serde_json::json!([]))) {
        api(&mut server, request.method);
    }
    let (subagents, status) = client_subagents(&server);
    assert_eq!(subagents, 0);
    assert!(
        matches!(status, AgentStatus::Idle | AgentStatus::Done),
        "still idle when the last agent ends: {status:?}"
    );

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// Closing a tab that hosts a Claude Code agent records its session, the
/// record reaches `session.closed_list`, and `session.closed_reopen` opens a
/// tab in the same space with the same label that types the native resume
/// command for the same session into its new shell and keeps the session on
/// the pane; the record is gone afterwards. Runs against a private config
/// directory (nextest gives each test its own process).
#[cfg(unix)]
#[tokio::test]
async fn closed_agent_tab_reopens_from_the_list_with_the_same_session() {
    use crate::api::schema::{ClosedSessionTarget, EmptyParams, TabTarget};

    let dir = std::env::temp_dir().join(format!("herdr-fork-smoke-closed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("project")).unwrap();
    std::env::set_var("HOME", dir.join("home"));
    std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
    std::env::remove_var(crate::session::SESSION_ENV_VAR);
    assert!(crate::persist::closed_sessions::store_path().starts_with(&dir));

    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    server.app.policy.persist_session = true;
    server.app.state.default_shell = "/bin/cat".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    // `/bin/cat` is not a shell prompt: let the reopen type into it.
    server.app.coordinator.assume_shell_ready = true;
    server.app.state.workspaces[0].tabs[0].custom_name = Some("review".into());
    server.app.state.workspaces[0].test_add_tab(Some("other"));
    server.app.state.ensure_test_terminals();
    let root_terminal = root_terminal_id(&server);
    server
        .app
        .state
        .terminals
        .get_mut(&root_terminal)
        .unwrap()
        .cwd = dir.join("project");

    let tab_id = server.app.public_tab_id(0, 0).unwrap();
    let closed = api(&mut server, Method::TabClose(TabTarget { tab_id }));
    assert_eq!(closed["result"]["type"], "ok", "{closed}");

    let listed = api(
        &mut server,
        Method::SessionClosedList(EmptyParams::default()),
    );
    let sessions = listed["result"]["sessions"].as_array().expect("a list");
    assert_eq!(sessions.len(), 1, "{listed}");
    assert_eq!(sessions[0]["session_id"], SESSION_ID);
    assert_eq!(sessions[0]["label"], "review");
    let id = sessions[0]["id"].as_str().unwrap().to_string();

    let reopened = api(
        &mut server,
        Method::SessionClosedReopen(ClosedSessionTarget { id }),
    );
    assert_eq!(reopened["result"]["type"], "tab_created", "{reopened}");
    assert_eq!(reopened["result"]["tab"]["label"], "review");
    assert_eq!(
        reopened["result"]["tab"]["workspace_id"],
        server.app.public_workspace_id(0)
    );

    let workspace = &server.app.state.workspaces[0];
    let tab = &workspace.tabs[workspace.active_tab];
    let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
    let session = server.app.state.terminals[&terminal_id]
        .persistable_agent_session()
        .expect("the reopened pane holds the session");
    assert_eq!(session.session_ref.value, SESSION_ID);
    assert_eq!(session.agent, "claude");

    let resume = format!("claude --resume {SESSION_ID}");
    let runtime = server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .expect("the reopened tab runs a shell");
    for _ in 0..80 {
        if runtime
            .snapshot_history()
            .is_some_and(|text| text.contains(&resume))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        runtime
            .snapshot_history()
            .is_some_and(|text| text.contains(&resume)),
        "the native resume command reaches the new shell"
    );

    let after = api(
        &mut server,
        Method::SessionClosedList(EmptyParams::default()),
    );
    assert_eq!(after["result"]["sessions"], serde_json::json!([]));

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// `news.status` answers on a plain server (scheduling off, nothing in
/// flight, no history) and `news.run` opens the `News` tab in the first
/// space without focusing it, types the bundled runner's command into its
/// shell, and reports the run in flight; a second `news.run` is refused
/// while it lasts. Runs against a private news home (the tab's shell is
/// `/bin/cat`, so nothing actually runs).
#[cfg(unix)]
#[tokio::test]
async fn news_status_and_run_reach_the_news_tab() {
    use crate::api::schema::EmptyParams;

    let dir = std::env::temp_dir().join(format!("herdr-fork-smoke-news-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (mut server, _rx) = server_with_claude(None);
    server.app.state.default_shell = "/bin/cat".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces[0].tabs[0].custom_name = Some("work".into());

    let status = api(&mut server, Method::NewsStatus(EmptyParams::default()));
    assert_eq!(status["result"]["type"], "news_status", "{status}");
    assert_eq!(status["result"]["status"]["enabled"], false);
    assert!(status["result"]["status"].get("run").is_none());
    assert_eq!(status["result"]["status"]["recent"], serde_json::json!([]));

    let refused = api(&mut server, Method::NewsRun(EmptyParams::default()));
    assert_eq!(refused["error"]["code"], "news_unavailable", "{refused}");

    server.app.news.home = Some(dir.join("news"));
    let started = api(&mut server, Method::NewsRun(EmptyParams::default()));
    assert_eq!(started["result"]["type"], "news_status", "{started}");
    let run = &started["result"]["status"]["run"];
    assert_eq!(run["trigger"], "manual");
    assert_eq!(
        run["phase"], "starting",
        "cat is not a shell prompt, so the pane gets `q` first: {started}"
    );
    let tab_id = started["result"]["status"]["tab_id"]
        .as_str()
        .expect("the News tab id")
        .to_string();

    let workspace = &server.app.state.workspaces[0];
    assert_eq!(workspace.tabs.len(), 2);
    assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("News"));
    assert_eq!(workspace.active_tab, 0, "the News tab is not focused");
    assert_eq!(
        server.app.public_tab_id(0, 1).as_deref(),
        Some(tab_id.as_str())
    );
    let tab = &workspace.tabs[1];
    let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
    assert!(
        crate::integration::news_assets::runner_path(&dir.join("news")).is_file(),
        "the runner is installed under the home"
    );

    let runtime = server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .expect("the News tab runs a shell");
    // The quit sequence (CSI 9999 ~) renders as nothing, so the busy pane's
    // screen cannot show it; app::news::tests checks the bytes themselves.
    let _ = runtime;

    // The prompt is back: the next scheduler pass types the command.
    server.app.news.assume_shell_ready = true;
    assert!(server
        .app
        .handle_news_tasks(std::time::Instant::now() + Duration::from_secs(1)));
    let status = api(&mut server, Method::NewsStatus(EmptyParams::default()));
    assert_eq!(
        status["result"]["status"]["run"]["phase"], "running",
        "{status}"
    );

    let command = "--trigger manual";
    let runtime = server
        .app
        .terminal_runtimes
        .get(&terminal_id)
        .expect("the News tab runs a shell");
    for _ in 0..80 {
        if runtime
            .snapshot_history()
            .is_some_and(|text| text.contains(command))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let history = runtime.snapshot_history().unwrap_or_default();
    assert!(
        history.contains("news_run.py"),
        "the runner command reaches the shell: {history}"
    );
    assert!(history.contains(command), "{history}");

    let again = api(&mut server, Method::NewsRun(EmptyParams::default()));
    assert_eq!(again["error"]["code"], "news_run_in_flight", "{again}");
    assert_eq!(
        server.app.state.workspaces[0].tabs.len(),
        2,
        "no second tab"
    );

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// Turning news on for a desk that has never run (no run log, no editions)
/// starts a first run on the next scheduled-tasks pass of the headless
/// loop, before any listed time: the News tab appears and the run is in
/// flight as `scheduled`; the next pass starts nothing more.
#[cfg(unix)]
#[tokio::test]
async fn news_enabled_on_a_desk_that_never_ran_starts_a_first_run() {
    let dir = std::env::temp_dir().join(format!(
        "herdr-fork-smoke-news-first-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let (mut server, _rx) = server_with_claude(None);
    server.app.state.default_shell = "/bin/cat".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.news.home = Some(dir.join("news"));
    // Noon, a start a minute ago: no listed time is owed.
    server.app.news.local_override = Some((
        crate::app::news::LocalClock {
            minute_of_day: 12 * 60,
            second: 0,
        },
        "2026-09-29",
    ));
    server.app.news.last_started_at = Some(crate::app::news::unix_now() - 60);
    let now = std::time::Instant::now();
    server.handle_scheduled_tasks_headless(now, false);
    assert!(server.app.news.run.is_none(), "news is off");

    server.app.news.apply_config(&crate::config::NewsConfig {
        enabled: true,
        ..crate::config::NewsConfig::default()
    });
    assert!(
        server
            .app
            .next_news_deadline(now)
            .is_some_and(|at| at <= now),
        "the loop wakes at once"
    );
    assert!(server.handle_scheduled_tasks_headless(now, false));
    let run = server.app.news.run.as_ref().expect("the first run started");
    assert_eq!(run.trigger, crate::app::news::NewsTrigger::Scheduled);
    let workspace = &server.app.state.workspaces[0];
    assert_eq!(workspace.tabs.len(), 2);
    assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("News"));

    server.app.news.run = None;
    server.handle_scheduled_tasks_headless(std::time::Instant::now(), false);
    assert!(server.app.news.run.is_none(), "only one first run");
    assert_eq!(server.app.state.workspaces[0].tabs.len(), 2);

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// A finished run's `decision.notify` becomes a `SemanticNotification` of
/// kind Custom, sent through the same client-shell path as
/// `notification.show`, carrying the News pane so a click focuses it. With
/// no client shell it waits in the queue and goes out when one attaches
/// (the flush at the end of the client-connected block); the rate limit
/// holds it for a second at most.
#[cfg(unix)]
#[tokio::test]
async fn news_notification_waits_for_a_client_shell_and_reaches_it_on_attach() {
    let dir = std::env::temp_dir().join(format!(
        "herdr-fork-smoke-news-notify-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("news/runs")).unwrap();
    let home = dir.join("news");

    let (mut server, _rx) = server_with_claude(None);
    server.app.news.home = Some(home.clone());
    // A fixed local clock (noon): the real one would land inside the default
    // quiet hours 00:00-08:00 at night and hold the notification.
    server.app.news.local_override = Some((
        crate::app::news::LocalClock {
            minute_of_day: 12 * 60,
            second: 0,
        },
        "2026-09-29",
    ));
    server.app.state.workspaces[0].tabs[0].set_custom_name("News".into());
    server.app.news.tab_id = server.app.public_tab_id(0, 0);
    let news_pane = server.app.public_pane_id(0, root_pane(&server)).unwrap();

    // A run in flight, then its record with the editor's request.
    let now = std::time::Instant::now();
    let started = 1_800_000_000;
    server.app.news.run = Some(crate::app::news::NewsRun {
        started_at: started,
        started: crate::app::news::iso_utc(started),
        trigger: crate::app::news::NewsTrigger::Scheduled,
        index_len: 0,
        watch_from: now,
        phase: crate::app::news::NewsPhase::Running { next_poll: now },
    });
    fs::write(
        crate::persist::news::index_path(&home),
        format!(
            "{{\"started\":\"{}\",\"trigger\":\"scheduled\",\"outcome\":\"ok\",\"edition\":2,\"decision\":{{\"changed\":true,\"notify\":{{\"title\":\"Sonnet 5.5\",\"body\":\"Out now.\",\"urgency\":\"high\"}}}}}}\n",
            crate::app::news::iso_utc(started + 5)
        ),
    )
    .unwrap();
    assert!(server.app.handle_news_tasks(now + Duration::from_secs(5)));
    assert!(server.app.news.run.is_none());
    assert_eq!(server.app.news.notify.pending.len(), 1);
    assert!(
        !server.flush_news_notifications(now),
        "no client shell: the notification waits"
    );
    assert_eq!(server.app.news.notify.pending.len(), 1);

    // A client shell attaches.
    let (shell_tx, shell_control, _shell_frames) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(shell_tx),
        ),
    );
    assert!(server.flush_news_notifications(now));
    let message = read_server_message(
        shell_control
            .recv_timeout(Duration::from_millis(200))
            .expect("the notification reaches the shell"),
    );
    let ServerMessage::SemanticNotification(notification) = message else {
        panic!("expected a semantic notification, got {message:?}");
    };
    assert_eq!(
        notification.kind,
        protocol::SemanticNotificationKind::Custom
    );
    assert_eq!(notification.title, "News: Sonnet 5.5");
    assert_eq!(notification.body.as_deref(), Some("Out now."));
    assert_eq!(
        notification.sound,
        Some(protocol::SemanticNotificationSound::Done)
    );
    assert_eq!(notification.pane_id.as_deref(), Some(news_pane.as_str()));
    assert_eq!(
        notification.tab_id,
        server.app.public_tab_id(0, 0),
        "the card focuses the News tab"
    );
    assert!(server.app.news.notify.pending.is_empty());

    // Within a second of a delivery the rate limit holds the next one.
    server
        .app
        .news
        .notify
        .pending
        .push(crate::persist::news::PendingNewsNotify {
            kind: "failures".into(),
            title: "News runs failing".into(),
            body: None,
            high: false,
            deliver_after: 0,
            queued_at: started,
        });
    assert!(!server.flush_news_notifications(now));
    assert_eq!(
        server.app.news.notify_retry_at,
        Some(now + Duration::from_secs(1))
    );
    assert_eq!(
        server.app.next_news_deadline(now),
        Some(now + Duration::from_secs(1)),
        "the loop wakes for the retry"
    );
    assert!(server.flush_news_notifications(now + Duration::from_secs(1)));
    assert!(server.app.news.notify_retry_at.is_none());
    assert!(shell_control
        .recv_timeout(Duration::from_millis(200))
        .is_ok());

    // A retry deadline left behind when the last client shell goes is
    // cleared by the next flush: nothing to retry for, nothing to wake for.
    server.app.news.notify_retry_at = Some(now);
    server.clients.clear();
    server
        .app
        .news
        .notify
        .pending
        .push(crate::persist::news::PendingNewsNotify {
            kind: "failures".into(),
            title: "News runs failing".into(),
            body: None,
            high: false,
            deliver_after: 0,
            queued_at: started,
        });
    assert!(!server.flush_news_notifications(now + Duration::from_secs(2)));
    assert!(
        server.app.news.notify_retry_at.is_none(),
        "no client shell: the retry deadline is dropped"
    );
    assert_eq!(
        server.app.news.notify.pending.len(),
        1,
        "the notification waits"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The daily cap (two run notifications a day, a high-urgency one past it
/// once) is charged when a notification goes out, not when a run queues
/// it: a queued one replaced before delivery costs nothing, a dropped one
/// does not hold up the alert behind it.
#[cfg(unix)]
#[tokio::test]
async fn news_daily_cap_is_charged_at_delivery() {
    let (mut server, _rx) = server_with_claude(None);
    server.app.news.local_override = Some((
        crate::app::news::LocalClock {
            minute_of_day: 12 * 60,
            second: 0,
        },
        "2026-09-29",
    ));
    let (shell_tx, shell_control, _shell_frames) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new_with_mode(
            ClientConnectionMode::ClientShell,
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(shell_tx),
        ),
    );
    let now = std::time::Instant::now();
    let pending = |kind: &str, title: &str, high: bool| crate::persist::news::PendingNewsNotify {
        kind: kind.into(),
        title: title.into(),
        body: None,
        high,
        deliver_after: 0,
        queued_at: 1_800_000_000,
    };
    let title_of = |bytes: Vec<u8>| match read_server_message(bytes) {
        ServerMessage::SemanticNotification(notification) => notification.title,
        other => panic!("expected a notification, got {other:?}"),
    };
    let second = Duration::from_secs(1);

    // Queued and replaced: the ledger is untouched until a delivery.
    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: first", false));
    server.app.news.notify.pending.clear();
    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: one", false));
    assert_eq!(server.app.news.notify.delivered, 0);
    assert!(server.flush_news_notifications(now));
    assert_eq!(
        title_of(shell_control.recv_timeout(second).unwrap()),
        "News: one"
    );
    assert_eq!(server.app.news.notify.day, "2026-09-29");
    assert_eq!(server.app.news.notify.delivered, 1);

    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: two", false));
    assert!(server.flush_news_notifications(now + second));
    assert_eq!(
        title_of(shell_control.recv_timeout(second).unwrap()),
        "News: two"
    );
    assert_eq!(server.app.news.notify.delivered, 2);

    // The third is dropped at delivery, and the alert queued behind it goes
    // out in the same flush.
    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: three", false));
    server
        .app
        .news
        .notify
        .pending
        .push(pending("failures", "News runs failing", false));
    assert!(server.flush_news_notifications(now + 2 * second));
    assert_eq!(
        title_of(shell_control.recv_timeout(second).unwrap()),
        "News runs failing"
    );
    assert!(
        server.app.news.notify.pending.is_empty(),
        "the dropped one is gone"
    );
    assert_eq!(
        server.app.news.notify.delivered, 2,
        "a dropped one is not charged"
    );

    // A high one passes the cap once, then nothing.
    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: urgent", true));
    assert!(server.flush_news_notifications(now + 3 * second));
    assert_eq!(
        title_of(shell_control.recv_timeout(second).unwrap()),
        "News: urgent"
    );
    assert_eq!(server.app.news.notify.delivered, 3);
    assert!(server.app.news.notify.high_extra_used);
    server
        .app
        .news
        .notify
        .pending
        .push(pending("run", "News: urgent 2", true));
    assert!(!server.flush_news_notifications(now + 4 * second));
    assert!(server.app.news.notify.pending.is_empty());
    assert!(shell_control
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    assert_eq!(server.app.news.notify.delivered, 3);
}

/// `browser.resolve_caller` turns a pane id into an actor that names the
/// pane's tab, and `browser.get` reports a tab the ledger holds with that
/// actor, marking it `gone` once the pane no longer exists. The hub is
/// process-global; the tab is seeded straight into its ledger (no sidecar).
#[cfg(unix)]
#[tokio::test]
async fn browser_get_reports_a_fake_host_tab_with_its_actor() {
    use crate::api::schema::{BrowserActor, BrowserCaller, BrowserGetParams};
    use crate::browser::state::{HostTab, TabKey};

    let (mut server, _rx) = server_with_claude(None);
    server.app.state.workspaces[0].tabs[0].custom_name = Some("planner".into());
    let root_pane = server.app.state.workspaces[0].tabs[0].root_pane;
    let pane_id = server.app.public_pane_id(0, root_pane).unwrap();

    let resolved = api(
        &mut server,
        Method::BrowserResolveCaller(BrowserCaller {
            pane_id: pane_id.clone(),
        }),
    );
    assert_eq!(resolved["result"]["type"], "browser_actor", "{resolved}");
    let actor: BrowserActor = serde_json::from_value(resolved["result"]["actor"].clone()).unwrap();
    assert_eq!(actor.pane_id(), Some(pane_id.as_str()));
    assert_eq!(actor.label(), "planner · claude");

    let hub = crate::browser::hub();
    let key = TabKey::new("main", "SMOKE-T");
    hub.with_state_mut(|state| {
        state.adopt_tab(
            &key,
            &HostTab {
                target: "SMOKE-T".into(),
                url: "https://github.com/herdr/pull/412".into(),
                title: "PR".into(),
                ..Default::default()
            },
            &actor,
            1,
        );
        state.touch(
            "main",
            Some(&key),
            &actor,
            "read",
            "markdown 20k/48k",
            true,
            300,
            2,
        );
    });
    // A cursor stored with a stale herdr tab is re-resolved through the pane.
    hub.with_state_mut(|state| state.set_cursor(&pane_id, &key, Some("w9:t9"), 3));
    let got = api(&mut server, Method::BrowserGet(BrowserGetParams::default()));
    assert_eq!(got["result"]["type"], "browser_get", "{got}");
    let cursor = got["result"]["browser"]["recent_panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|cursor| cursor["pane_id"] == pane_id)
        .expect("the pane's cursor")
        .clone();
    assert_eq!(cursor["tab_id"], actor.tab_id().unwrap(), "{cursor}");
    let tab = got["result"]["browser"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["target_id"] == "SMOKE-T")
        .expect("the seeded tab")
        .clone();
    assert_eq!(tab["opened_by"]["kind"], "pane");
    assert_eq!(tab["opened_by"]["pane_id"], pane_id);
    assert_eq!(tab["opened_by"]["tab_label"], "planner");
    assert!(
        tab["opened_by"].get("gone").is_none(),
        "the pane exists: {tab}"
    );
    assert_eq!(tab["last"]["op"], "read");
    assert_eq!(tab["users"][0], pane_id);
    let seq = got["result"]["browser"]["seq"].as_u64().unwrap();
    let same = api(
        &mut server,
        Method::BrowserGet(BrowserGetParams {
            since_seq: Some(seq),
        }),
    );
    assert_eq!(same["result"]["browser"]["unchanged"], true, "{same}");

    // A pane id nobody has: external, and a tab whose pane is gone says so.
    let missing = api(
        &mut server,
        Method::BrowserResolveCaller(BrowserCaller {
            pane_id: "w9:p9".into(),
        }),
    );
    assert_eq!(missing["error"]["code"], "pane_not_found", "{missing}");
    hub.with_state_mut(|state| {
        let gone_actor = BrowserActor::Pane {
            pane_id: "w9:p9".into(),
            tab_id: "w9:t9".into(),
            workspace_id: "w9".into(),
            tab_label: "old".into(),
            workspace_label: None,
            agent: None,
            session: "default".into(),
            gone: false,
            shell_pid: None,
        };
        let key = TabKey::new("main", "SMOKE-GONE");
        state.adopt_tab(
            &key,
            &HostTab {
                target: "SMOKE-GONE".into(),
                ..Default::default()
            },
            &gone_actor,
            3,
        );
    });
    let got = api(&mut server, Method::BrowserGet(BrowserGetParams::default()));
    let gone = got["result"]["browser"]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["target_id"] == "SMOKE-GONE")
        .unwrap()
        .clone();
    assert_eq!(gone["opened_by"]["gone"], true, "{gone}");
}

/// The Chromium argv herdr builds never carries an automation or
/// debugging-weakening switch, whatever `[browser] extra_args` says, and
/// never `--remote-debugging-port=0` (Chromium flags that as automated).
#[test]
fn browser_launch_argv_carries_no_automation_switches() {
    let config = crate::config::BrowserConfig {
        extra_args: vec![
            "--lang=tr".into(),
            "--enable-automation".into(),
            "--remote-debugging-port=0".into(),
            "--no-sandbox".into(),
            "--headless".into(),
        ],
        ..Default::default()
    };
    let argv = crate::browser::launch::argv(
        std::path::Path::new("/tmp/profile"),
        43210,
        true,
        &config.extra_args(),
        false,
        Some(std::path::Path::new("/tmp/host/companion")),
    );
    assert!(argv.contains(&"--remote-debugging-port=43210".to_string()));
    assert!(argv.contains(&"--load-extension=/tmp/host/companion".to_string()));
    assert!(!argv.iter().any(|a| a.contains("AutomationControlled")));
    assert!(argv.contains(&"--restore-last-session".to_string()));
    assert!(argv.contains(&"--lang=tr".to_string()));
    let own: Vec<&String> = argv
        .iter()
        .filter(|arg| crate::config::is_forbidden_switch(arg))
        .collect();
    assert_eq!(
        own.len(),
        3,
        "only herdr's profile dir, port and companion: {own:?}"
    );
    assert!(own.iter().all(|arg| {
        arg.starts_with("--user-data-dir=")
            || arg.starts_with("--remote-debugging-port=")
            || arg.starts_with("--load-extension=")
    }));
    assert!(!argv.iter().any(|arg| arg == "--remote-debugging-port=0"));
    assert!(!argv
        .iter()
        .any(|arg| arg.starts_with("--use-mock-keychain")));
}

/// `browser.settings.set` writes the server's config file and reloads it;
/// `mcp_agents` runs its file-editing fix (on temporary files here); the
/// shell hook moved to the Agents section (`moved`; neither the setting nor
/// `browser.fix` touches `.zshrc`); `wrap_agents` / `steer_wrap` write
/// `[agents] wrap`; a failing file-editing check surfaces as `setup_needed`
/// in `browser.get`.
#[tokio::test(flavor = "current_thread")]
async fn browser_settings_write_the_config_and_fix_the_codex_entries_but_never_the_hook() {
    use crate::api::schema::{
        BrowserFixParams, BrowserGetParams, BrowserSettingsSetParams, EmptyParams,
    };
    use crate::browser::setup::{self, SetupEnv};
    use std::time::Duration;

    let dir = std::env::temp_dir().join(format!(
        "herdr-fork-smoke-browser-settings-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("home/.codex")).unwrap();
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("bin/herdr"), "herdr").unwrap();
    let config_path = dir.join("config.toml");
    fs::write(&config_path, "[ui]\nsidebar_layout = \"tabs\"\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
    let env = SetupEnv {
        browser_home: dir.join("browser"),
        binary: dir.join("bin/herdr"),
        shell_file: dir.join("config/shell/herdr-plus.zsh"),
        zshrc: Some(dir.join("home/.zshrc")),
        claude_json: Some(dir.join("home/.claude.json")),
        claude_bin: None,
        claude_z_bin: None,
        codex_config: Some(dir.join("home/.codex/config.toml")),
        codex_bin: None,
        home: Some(dir.join("home")),
        node_override: None,
    };
    let hub = crate::browser::hub();
    hub.set_setup_env(Some(env.clone()));
    let (mut server, _rx) = server_with_claude(None);

    // A toggle: the file gets a [browser] section, the running config follows.
    let set = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "pin_dashboard".into(),
            value: serde_json::Value::Bool(false),
        }),
    );
    assert_eq!(set["result"]["type"], "browser_settings", "{set}");
    assert_eq!(set["result"]["settings"]["pin_dashboard"], false);
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(
        text.starts_with("[ui]\nsidebar_layout = \"tabs\"\n"),
        "{text}"
    );
    assert!(text.contains("[browser]\npin_dashboard = false"), "{text}");
    assert!(!hub.config().pin_dashboard, "reloaded live");
    hub.wait_for_setup_idle(Duration::from_secs(20));

    // The colour is normalised; a bad value and an unknown key are refused.
    let set = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "activity_color".into(),
            value: serde_json::Value::String("#00C8FF".into()),
        }),
    );
    assert_eq!(
        set["result"]["settings"]["activity_color"], "#00c8ff",
        "{set}"
    );
    assert!(fs::read_to_string(&config_path)
        .unwrap()
        .contains("activity_color = \"#00c8ff\""));
    let bad = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "activity_color".into(),
            value: serde_json::Value::String("purple".into()),
        }),
    );
    assert_eq!(bad["error"]["code"], "invalid_request", "{bad}");
    let bad = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "executable".into(),
            value: serde_json::Value::String("/x".into()),
        }),
    );
    assert_eq!(bad["error"]["code"], "invalid_request", "{bad}");
    hub.wait_for_setup_idle(Duration::from_secs(20));

    // mcp_agents = ["codex"]: the Codex entry is written (for this binary), nothing for Claude.
    let set = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "mcp_agents".into(),
            value: serde_json::json!(["codex"]),
        }),
    );
    assert_eq!(
        set["result"]["settings"]["mcp_agents"],
        serde_json::json!(["codex"]),
        "{set}"
    );
    assert!(
        hub.wait_for_setup_idle(Duration::from_secs(30)),
        "the fix finishes"
    );
    let codex = fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
    assert!(codex.contains("[mcp_servers.herdr-browser]"), "{codex}");
    assert!(
        codex.contains(&dir.join("bin/herdr").display().to_string()),
        "{codex}"
    );
    assert!(!dir.join("home/.claude.json").exists());
    let settings = api(&mut server, Method::BrowserSettings(EmptyParams::default()));
    let checks = settings["result"]["settings"]["checks"].as_array().unwrap();
    let check = |id: &str| {
        checks
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no check {id} in {checks:?}"))
            .clone()
    };
    assert_eq!(check("mcp_codex")["ok"], true, "{}", check("mcp_codex"));
    assert_eq!(
        check("mcp_claude")["ok"],
        true,
        "off is fine: {}",
        check("mcp_claude")
    );
    let fixes = settings["result"]["settings"]["fixes"].as_array().unwrap();
    assert!(
        fixes
            .iter()
            .any(|f| f["id"] == "mcp_codex" && f["ok"] == true),
        "{fixes:?}"
    );
    assert_eq!(settings["result"]["settings"]["fixing"], false);
    assert!(settings["result"]["settings"]["checked_at"].is_u64());

    // shell_hook moved to Settings → Agents: refused, `.zshrc` untouched;
    // browser.fix cannot reach it either.
    let other = PathBuf::from("/Users/me/.herdr-dev/config/herdr/shell/herdr-plus.zsh");
    let zshrc_before = format!("alias x=y\n{}\n", setup::zshrc_hook_line(&other));
    fs::write(dir.join("home/.zshrc"), &zshrc_before).unwrap();
    let moved = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "shell_hook".into(),
            value: serde_json::Value::Bool(true),
        }),
    );
    assert_eq!(moved["error"]["code"], "moved", "{moved}");
    assert!(
        moved["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Settings → Agents"),
        "{moved}"
    );
    hub.refresh_checks(true);
    assert!(hub.wait_for_setup_idle(Duration::from_secs(20)));
    let settings = api(&mut server, Method::BrowserSettings(EmptyParams::default()));
    assert!(
        settings["result"]["settings"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"] != "shell_hook"),
        "{settings}"
    );
    let fix = api(
        &mut server,
        Method::BrowserFix(BrowserFixParams {
            ids: vec!["shell_hook".into()],
        }),
    );
    assert_eq!(fix["result"]["settings"]["fixing"], true, "{fix}");
    assert!(hub.wait_for_setup_idle(Duration::from_secs(30)));
    let fix = api(
        &mut server,
        Method::BrowserFix(BrowserFixParams { ids: vec![] }),
    );
    assert!(fix["result"]["settings"].is_object(), "{fix}");
    assert!(hub.wait_for_setup_idle(Duration::from_secs(30)));
    assert_eq!(
        fs::read_to_string(dir.join("home/.zshrc")).unwrap(),
        zshrc_before,
        "byte-identical: no browser path edits .zshrc"
    );
    assert!(!env.shell_file.exists());
    assert!(!dir.join("home/.zshrc.herdr-backup").exists());

    // A failing file-editing check (a stale Codex entry) is `setup_needed`.
    let got = api(&mut server, Method::BrowserGet(BrowserGetParams::default()));
    assert_eq!(got["result"]["browser"]["setup_needed"], false, "{got}");
    let codex = fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
    fs::write(
        dir.join("home/.codex/config.toml"),
        codex.replace(
            &dir.join("bin/herdr").display().to_string(),
            "/opt/other/herdr",
        ),
    )
    .unwrap();
    hub.refresh_checks(true);
    assert!(hub.wait_for_setup_idle(Duration::from_secs(20)));
    let got = api(&mut server, Method::BrowserGet(BrowserGetParams::default()));
    assert_eq!(got["result"]["browser"]["setup_needed"], true, "{got}");
    fs::write(dir.join("home/.codex/config.toml"), codex).unwrap();
    hub.refresh_checks(true);
    assert!(hub.wait_for_setup_idle(Duration::from_secs(20)));

    // wrap_agents: the legacy key is read as the fallback; setting it
    // writes [agents] wrap and drops the legacy key in one write.
    let text = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        text.replace("[browser]\n", "[browser]\nwrap_agents = true\n"),
    )
    .unwrap();
    server.app.reload_config();
    let settings = api(&mut server, Method::BrowserSettings(EmptyParams::default()));
    assert_eq!(
        settings["result"]["settings"]["wrap_agents"], true,
        "the effective value from the legacy key: {settings}"
    );
    let set = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "wrap_agents".into(),
            value: serde_json::Value::Bool(false),
        }),
    );
    assert_eq!(set["result"]["settings"]["wrap_agents"], false, "{set}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[agents]\nwrap = false"), "{text}");
    assert!(!text.contains("wrap_agents"), "{text}");
    hub.wait_for_setup_idle(Duration::from_secs(20));

    // steer + wrap: steer_agents and [agents] wrap in one file write.
    let set = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "steer_wrap".into(),
            value: serde_json::Value::Bool(true),
        }),
    );
    assert_eq!(set["result"]["settings"]["steer_agents"], true, "{set}");
    assert_eq!(set["result"]["settings"]["wrap_agents"], true, "{set}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("steer_agents = true") && text.contains("[agents]\nwrap = true"),
        "{text}"
    );
    assert!(!text.contains("wrap_agents"), "{text}");
    assert!(hub.config().effective_wrap);
    hub.wait_for_setup_idle(Duration::from_secs(20));

    // A request during a running fix is queued and drained by the same worker.
    hub.run_fixes(vec!["mcp_codex".into()]);
    hub.run_fixes(vec!["mcp_claude".into()]);
    let (fixing, pending, _) = hub.setup_test_state();
    assert!(fixing);
    assert_eq!(
        pending.as_deref(),
        Some(&["mcp_claude".to_string()][..]),
        "queued behind the running fix"
    );
    hub.run_fixes(vec![]);
    assert_eq!(
        hub.setup_test_state().1.as_deref(),
        Some(&[][..]),
        "an all request absorbs the named ones"
    );
    assert!(hub.wait_for_setup_idle(Duration::from_secs(40)));
    let settings = api(&mut server, Method::BrowserSettings(EmptyParams::default()));
    let fixes = settings["result"]["settings"]["fixes"].as_array().unwrap();
    assert!(
        fixes.iter().any(|f| f["id"] == "mcp_codex"),
        "the first batch: {fixes:?}"
    );
    assert!(hub.setup_test_state().1.is_none());
    // a check result from before a fix is discarded
    let (_, _, generation) = hub.setup_test_state();
    hub.setup_test_bump();
    assert!(
        !hub.setup_test_store(Vec::new(), Some(generation)),
        "stale generation dropped"
    );
    assert!(hub.setup_test_store(Vec::new(), Some(generation + 1)));
    hub.refresh_checks(true);
    assert!(hub.wait_for_setup_idle(Duration::from_secs(20)));

    // A write the reload cannot apply (an invalid [browser] section) is an error and runs no fix.
    let text = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        text.replace("[browser]\n", "[browser]\nenabled = \"maybe\"\n"),
    )
    .unwrap();
    let codex_before = fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
    let refused = api(
        &mut server,
        Method::BrowserSettingsSet(BrowserSettingsSetParams {
            key: "mcp_agents".into(),
            value: serde_json::json!([]),
        }),
    );
    assert_eq!(
        refused["error"]["code"], "browser_config_write_failed",
        "{refused}"
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("written but not applied"),
        "{refused}"
    );
    assert_eq!(
        hub.config().mcp_agents,
        ["codex"],
        "the running config kept the old value"
    );
    hub.wait_for_setup_idle(Duration::from_secs(20));
    assert_eq!(
        fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap(),
        codex_before,
        "no fix ran on the old setting"
    );
    assert_eq!(
        fs::read_to_string(dir.join("home/.zshrc")).unwrap(),
        zshrc_before,
        "and the hook was never touched"
    );

    hub.set_setup_env(None);
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
/// Every `endpoint.voice.v1` payload in a client's control stream so far.
fn voice_payloads(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<crate::server::headless::voice::VoicePayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::server::headless::voice::VOICE_KIND {
                payloads.push(
                    crate::server::headless::voice::VoicePayload::decode(&data)
                        .expect("voice payload decodes"),
                );
            }
        }
    }
    payloads
}

fn report_voice(server: &mut HeadlessServer, voice: crate::detect::AgentVoice) {
    let pane_id = root_pane(server);
    server.app.handle_internal_event(AppEvent::StateChanged {
        pane_id,
        agent: Some(Agent::Claude),
        state: AgentState::Idle,
        visible_blocker: false,
        visible_working: false,
        voice,
        process_exited: false,
        observed_at: std::time::Instant::now(),
    });
}

/// A detected voice mode reaches every client shell once as
/// `endpoint.voice.v1`, keyed by the same pane id the shell snapshot's agent
/// row carries; an unchanged revision sends nothing and the agent stays idle.
#[tokio::test]
async fn fork_smoke_voice_mode_reaches_every_client_keyed_like_the_agent_row() {
    let (mut server, _rx) = server_with_claude(None);
    let (first, _first_render) = connect_test_shell(&mut server, 61, 80, 23);
    let (second, _second_render) = connect_test_shell(&mut server, 62, 80, 23);
    server.render_and_stream();
    assert!(
        voice_payloads(&first).is_empty() && voice_payloads(&second).is_empty(),
        "a server that never saw a voice mode sends nothing"
    );

    report_voice(&mut server, crate::detect::AgentVoice::Live);
    server.render_and_stream();
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let agent_row = snapshot.agents[0].pane_id.clone();
    assert_eq!(
        snapshot.agents[0].agent_status,
        AgentStatus::Idle,
        "a listening agent stays idle"
    );
    for control in [&first, &second] {
        let payloads = voice_payloads(control);
        assert_eq!(payloads.len(), 1, "one payload per client");
        assert_eq!(payloads[0].panes.len(), 1);
        assert_eq!(payloads[0].panes[0].pane_id, agent_row);
        assert_eq!(
            payloads[0].panes[0].voice,
            crate::api::schema::AgentVoiceMode::Live
        );
    }

    // Nothing changed: no payload. Muted, then off: one each.
    report_voice(&mut server, crate::detect::AgentVoice::Live);
    server.render_and_stream();
    assert!(voice_payloads(&first).is_empty());
    report_voice(&mut server, crate::detect::AgentVoice::Muted);
    server.render_and_stream();
    let muted = voice_payloads(&first);
    assert_eq!(muted.len(), 1);
    assert_eq!(
        muted[0].panes[0].voice,
        crate::api::schema::AgentVoiceMode::Muted
    );
    report_voice(&mut server, crate::detect::AgentVoice::Off);
    server.render_and_stream();
    let off = voice_payloads(&first);
    assert_eq!(off.len(), 1);
    assert!(off[0].panes.is_empty(), "voice mode off lists no pane");
}

fn agent_times_payloads(control: &std::sync::mpsc::Receiver<Vec<u8>>) -> Vec<(String, String)> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::server::headless::agent_times::AGENT_TIMES_KIND {
                payloads.push((kind, data));
            }
        }
    }
    payloads
}

fn report_state(server: &mut HeadlessServer, state: AgentState) {
    let pane_id = root_pane(server);
    server.app.handle_internal_event(AppEvent::StateChanged {
        pane_id,
        agent: Some(Agent::Claude),
        state,
        visible_blocker: false,
        visible_working: state == AgentState::Working,
        voice: crate::detect::AgentVoice::Off,
        process_exited: false,
        observed_at: std::time::Instant::now(),
    });
}

/// Fork (sidebar v2): an agent state change reaches the client shell as
/// `endpoint.agent-times.v1`, keyed by the pane id and `state_change_seq` the
/// shell snapshot's agent row carries, and the client accepts it; an
/// unchanged revision sends nothing.
#[tokio::test]
async fn agent_times_push_reaches_the_client_shell() {
    let (mut server, _rx) = server_with_claude(None);
    let (control, _render) = connect_test_shell(&mut server, 71, 80, 23);
    server.render_and_stream();
    assert!(
        agent_times_payloads(&control).is_empty(),
        "a server that never saw a state change sends nothing"
    );

    let before = crate::codex_sessions::now_unix_ms();
    report_state(&mut server, AgentState::Working);
    server.render_and_stream();
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let row = &snapshot.agents[0];
    assert_eq!(row.agent_status, AgentStatus::Working);
    let payloads = agent_times_payloads(&control);
    assert_eq!(payloads.len(), 1, "one payload per change");
    let (kind, data) = &payloads[0];
    let crate::client::endpoint::EndpointControlMessage::AgentTimes(payload) =
        crate::client::endpoint::decode_endpoint_control(kind, data).expect("decodes")
    else {
        panic!("an agent times control message");
    };
    assert_eq!(payload.panes.len(), 1);
    assert_eq!(
        payload.panes[0].pane_id, row.pane_id,
        "keyed like the agent row"
    );
    assert_eq!(payload.panes[0].state_change_seq, row.state_change_seq);
    assert!(payload.panes[0].since_unix_ms >= before);
    assert!(payload.server_now_unix_ms >= payload.panes[0].since_unix_ms);

    // Nothing changed: no payload.
    report_state(&mut server, AgentState::Working);
    server.render_and_stream();
    assert!(agent_times_payloads(&control).is_empty());

    // The client shell takes it once (a repeat of the revision is ignored).
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    assert!(shell.receive_agent_times(
        &crate::client::endpoint::ClientEndpointId::Local,
        payload.clone()
    ));
    assert!(!shell.receive_agent_times(&crate::client::endpoint::ClientEndpointId::Local, payload));
}

/// Every `endpoint.tab-pins.v1` payload in a client's control stream so far.
fn tab_pins_payloads(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<crate::server::headless::tab_pins::TabPinsPayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::server::headless::tab_pins::TAB_PINS_KIND {
                payloads.push(
                    crate::server::headless::tab_pins::TabPinsPayload::decode(&data)
                        .expect("tab pins payload decodes"),
                );
            }
        }
    }
    payloads
}

/// Fork (sidebar v3): `tab.set_pinned` reaches every client shell once as
/// `endpoint.tab-pins.v1`, keyed like the shell snapshot's tab row; a
/// whole-tab move carries the pin and re-sends the list under the tab's new
/// public id; the client shell takes it.
#[tokio::test]
async fn tab_pin_reaches_every_client_and_survives_a_move() {
    let (mut server, _rx) = server_with_claude(None);
    server
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("other"));
    server.app.state.ensure_test_terminals();
    let (first, _first_render) = connect_test_shell(&mut server, 81, 80, 23);
    let (second, _second_render) = connect_test_shell(&mut server, 82, 80, 23);
    server.render_and_stream();
    assert!(
        tab_pins_payloads(&first).is_empty() && tab_pins_payloads(&second).is_empty(),
        "a server that never had a pin sends nothing"
    );

    let tab_id = server.app.public_tab_id(0, 0).expect("tab id");
    let reply = api(
        &mut server,
        Method::TabSetPinned(crate::api::schema::TabSetPinnedParams {
            tab_id: tab_id.clone(),
            pinned: true,
        }),
    );
    assert_eq!(reply["result"]["tab"]["pinned"], true, "{reply}");
    server.render_and_stream();
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    assert!(snapshot.tabs.iter().any(|tab| tab.tab_id == tab_id));
    for control in [&first, &second] {
        let payloads = tab_pins_payloads(control);
        assert_eq!(payloads.len(), 1, "one payload per client");
        assert_eq!(
            payloads[0].tab_ids,
            std::slice::from_ref(&tab_id),
            "keyed like the tab row"
        );
    }
    server.render_and_stream();
    assert!(
        tab_pins_payloads(&first).is_empty(),
        "an unchanged revision sends nothing"
    );

    // The whole tab moves to the other group: the pin goes with it.
    let pane_id = server
        .app
        .public_pane_id(0, root_pane(&server))
        .expect("pane id");
    let target = server.app.public_workspace_id(1);
    let moved = api(
        &mut server,
        Method::PaneMove(crate::api::schema::PaneMoveParams {
            pane_id,
            destination: crate::api::schema::PaneMoveDestination::NewTab {
                workspace_id: Some(target),
                label: Some("moved".into()),
            },
            focus: false,
        }),
    );
    assert!(moved.get("error").is_none(), "{moved}");
    server.render_and_stream();
    let payloads = tab_pins_payloads(&first);
    let last = payloads.last().expect("the move re-sends the list");
    assert_eq!(last.tab_ids.len(), 1);
    assert_ne!(last.tab_ids[0], tab_id, "the moved tab has a new public id");
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let moved_row = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == last.tab_ids[0])
        .expect("the pinned id is a tab row");
    assert_eq!(moved_row.label, "moved");

    // The client shell takes it once.
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let local = crate::client::endpoint::ClientEndpointId::Local;
    assert!(shell.receive_tab_pins(&local, last.clone()));
    assert!(!shell.receive_tab_pins(&local, last.clone()));
}

/// Every message a client's control stream delivers until it has been quiet
/// for a while. The test writer forwards through a drain thread that blocks
/// on the one-slot render channel, so the render frames are drained too.
fn control_messages(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
    render: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<ServerMessage> {
    let mut messages = Vec::new();
    let mut quiet = 0;
    while quiet < 3 {
        while render.try_recv().is_ok() {}
        match control.recv_timeout(Duration::from_millis(50)) {
            Ok(bytes) => {
                messages.push(read_server_message(bytes));
                quiet = 0;
            }
            Err(_) => quiet += 1,
        }
    }
    messages
}

/// The `endpoint.tab-mutes.v1` payloads among `messages`.
fn tab_mutes_payloads(
    messages: &[ServerMessage],
) -> Vec<crate::server::headless::tab_mutes::TabMutesPayload> {
    messages
        .iter()
        .filter_map(|message| match message {
            ServerMessage::EndpointControl { kind, data }
                if kind == crate::server::headless::tab_mutes::TAB_MUTES_KIND =>
            {
                Some(
                    crate::server::headless::tab_mutes::TabMutesPayload::decode(data)
                        .expect("tab mutes payload decodes"),
                )
            }
            _ => None,
        })
        .collect()
}

/// Whether `messages` hold a notification or a bell (what a mute silences).
fn rings(messages: &[ServerMessage]) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            ServerMessage::SemanticNotification(_)
                | ServerMessage::TerminalBell { .. }
                | ServerMessage::Notify { .. }
        )
    })
}

/// Fork: `tab.set_muted` reaches every client shell once as
/// `endpoint.tab-mutes.v1`, keyed like the shell snapshot's tab row; the
/// server raises no Finished / NeedsAttention notification and forwards no
/// bell for the muted tab; a whole-tab move carries the mute and re-sends
/// the list under the tab's new public id; the client shell takes it.
#[tokio::test]
async fn fork_smoke_tab_mute_reaches_every_client_silences_the_tab_and_survives_a_move() {
    let (mut server, _rx) = server_with_claude(None);
    server
        .app
        .state
        .workspaces
        .push(crate::workspace::Workspace::test_new("other"));
    server.app.state.ensure_test_terminals();
    let (first, first_render) = connect_test_shell(&mut server, 81, 80, 23);
    let (second, second_render) = connect_test_shell(&mut server, 82, 80, 23);
    server.render_and_stream();
    assert!(
        tab_mutes_payloads(&control_messages(&first, &first_render)).is_empty()
            && tab_mutes_payloads(&control_messages(&second, &second_render)).is_empty(),
        "a server that never had a mute sends nothing"
    );

    // Unmuted, the agent's NeedsAttention and a bell reach the clients.
    let pane = root_pane(&server);
    assert!(server.forward_semantic_agent_transition(
        0,
        pane,
        AgentState::Working,
        AgentState::Blocked,
        Some("claude"),
        Some("claude"),
        Some(Agent::Claude),
    ));
    server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
        pane_id: pane,
        count: 1,
    });
    let mut heard = control_messages(&first, &first_render);
    heard.extend(control_messages(&second, &second_render));
    assert!(rings(&heard), "an unmuted tab rings");

    let tab_id = server.app.public_tab_id(0, 0).expect("tab id");
    let reply = api(
        &mut server,
        Method::TabSetMuted(crate::api::schema::TabSetMutedParams {
            tab_id: tab_id.clone(),
            muted: true,
        }),
    );
    assert_eq!(reply["result"]["tab"]["muted"], true, "{reply}");
    server.render_and_stream();
    for (control, render) in [(&first, &first_render), (&second, &second_render)] {
        let payloads = tab_mutes_payloads(&control_messages(control, render));
        assert_eq!(payloads.len(), 1, "one payload per client");
        assert_eq!(
            payloads[0].tab_ids,
            std::slice::from_ref(&tab_id),
            "keyed like the tab row"
        );
    }
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    assert!(snapshot.tabs.iter().any(|tab| tab.tab_id == tab_id));

    // Muted: the same transition and bell raise nothing.
    assert!(!server.forward_semantic_agent_transition(
        0,
        pane,
        AgentState::Working,
        AgentState::Blocked,
        Some("claude"),
        Some("claude"),
        Some(Agent::Claude),
    ));
    server.handle_internal_event_with_forwarding(AppEvent::TerminalBell {
        pane_id: pane,
        count: 1,
    });
    server.render_and_stream();
    let mut heard = control_messages(&first, &first_render);
    heard.extend(control_messages(&second, &second_render));
    assert!(!rings(&heard), "a muted tab stays silent: {heard:?}");
    assert!(
        tab_mutes_payloads(&heard).is_empty(),
        "an unchanged revision sends nothing"
    );

    // The whole tab moves to the other group: the mute goes with it.
    let pane_id = server.app.public_pane_id(0, pane).expect("pane id");
    let target = server.app.public_workspace_id(1);
    let moved = api(
        &mut server,
        Method::PaneMove(crate::api::schema::PaneMoveParams {
            pane_id,
            destination: crate::api::schema::PaneMoveDestination::NewTab {
                workspace_id: Some(target),
                label: Some("moved".into()),
            },
            focus: false,
        }),
    );
    assert!(moved.get("error").is_none(), "{moved}");
    server.render_and_stream();
    let payloads = tab_mutes_payloads(&control_messages(&first, &first_render));
    let last = payloads.last().expect("the move re-sends the list");
    assert_eq!(last.tab_ids.len(), 1);
    assert_ne!(last.tab_ids[0], tab_id, "the moved tab has a new public id");
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let moved_row = snapshot
        .tabs
        .iter()
        .find(|tab| tab.tab_id == last.tab_ids[0])
        .expect("the muted id is a tab row");
    assert_eq!(moved_row.label, "moved");
    assert!(
        server.app.state.pane_notifications_muted(pane),
        "the moved pane is still muted"
    );

    // The client shell takes it once.
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let local = crate::client::endpoint::ClientEndpointId::Local;
    assert!(shell.receive_tab_mutes(&local, last.clone()));
    assert!(!shell.receive_tab_mutes(&local, last.clone()));
}

/// Every `endpoint.agent-context.v1` payload in a client's control stream so far.
fn agent_context_payloads(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<crate::server::headless::agent_context::AgentContextPayload> {
    let mut payloads = Vec::new();
    while let Ok(bytes) = control.try_recv() {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::server::headless::agent_context::AGENT_CONTEXT_KIND {
                let crate::client::endpoint::EndpointControlMessage::AgentContext(payload) =
                    crate::client::endpoint::decode_endpoint_control(&kind, &data)
                        .expect("decodes")
                else {
                    panic!("an agent context control message");
                };
                payloads.push(payload);
            }
        }
    }
    payloads
}

/// Fork: an agent's context use read from its session file reaches the
/// client shell once as `endpoint.agent-context.v1`, keyed like the shell
/// snapshot's agent row, and `agent.list` carries it as `AgentInfo.context`;
/// an unchanged value sends nothing; the client shell takes it.
#[tokio::test]
async fn fork_smoke_agent_context_push_reaches_the_client_shell() {
    let (mut server, _rx) = server_with_claude(Some(SESSION_ID));
    let (control, _render) = connect_test_shell(&mut server, 72, 80, 23);
    server.render_and_stream();
    assert!(
        agent_context_payloads(&control).is_empty(),
        "a server that never read a context use sends nothing"
    );
    // The tick finds the Claude pane due (its session file read off-thread).
    let (requests, _) = server.app.agent_context_requests(std::time::Instant::now());
    assert_eq!(requests.len(), 1, "the live Claude pane is due");

    let terminal_id = root_terminal_id(&server);
    let usage = crate::agent_context::TranscriptUsage {
        used: 164_000,
        model: Some("claude-opus-5-5".into()),
        window: None,
    };
    assert!(server.app.apply_agent_context_results(vec![(
        terminal_id.clone(),
        (
            crate::agent_context::ContextAgent::Claude,
            SESSION_ID.into()
        ),
        Some(usage.clone()),
    )]));
    server.render_and_stream();
    let snapshot = crate::server::client_shell::snapshot(&server.app, "fork-smoke", 1, None, None);
    let payloads = agent_context_payloads(&control);
    assert_eq!(payloads.len(), 1, "one payload per change");
    let payload = payloads[0].clone();
    assert_eq!(payload.panes.len(), 1);
    assert_eq!(payload.panes[0].pane_id, snapshot.agents[0].pane_id);
    assert_eq!(
        (payload.panes[0].used, payload.panes[0].window),
        (164_000, 200_000)
    );

    let list = api(
        &mut server,
        Method::AgentList(crate::api::schema::EmptyParams::default()),
    );
    let context = &list["result"]["agents"][0]["context"];
    assert_eq!(context["used_tokens"], 164_000, "{list}");
    assert_eq!(context["window_tokens"], 200_000, "{list}");
    assert_eq!(context["percent"], 82, "{list}");

    // The same value again: no payload.
    server.app.apply_agent_context_results(vec![(
        terminal_id,
        (
            crate::agent_context::ContextAgent::Claude,
            SESSION_ID.into(),
        ),
        Some(usage),
    )]);
    server.render_and_stream();
    assert!(agent_context_payloads(&control).is_empty());

    // The client shell takes it once.
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let local = crate::client::endpoint::ClientEndpointId::Local;
    assert!(shell.receive_agent_context(&local, payload.clone()));
    assert!(!shell.receive_agent_context(&local, payload));
}

#[path = "fork_smoke/coordinator.rs"]
mod coordinator;

#[path = "fork_smoke/notes.rs"]
mod notes;
