//! Coordinator fork smoke tests, server side (a child of `fork_smoke`, so the
//! gate's `-E 'test(fork_smoke)'` filter runs them). Each one locks a point
//! where the coordinator hooks into an upstream path: the managed-agent
//! launch, prompt delivery, client-shell notification delivery, the
//! completion toast and the herdr-served dashboard.
//!
//! The coordinator runs a stub `claude` (`stub_claude.sh`): the in-process
//! test server has no API socket, so the stub runs against a stand-in socket
//! (as the Claude hook asset tests do) and the request it sent is replayed
//! into the server. Its `pane.report_agent` reports settle herdr's own
//! managed-agent launch state, which the coordinator's phase machine follows.
//! Each test uses a private coordinator dir and a private or free port; none
//! touches the user's config dir or a live server.

use super::*;
use crate::api::schema::coordinator::{CoordinatorOpenDashboardParams, CoordinatorWakeParams};
use crate::api::schema::EmptyParams;

const STUB: &str = include_str!("stub_claude.sh");

/// How long a test waits for the coordinator worker (lock, seeding, the
/// dashboard bind) before it fails.
const WORKER_WAIT: Duration = Duration::from_secs(10);

/// A private scratch dir, removed first.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hfs-coord-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A loopback port nobody listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("a free loopback port")
}

/// A headless server with the fork-smoke Claude pane (tab 0, focused), a
/// `/bin/cat` shell for new panes (nothing typed into them runs) and the
/// coordinator pointed at `dir/coordinator`.
fn coordinator_server(
    dir: &std::path::Path,
) -> (HeadlessServer, tokio::sync::mpsc::Receiver<Bytes>) {
    let (mut server, rx) = server_with_claude(None);
    server.app.state.default_shell = "/bin/cat".into();
    server.app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    server.app.state.workspaces[0].tabs[0].custom_name = Some("work".into());
    server.app.state.toast_config.delay_seconds = 0;
    server.app.coordinator.dir = dir.join("coordinator");
    // `/bin/cat` is not a shell prompt: let `start_agent` type into it.
    server.app.coordinator.assume_shell_ready = true;
    (server, rx)
}

/// Turn the coordinator on the way a config reload does.
fn enable(server: &mut HeadlessServer, dashboard_port: u16) {
    server
        .app
        .apply_coordinator_config(&crate::config::CoordinatorConfig {
            enabled: true,
            dashboard_port,
            ..crate::config::CoordinatorConfig::default()
        });
}

fn disable(server: &mut HeadlessServer) {
    server
        .app
        .apply_coordinator_config(&crate::config::CoordinatorConfig::default());
}

/// `coordinator.get`'s read model.
fn get(server: &mut HeadlessServer) -> serde_json::Value {
    let response = api(server, Method::CoordinatorGet(EmptyParams::default()));
    assert_eq!(response["result"]["type"], "coordinator_get", "{response}");
    response["result"]["info"].clone()
}

