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
