use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::agent_wrap::team::TeamLaunch;
use crate::api::schema::agents_model::AgentsOpenTabParams;
use crate::api::schema::Method;
use crate::app::agents_model::tests::{model_app, pane, public, user_turn};
use crate::config::Config;
use crate::coordinator::launch::{self, ClaudeSession, LaunchCtx};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "herdr-launch-gate-{name}-{}-{}",
        std::process::id(),
        launch::new_uuid()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// One space, one tab, its shell swapped for a channel runtime.
fn launch_app(dir: &Path) -> (App, String, tokio::sync::mpsc::Receiver<Bytes>) {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![crate::workspace::Workspace::test_new("launch")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.ensure_test_terminals();
    app.coordinator.dir = dir.to_path_buf();
    app.agents_model.dir = Some(dir.to_path_buf());
    let root = app.state.workspaces[0].tabs[0].root_pane;
    let id = app.state.workspaces[0]
        .terminal_id(root)
        .expect("terminal")
        .clone();
    let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
    app.terminal_runtimes.insert(id, runtime);
    let pane_id = app.public_pane_id(0, root).expect("public id");
    (app, pane_id, rx)
}

fn long_kickoff() -> String {
    "Investigate the flaky shell readiness in the new tab, then report back. ".repeat(12)
}

fn roster() -> TeamLaunch {
    TeamLaunch {
        text: (1..=8)
            .map(|n| format!("- member{n} (pane w1:p{n}): reviewer of the launch path {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Every herdr+ launch shape, with a long kickoff and a team roster.
fn launches(ctx: &LaunchCtx) -> Vec<(&'static str, &'static str, Vec<String>)> {
    let kickoff = long_kickoff();
    let team = roster();
    let session = ClaudeSession::New(launch::new_uuid());
    vec![
        (
            "claude managed",
            "claude",
            launch::claude_args(ctx, &session, false, Some(&kickoff)).expect("claude args"),
        ),
        (
            "claude team",
            "claude",
            launch::claude_args_with_team(ctx, &session, false, Some(&kickoff), Some(&team))
                .expect("claude team args"),
        ),
        (
            "coordinator",
            "claude",
            crate::coordinator::engine::coordinator_launch_args(ctx, None, Some("opus"))
                .expect("coordinator args")
                .1,
        ),
        (
            "codex managed",
            "codex",
            launch::codex_args(ctx, Some(&kickoff)),
        ),
        (
            "codex team",
            "codex",
            launch::codex_args_with_team(ctx, Some(&kickoff), Some(&team)),
        ),
    ]
}

#[tokio::test]
async fn fork_smoke_every_herdr_launch_types_a_short_line_that_runs_the_full_command() {
    for (what, kind, args) in launches(&LaunchCtx::current(temp_dir("ctx"), 0).expect("ctx")) {
        let dir = temp_dir("typed");
        let (mut app, pane_id, mut rx) = launch_app(&dir);
        let native: Vec<String> = std::iter::once(kind.to_string())
            .chain(args.iter().cloned())
            .collect();
        let full = crate::platform::interactive_shell_command(
            &app.herdr_launch_argv(kind, native.clone()),
            "sh",
        )
        .expect("command");
        app.start_agent_gated(
            AgentStartParams {
                name: "worker".into(),
                kind: kind.into(),
                pane_id,
                args,
                timeout_ms: None,
            },
            ShellGate::LineEditor,
        )
        .unwrap_or_else(|_| panic!("{what}: started"));
        let typed = rx.try_recv().expect("typed input");
        let typed = std::str::from_utf8(&typed).expect("utf8");
        assert!(
            typed.len() < 512,
            "{what}: typed line is {} bytes",
            typed.len()
        );
        assert!(
            full.len() > TYPED_LAUNCH_DIRECT_MAX,
            "{what}: the fixture is a long launch ({} bytes)",
            full.len()
        );
        // `. '<dir>/launch/<uuid>.sh'` + Enter: the script holds the full
        // command after removing itself.
        let script = typed
            .trim_end_matches('\r')
            .strip_prefix(". ")
            .unwrap_or_else(|| panic!("{what}: a source line: {typed:?}"))
            .trim_matches('\'');
        assert!(script.starts_with(&dir.join("launch").display().to_string()));
        let body = std::fs::read_to_string(script).expect("launch script");
        let mut lines = body.lines();
        assert_eq!(
            lines.next(),
            Some(format!("command rm -f -- {script}").as_str()),
            "{what}"
        );
        assert_eq!(lines.next(), Some(full.as_str()), "{what}");
        assert_eq!(lines.next(), None, "{what}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[tokio::test]
async fn a_short_launch_is_typed_as_is_and_long_ones_only_through_sourcing_shells() {
    let dir = temp_dir("short");
    let (app, _, _rx) = launch_app(&dir);
    assert_eq!(
        app.short_launch_line("claude --resume abc".into(), "zsh"),
        "claude --resume abc"
    );
    let long = format!("claude -- '{}'", "x".repeat(600));
    let fish = app.short_launch_line(long.clone(), "/opt/homebrew/bin/fish");
    assert!(
        fish.starts_with("source /") || fish.starts_with("source '"),
        "{fish}"
    );
    let script = fish.trim_start_matches("source ").trim_matches('\'');
    assert!(std::fs::read_to_string(script)
        .expect("script")
        .ends_with(&format!("{long}\n")));
    // No source verb herdr types: the line goes as is.
    assert_eq!(app.short_launch_line(long.clone(), "nu"), long);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_launch_scripts_are_pruned() {
    let dir = temp_dir("prune");
    let old = dir.join("old.sh");
    std::fs::write(&old, "true\n").expect("write");
    let file = std::fs::File::options()
        .write(true)
        .open(&old)
        .expect("open");
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(7200))
        .expect("mtime");
    let fresh = write_launch_script(&dir, "true").expect("script");
    assert!(!old.exists());
    assert!(fresh.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_line_editor_gate_refuses_a_starting_shell_and_the_plain_gate_does_not() {
    let dir = temp_dir("gate");
    let (mut app, pane_id, mut rx) = launch_app(&dir);
    app.coordinator.assume_shell_starting = true;
    let start = AgentStartParams {
        name: "worker".into(),
        kind: "claude".into(),
        pane_id,
        args: Vec::new(),
        timeout_ms: None,
    };
    assert!(matches!(
        app.start_agent_gated(start.clone(), ShellGate::LineEditor),
        Err(AgentStartError::ShellNotReady(_))
    ));
    assert!(rx.try_recv().is_err(), "nothing typed");
    // Upstream `agent.start` keeps its foreground-shell gate.
    assert!(app.start_agent(start).is_ok());
    assert!(rx.try_recv().is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `agents.open_tab` with a claude start in the team group, through the
/// server's deferred path.
fn open_agent_tab(app: &mut App) -> std::sync::mpsc::Receiver<String> {
    let lead = public(app, 1, 0);
    let (respond_to, rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_agents_open_tab(
        Request {
            id: "open".into(),
            method: Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: lead,
                agent: Some("claude".into()),
                role: Some("tester".into()),
                kickoff: Some(long_kickoff()),
                ..Default::default()
            }),
        },
        respond_to,
    ));
    rx
}

fn model_launch_app(name: &str) -> (App, PathBuf) {
    let mut app = model_app();
    let dir = temp_dir(name);
    app.agents_model.dir = Some(dir.clone());
    app.coordinator.dir = dir.clone();
    let lead_pane = pane(&app, 1, 0);
    user_turn(&mut app, lead_pane);
    (app, dir)
}

#[tokio::test]
async fn fork_smoke_open_tab_waits_for_the_new_shell_then_types_the_launch() {
    let (mut app, dir) = model_launch_app("open-waits");
    let tabs_before = app.state.workspaces[1].tabs.len();
    app.coordinator.assume_shell_starting = true;
    let reply = open_agent_tab(&mut app);
    assert!(reply.try_recv().is_err(), "the reply waits for the shell");
    assert_eq!(app.agents_model.launches.open_tabs.len(), 1);
    assert_eq!(app.state.workspaces[1].tabs.len(), tabs_before + 1);
    let now = Instant::now();
    assert!(app
        .next_pending_launch_deadline()
        .is_some_and(|at| at <= now + SHELL_READY_RETRY));

    // Still starting: nothing typed, still parked.
    let pane_public = app.agents_model.launches.open_tabs[0]
        .opened
        .pane_id
        .clone();
    let (ws_idx, new_pane) = app.parse_pane_id(&pane_public).expect("new pane");
    let terminal_id = app.state.workspaces[ws_idx]
        .terminal_id(new_pane)
        .expect("terminal")
        .clone();
    if let Some(runtime) = app.terminal_runtimes.remove(&terminal_id) {
        runtime.shutdown();
    }
    let (runtime, mut input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
    app.terminal_runtimes.insert(terminal_id.clone(), runtime);
    app.drive_pending_launches(now + Duration::from_secs(1));
    assert!(reply.try_recv().is_err());
    assert!(
        input.try_recv().is_err(),
        "nothing typed into a starting shell"
    );

    // The line editor reads: the launch is typed and the reply sent.
    app.coordinator.assume_shell_starting = false;
    app.drive_pending_launches(now + Duration::from_secs(2));
    let response: serde_json::Value =
        serde_json::from_str(&reply.try_recv().expect("reply")).expect("json");
    assert_eq!(
        response["result"]["open"]["pane_id"].as_str(),
        Some(pane_public.as_str()),
        "{response}"
    );
    assert_eq!(response["result"]["open"]["member"], true, "{response}");
    let typed = input.try_recv().expect("typed launch");
    assert!(typed.len() < 512, "{} bytes", typed.len());
    assert!(app.agents_model.launches.open_tabs.is_empty());
    assert!(app.state.terminals[&terminal_id].managed_agent_launch_pending());
    let _ = std::fs::remove_dir_all(&dir);
    crate::app::api::test_support::shutdown_test_runtimes(&mut app);
}

#[tokio::test]
async fn open_tab_fails_cleanly_when_the_shell_never_reads() {
    let (mut app, dir) = model_launch_app("open-times-out");
    let tabs_before = app.state.workspaces[1].tabs.len();
    let members_before = app.state.workspaces[1]
        .team
        .as_ref()
        .map(|team| team.members.len());
    app.coordinator.assume_shell_starting = true;
    let reply = open_agent_tab(&mut app);
    assert!(reply.try_recv().is_err());
    assert_eq!(app.state.workspaces[1].tabs.len(), tabs_before + 1);

    app.drive_pending_launches(Instant::now() + SHELL_READY_TIMEOUT + Duration::from_secs(1));
    let response: serde_json::Value =
        serde_json::from_str(&reply.try_recv().expect("reply")).expect("json");
    assert_eq!(response["error"]["code"], "failed", "{response}");
    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("did not become ready"), "{response}");
    assert!(message.contains("closed)"), "{response}");
    assert_eq!(
        app.state.workspaces[1].tabs.len(),
        tabs_before,
        "no orphan tab"
    );
    assert_eq!(
        app.state.workspaces[1]
            .team
            .as_ref()
            .map(|team| team.members.len()),
        members_before,
        "no half-joined member"
    );
    assert!(app.agents_model.launches.open_tabs.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    crate::app::api::test_support::shutdown_test_runtimes(&mut app);
}

#[tokio::test]
async fn a_reopen_resume_line_waits_for_the_shell() {
    let dir = temp_dir("reopen");
    let (mut app, _, mut rx) = launch_app(&dir);
    let root = app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = app.state.workspaces[0]
        .terminal_id(root)
        .expect("terminal")
        .clone();
    app.coordinator.assume_shell_starting = true;
    let now = Instant::now();
    assert_eq!(
        app.type_launch_when_ready(&terminal_id, b"claude --resume s1\r".to_vec(), now),
        Ok(false)
    );
    app.drive_pending_launches(now + Duration::from_secs(1));
    assert!(
        rx.try_recv().is_err(),
        "nothing typed into a starting shell"
    );
    app.coordinator.assume_shell_starting = false;
    app.drive_pending_launches(now + Duration::from_secs(2));
    assert_eq!(&rx.try_recv().expect("typed")[..], b"claude --resume s1\r");
    assert!(app.agents_model.launches.typed.is_empty());

    // A shell that never reads gets the short line at the deadline anyway.
    app.coordinator.assume_shell_starting = true;
    app.type_launch_when_ready(&terminal_id, b"claude --resume s2\r".to_vec(), now)
        .expect("queued");
    app.drive_pending_launches(now + SHELL_READY_TIMEOUT + Duration::from_secs(1));
    assert_eq!(&rx.try_recv().expect("typed")[..], b"claude --resume s2\r");
    let _ = std::fs::remove_dir_all(&dir);
}