/// Run the headless loop's coordinator work (worker replies, the phase
/// machine, notification flushes) until `done` holds or the wait runs out.
fn pump_until(
    server: &mut HeadlessServer,
    what: &str,
    mut done: impl FnMut(&mut HeadlessServer) -> bool,
) {
    let deadline = std::time::Instant::now() + WORKER_WAIT;
    loop {
        server.drain_internal_events_with_forwarding();
        server.handle_scheduled_tasks_headless(std::time::Instant::now(), false);
        if done(server) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}: {}",
            get(server)
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The coordinator tab's indices, found by its label.
fn coordinator_tab(server: &HeadlessServer) -> Option<usize> {
    server.app.state.workspaces[0]
        .tabs
        .iter()
        .position(|tab| tab.custom_name.as_deref() == Some("coordinator"))
}

fn coordinator_pane_id(server: &HeadlessServer) -> String {
    let tab = coordinator_tab(server).expect("the coordinator tab");
    let pane = server.app.state.workspaces[0].tabs[tab].root_pane;
    server
        .app
        .public_pane_id(0, pane)
        .expect("the coordinator pane's public id")
}

fn coordinator_terminal_id(server: &HeadlessServer) -> crate::terminal::TerminalId {
    let tab = &server.app.state.workspaces[0].tabs[coordinator_tab(server).expect("tab")];
    tab.terminal_id(tab.root_pane)
        .expect("the coordinator pane's terminal")
        .clone()
}

/// Run the stub `claude` with `state` for `pane_id` against a stand-in
/// socket and return the `pane.report_agent` request it sent.
fn stub_report(dir: &std::path::Path, pane_id: &str, state: &str, seq: u64) -> Request {
    use std::io::Read as _;

    let stub = dir.join("stub_claude.sh");
    fs::write(&stub, STUB).unwrap();
    let socket_path = dir.join(format!("stub-{seq}.sock"));
    let _ = fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let status = std::process::Command::new("sh")
        .arg(&stub)
        .arg(state)
        .env("HERDR_SOCKET_PATH", &socket_path)
        .env("HERDR_PANE_ID", pane_id)
        .env("HERDR_STUB_SEQ", seq.to_string())
        .status()
        .expect("run the stub claude");
    assert!(
        status.success(),
        "the stub claude failed (is python3 on PATH?)"
    );
    listener.set_nonblocking(true).unwrap();
    let (mut stream, _) = listener
        .accept()
        .expect("the stub connected to HERDR_SOCKET_PATH");
    stream.set_nonblocking(false).unwrap();
    let mut sent = String::new();
    stream.read_to_string(&mut sent).unwrap();
    let request: Request =
        serde_json::from_str(sent.lines().next().expect("one request line")).unwrap();
    assert!(
        matches!(request.method, Method::PaneReportAgent(_)),
        "unexpected stub request: {sent}"
    );
    request
}

/// Replay a stub report through the server's API path (the one that
/// forwards agent notifications to clients).
fn replay(server: &mut HeadlessServer, request: Request) {
    super::super::completion_guard_api_report(server, request.method);
}

/// Enable the coordinator and drive it to `running`: the worker takes the
/// lock, the coordinator tab appears, `start_agent` launches the stub, and
/// the stub's idle report settles the launch. Returns the coordinator's
/// public pane id.
fn running_coordinator(server: &mut HeadlessServer, dir: &std::path::Path, port: u16) -> String {
    enable(server, port);
    pump_until(server, "the coordinator launch", |server| {
        coordinator_tab(server).is_some()
            && server
                .app
                .state
                .terminals
                .get(&coordinator_terminal_id(server))
                .is_some_and(|terminal| terminal.managed_agent_launch_pending())
    });
    let pane_id = coordinator_pane_id(server);
    let report = stub_report(dir, &pane_id, "idle", 1);
    replay(server, report);
    pump_until(server, "the coordinator running", |server| {
        get(server)["state"] == "running"
    });
    pane_id
}

/// Turning the coordinator on starts it without a separate process: the
/// worker takes the lock and seeds the coordinator dir, a tab labelled
/// `coordinator` appears in the first space without taking focus, and
/// herdr's own `start_agent` launches `claude` in it as the managed agent
/// `coordinator`. The read model follows it from `starting` to `running`
/// when the launch settles.
#[cfg(unix)]
#[tokio::test]
async fn coordinator_enabled_starts_a_coordinator_in_a_pinned_tab() {
    let dir = scratch("start");
    let (mut server, _rx) = coordinator_server(&dir);
    assert_eq!(get(&mut server)["state"], "off");
    assert!(coordinator_tab(&server).is_none(), "off: no tab");

    enable(&mut server, 0);
    pump_until(&mut server, "the coordinator tab", |server| {
        coordinator_tab(server).is_some()
    });
    let workspace = &server.app.state.workspaces[0];
    assert_eq!(
        workspace.active_tab, 0,
        "the coordinator tab is not focused"
    );
    let tab_index = coordinator_tab(&server).unwrap();
    let info = get(&mut server);
    assert_eq!(info["enabled"], true);
    assert_eq!(info["state"], "starting", "{info}");
    assert_eq!(
        info["tab_id"].as_str(),
        server.app.public_tab_id(0, tab_index).as_deref()
    );
    let pane_id = coordinator_pane_id(&server);
    assert_eq!(info["pane_id"].as_str(), Some(pane_id.as_str()));
    assert!(
        dir.join("coordinator/coordinator.md").is_file(),
        "the worker seeded the coordinator dir"
    );

    let terminal = &server.app.state.terminals[&coordinator_terminal_id(&server)];
    assert_eq!(terminal.agent_name.as_deref(), Some("coordinator"));
    assert_eq!(terminal.managed_agent_kind(), Some(Agent::Claude));
    assert!(terminal.managed_agent_launch_pending());
    let runtime = server
        .app
        .terminal_runtimes
        .get(&coordinator_terminal_id(&server))
        .expect("the coordinator tab runs a shell");
    for _ in 0..80 {
        if runtime
            .snapshot_history()
            .is_some_and(|text| text.contains("herdr_agents"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let history = runtime.snapshot_history().unwrap_or_default();
    assert!(
        history.contains("claude"),
        "start_agent typed the launch: {history}"
    );
    assert!(
        history.contains("herdr_agents"),
        "the launch carries the agent MCP tools: {history}"
    );

    replay(&mut server, stub_report(&dir, &pane_id, "idle", 1));
    pump_until(&mut server, "running", |server| {
        get(server)["state"] == "running"
    });
    assert_eq!(
        server.app.state.workspaces[0].tabs.len(),
        2,
        "one coordinator tab, reused"
    );

    disable(&mut server);
    pump_until(&mut server, "off", |server| get(server)["state"] == "off");
    assert!(
        coordinator_tab(&server).is_some(),
        "disabling leaves the coordinator's pane running"
    );
    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// `coordinator.wake` reaches the coordinator pane as a herdr+ wake-up
/// prompt (digest written, turn marker set by the worker; the prompt queued
/// by the app once the pane is idle), and the coordinator itself cannot wake
/// itself during the live turn.
#[cfg(unix)]
#[tokio::test]
async fn coordinator_wake_reaches_the_coordinator_pane() {
    let dir = scratch("wake");
    let (mut server, _rx) = coordinator_server(&dir);
    let pane_id = running_coordinator(&mut server, &dir, 0);

    let woken = api(
        &mut server,
        Method::CoordinatorWake(CoordinatorWakeParams { caller_pane: None }),
    );
    assert_eq!(woken["result"]["type"], "coordinator_get", "{woken}");
    let terminal_id = coordinator_terminal_id(&server);
    pump_until(&mut server, "the wake-up prompt", |server| {
        server
            .app
            .terminal_runtimes
            .get(&terminal_id)
            .and_then(|runtime| runtime.snapshot_history())
            .is_some_and(|text| text.contains("herdr+ wake-up #"))
    });
    // The worker counts the wake once the App reports it delivered.
    pump_until(&mut server, "the wake counted", |server| {
        get(server)["wake"]["seq"]
            .as_u64()
            .is_some_and(|seq| seq >= 1)
    });
    let info = get(&mut server);
    assert_eq!(info["turn"]["source"], "wake", "{info}");
    assert!(
        dir.join("coordinator/wake")
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_some()),
        "the digest is on disk"
    );

    let refused = api(
        &mut server,
        Method::CoordinatorWake(CoordinatorWakeParams {
            caller_pane: Some(pane_id),
        }),
    );
    assert_eq!(refused["error"]["code"], "in_coordinator_turn", "{refused}");

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// A new suggestion on the coordinator's board becomes one Custom
/// notification ("coordinator: 1 new suggestion …") that waits while no
/// client shell is attached and reaches the shell when one attaches,
/// carrying the coordinator pane so a click focuses its tab. The same
/// suggestion seen again notifies nothing.
#[cfg(unix)]
#[tokio::test]
async fn coordinator_suggestion_notification_waits_for_a_client_shell() {
    let dir = scratch("suggest");
    let (mut server, _rx) = coordinator_server(&dir);
    let pane_id = running_coordinator(&mut server, &dir, 0);
    // Noon on a fixed day: never inside quiet hours.
    server.app.coordinator.local_override = Some((12 * 60, "2026-10-02"));

    let board = crate::coordinator::board_path(&dir.join("coordinator"));
    fs::write(
        &board,
        r#"{"summary":"two agents","suggestions":[{"text":"Ask rev to review lead's branch?","why":"lead finished"}]}"#,
    )
    .unwrap();
    pump_until(&mut server, "the suggestion queued", |server| {
        !server.app.coordinator.notify_ledger.pending.is_empty()
    });
    assert_eq!(get(&mut server)["unread_suggestions"], 1);
    assert!(
        !server.flush_coordinator_notifications(std::time::Instant::now()),
        "no client shell: the notification waits"
    );
    assert_eq!(server.app.coordinator.notify_ledger.pending.len(), 1);

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
    // The rate limit may hold it for a second after another notification.
    let now = std::time::Instant::now() + Duration::from_secs(2);
    assert!(server.flush_coordinator_notifications(now));
    let notification = loop {
        let message = read_server_message(
            shell_control
                .recv_timeout(Duration::from_millis(500))
                .expect("the notification reaches the shell"),
        );
        if let ServerMessage::SemanticNotification(notification) = message {
            break notification;
        }
    };
    assert_eq!(
        notification.kind,
        protocol::SemanticNotificationKind::Custom
    );
    assert!(
        notification
            .title
            .starts_with("coordinator: 1 new suggestion"),
        "{notification:?}"
    );
    assert!(
        format!("{} {:?}", notification.title, notification.body).contains("Ask rev"),
        "the first suggestion's text: {notification:?}"
    );
    assert_eq!(notification.pane_id.as_deref(), Some(pane_id.as_str()));
    assert!(server.app.coordinator.notify_ledger.pending.is_empty());

    // The coordinator rewrites the board with the same suggestion: seen.
    fs::write(
        &board,
        r#"{"summary":"still two agents","suggestions":[{"text":"Ask rev to review lead's branch?","why":"still"}]}"#,
    )
    .unwrap();
    pump_until(&mut server, "the board re-read", |server| {
        get(server)["board"]["summary"] == "still two agents"
    });
    assert!(
        server.app.coordinator.notify_ledger.pending.is_empty(),
        "a seen suggestion notifies once"
    );

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// The coordinator finishing a turn in a background tab sends no generic
/// Finished toast (the `suppress_completion` arm for its terminal), while
/// any other agent finishing in a background tab still does.
#[cfg(unix)]
#[tokio::test]
async fn coordinator_idle_sends_no_finished_toast() {
    use protocol::SemanticNotificationKind::Finished;

    let dir = scratch("toast");
    let (mut server, _rx) = coordinator_server(&dir);
    let coordinator_pane = running_coordinator(&mut server, &dir, 0);
    let (writer, control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        1,
        ClientConnection::new(
            (80, 24),
            Default::default(),
            1,
            RenderEncoding::SemanticFrame,
            Some(writer),
        ),
    );
    assert_ne!(
        Some(server.app.state.workspaces[0].active_tab),
        coordinator_tab(&server),
        "the coordinator tab is in the background"
    );

    for (seq, state) in [(2, "working"), (3, "idle")] {
        replay(
            &mut server,
            stub_report(&dir, &coordinator_pane, state, seq),
        );
    }
    server.drain_internal_events_with_forwarding();
    let notifications = super::super::completion_guard_notifications(&mut server, &control_rx);
    assert!(
        !notifications.contains(&Finished),
        "the coordinator's turn ends silently: {notifications:?}"
    );

    // Control: the work tab in the background finishes with a toast.
    let coordinator_index = coordinator_tab(&server).unwrap();
    server.app.state.workspaces[0].active_tab = coordinator_index;
    let worker_pane = server
        .app
        .public_pane_id(0, root_pane(&server))
        .expect("the work pane");
    for (seq, state) in [(1, "working"), (2, "idle")] {
        replay(&mut server, stub_report(&dir, &worker_pane, state, seq));
    }
    server.drain_internal_events_with_forwarding();
    let notifications = super::super::completion_guard_notifications(&mut server, &control_rx);
    assert!(
        notifications.contains(&Finished),
        "control: other agents still toast: {notifications:?}"
    );

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}

/// One HTTP/1.0 GET against the dashboard; the status line and the body.
fn http_get(port: u16, path: &str) -> (String, String) {
    use std::io::{Read as _, Write as _};

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("the dashboard listens");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").expect("an HTTP response");
    (
        head.lines().next().unwrap_or_default().to_string(),
        body.to_string(),
    )
}

/// The herdr server itself serves the coordinator's dashboard on
/// `[coordinator] dashboard_port` once its worker holds the lock: the read
/// model carries the URL, `/board.json` is the coordinator's board and
/// `/live.json` herdr's live file. `open_dashboard {open: false}` answers
/// the URL without opening anything. Disabling the coordinator releases
/// the port.
#[cfg(unix)]
#[tokio::test]
async fn coordinator_dashboard_serves_board_json() {
    let dir = scratch("dashboard");
    let (mut server, _rx) = coordinator_server(&dir);
    let port = free_port();
    enable(&mut server, port);
    pump_until(&mut server, "the dashboard listening", |server| {
        get(server)["dashboard_url"].is_string()
    });
    let url = format!("http://127.0.0.1:{port}/");
    assert_eq!(get(&mut server)["dashboard_url"], url.as_str());

    fs::write(
        crate::coordinator::board_path(&dir.join("coordinator")),
        r#"{"summary":"fork smoke board"}"#,
    )
    .unwrap();
    let (status, body) = http_get(port, "/board.json");
    assert!(status.contains("200"), "{status}");
    assert!(body.contains("fork smoke board"), "{body}");
    pump_until(&mut server, "live.json written", |_| {
        crate::coordinator::live_path(&dir.join("coordinator")).is_file()
    });
    let (status, body) = http_get(port, "/live.json");
    assert!(status.contains("200"), "{status}");
    serde_json::from_str::<serde_json::Value>(&body).expect("live.json is JSON");
    let (status, _) = http_get(port, "/../managed.json");
    assert!(
        !status.contains("200"),
        "no traversal out of the dir: {status}"
    );

    let opened = api(
        &mut server,
        Method::CoordinatorOpenDashboard(CoordinatorOpenDashboardParams { open: false }),
    );
    assert_eq!(
        opened["result"]["info"]["dashboard_url"],
        url.as_str(),
        "{opened}"
    );

    disable(&mut server);
    pump_until(&mut server, "the port released", |_| {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    });
    let refused = api(
        &mut server,
        Method::CoordinatorOpenDashboard(CoordinatorOpenDashboardParams { open: false }),
    );
    assert_eq!(
        refused["error"]["code"], "coordinator_disabled",
        "{refused}"
    );

    shutdown_test_runtimes(&mut server);
    let _ = fs::remove_dir_all(&dir);
}
