use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;

use super::{missing_cwd_message, recreate_missing_cwd_under, set_test_home};
use crate::app::App;
use crate::detect::{Agent, AgentState};
use crate::persist::SessionSnapshot;
use crate::render_signal::RenderSignal;
use crate::terminal::TerminalId;

/// A fresh test home under the temp dir, removed on drop; also the
/// thread's creation root while it lives.
struct TestHome(PathBuf);

impl TestHome {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("herdr-missing-cwd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        set_test_home(Some(path.clone()));
        Self(path)
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        set_test_home(None);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_app() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    )
}

#[test]
fn only_absolute_paths_strictly_under_the_home_are_recreated() {
    let home = TestHome::new("policy");
    let nested = home.0.join("worktrees/gone/sub");
    assert!(recreate_missing_cwd_under(&nested, &home.0));
    assert!(nested.is_dir());

    assert!(
        !recreate_missing_cwd_under(&home.0, &home.0),
        "the home itself"
    );
    let outside =
        std::env::temp_dir().join(format!("herdr-missing-cwd-outside-{}", std::process::id()));
    assert!(!recreate_missing_cwd_under(&outside, &home.0));
    assert!(!outside.exists());
    assert!(!recreate_missing_cwd_under(
        Path::new("relative/dir"),
        &home.0
    ));
    let dotted = home.0.join("a/../../escape");
    assert!(!recreate_missing_cwd_under(&dotted, &home.0));
    assert!(!home.0.join("a").exists());

    // Creation fails: a regular file stands where a parent should be.
    std::fs::write(home.0.join("file"), b"x").unwrap();
    assert!(!recreate_missing_cwd_under(
        &home.0.join("file/sub"),
        &home.0
    ));
}

#[test]
fn the_notice_names_the_directory_and_asks_for_agents_notify() {
    let text = missing_cwd_message(Path::new("/home/u/wt/feature"));
    assert!(text.starts_with("herdr+: your working directory /home/u/wt/feature was missing"));
    assert!(text.contains("agents_notify"));
    assert!(!text.contains("{path}"));
}

fn snapshot_with(cwd: &Path, missing: &Path, agent: bool) -> SessionSnapshot {
    let mut snapshot: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session/current-herdr-session.json"
    ))
    .unwrap();
    for workspace in snapshot["workspaces"].as_array_mut().unwrap() {
        workspace["identity_cwd"] = serde_json::json!(cwd);
        for tab in workspace["tabs"].as_array_mut().unwrap() {
            for pane in tab["panes"].as_object_mut().unwrap().values_mut() {
                pane["cwd"] = serde_json::json!(cwd);
            }
        }
    }
    let first = snapshot["workspaces"][0]["tabs"][0]["panes"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    first["cwd"] = serde_json::json!(missing);
    if agent {
        first["agent_session"] = serde_json::json!({
            "source": "herdr:claude",
            "agent": "claude",
            "kind": "id",
            "value": "0b5e4a7e-1d0c-4b3f-9f1a-2a6c8d9e0f11",
        });
    }
    serde_json::from_value(snapshot).unwrap()
}

type Restored = (
    Vec<crate::workspace::Workspace>,
    HashMap<TerminalId, crate::terminal::TerminalState>,
    HashMap<TerminalId, crate::terminal::TerminalRuntime>,
);

fn restore(snapshot: &SessionSnapshot, resume: bool) -> Restored {
    let (events, _rx) = tokio::sync::mpsc::channel(32);
    crate::persist::restore(
        snapshot,
        None,
        24,
        80,
        0,
        "/bin/sh",
        crate::config::ShellModeConfig::NonLogin,
        resume,
        events,
        Arc::new(Notify::new()),
        Arc::new(RenderSignal::new()),
    )
}

#[cfg(unix)]
#[tokio::test]
async fn a_restored_shell_pane_gets_its_missing_directory_back_and_starts() {
    let home = TestHome::new("restore-shell");
    let missing = home.0.join("worktrees/removed");
    let (workspaces, terminals, runtimes) =
        restore(&snapshot_with(&home.0, &missing, false), false);
    assert!(missing.is_dir(), "the saved directory is recreated");
    let tab = &workspaces[0].tabs[0];
    let terminal_id = tab.terminal_id(tab.root_pane).unwrap();
    let terminal = &terminals[terminal_id];
    assert_eq!(terminal.cwd, missing, "the same path, not a fallback");
    assert!(terminal.restore_error.is_none());
    assert!(terminal.cwd_recreated);
    let runtimes = crate::terminal::TerminalRuntimeRegistry::from(runtimes);
    assert!(
        runtimes.get(terminal_id).is_some(),
        "the shell starts there"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_restored_agent_pane_resumes_in_the_recreated_directory() {
    let home = TestHome::new("restore-agent");
    let missing = home.0.join("worktrees/removed-agent");
    let (workspaces, terminals, _runtimes) = restore(&snapshot_with(&home.0, &missing, true), true);
    assert!(missing.is_dir());
    let tab = &workspaces[0].tabs[0];
    let terminal = &terminals[tab.terminal_id(tab.root_pane).unwrap()];
    assert!(terminal.restore_error.is_none());
    assert!(terminal.cwd_recreated);
    let plan = terminal
        .pending_agent_resume_plan
        .as_ref()
        .expect("the resume is pending");
    assert!(plan.argv.iter().any(|arg| arg.contains("0b5e4a7e")));
}

#[cfg(unix)]
#[tokio::test]
async fn a_directory_that_cannot_be_recreated_keeps_the_unavailable_card() {
    let home = TestHome::new("restore-outside");
    // Outside the home: never created.
    let missing =
        std::env::temp_dir().join(format!("herdr-missing-cwd-not-home-{}", std::process::id()));
    let (workspaces, terminals, runtimes) =
        restore(&snapshot_with(&home.0, &missing, false), false);
    assert!(!missing.exists());
    let tab = &workspaces[0].tabs[0];
    let terminal_id = tab.terminal_id(tab.root_pane).unwrap();
    let terminal = &terminals[terminal_id];
    assert!(terminal
        .restore_error
        .as_deref()
        .is_some_and(|error| error.starts_with("Saved directory is unavailable.")));
    assert!(!terminal.cwd_recreated);
    let runtimes = crate::terminal::TerminalRuntimeRegistry::from(runtimes);
    assert!(runtimes.get(terminal_id).is_none());
}

/// An app with one pane whose terminal sits in `cwd` (missing).
fn app_in(cwd: &Path) -> (App, TerminalId) {
    let mut app = test_app();
    let workspace = crate::workspace::Workspace::test_new("recreated");
    let pane_id = workspace.tabs[0].root_pane;
    let terminal_id = workspace.terminal_id(pane_id).unwrap().clone();
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    app.state.terminals.get_mut(&terminal_id).unwrap().cwd = cwd.to_path_buf();
    (app, terminal_id)
}

#[cfg(unix)]
#[tokio::test]
async fn a_deferred_agent_resume_recreates_the_directory_and_queues_one_notice() {
    let home = TestHome::new("deferred-agent");
    let missing = home.0.join("worktrees/feature");
    let (mut app, terminal_id) = app_in(&missing);
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
        agent: "claude".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        dedupe_key: "missing-cwd-test".into(),
    });
    assert!(app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true));
    assert!(missing.is_dir());
    assert!(
        app.terminal_runtimes.get(&terminal_id).is_some(),
        "it starts"
    );
    assert!(app.state.terminals[&terminal_id].restore_error.is_none());
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 1);

    // No agent detected yet: the notice waits, nothing is queued.
    let now = Instant::now();
    let now_unix = crate::coordinator::now_unix();
    assert!(!app.deliver_missing_cwd_notices(now, now_unix));
    assert!(app.message_queue.entries.is_empty());
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 1);

    // The agent is up: one notice goes into the queue (never typed in
    // directly), and only once.
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentDetected);
    assert!(app.deliver_missing_cwd_notices(Instant::now(), now_unix));
    assert_eq!(app.message_queue.entries.len(), 1);
    let entry = &app.message_queue.entries[0];
    assert_eq!(entry.terminal_id, terminal_id.to_string());
    assert_eq!(entry.message.text, missing_cwd_message(&missing));
    assert!(entry.envelope.contains("from herdr"));
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    assert!(!app.deliver_missing_cwd_notices(Instant::now() + super::POLL, now_unix));
    assert_eq!(app.message_queue.entries.len(), 1);

    // The queue holds it to its gates: a plain shell shows no Claude input
    // box, so the typing guard fails closed and nothing is typed in.
    let later = Instant::now();
    app.message_queue_pass(later, now_unix);
    app.message_queue_pass(later + crate::app::message_queue::SETTLE * 2, now_unix);
    assert_eq!(app.message_queue.entries.len(), 1, "still queued");

    // One card for the pane.
    assert_eq!(app.agent_notices.list().len(), 1);
}

#[tokio::test]
async fn a_suspended_agent_keeps_its_notice_without_polling_until_activation() {
    let home = TestHome::new("parked");
    let (mut app, terminal_id) = app_in(&home.0.join("parked"));
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.restore_suspended_agent(
        "claude".into(),
        Some("parked".into()),
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("parked-session").unwrap(),
            transcript_path: None,
        },
    );
    terminal.cwd_recreated = true;
    app.note_recreated_cwds();
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 1);
    let later = Instant::now() + super::GIVE_UP + std::time::Duration::from_secs(1);
    assert!(!app.deliver_missing_cwd_notices(later, crate::coordinator::now_unix()));
    assert_eq!(
        app.message_queue.missing_cwd.pending_len(),
        1,
        "parked, not given up"
    );
    assert!(app.message_queue.entries.is_empty());
    assert_eq!(
        app.message_queue.missing_cwd.next_deadline(),
        None,
        "no poll while parked"
    );
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    assert!(
        app.message_queue.missing_cwd.next_deadline().is_some(),
        "an agent event wakes it"
    );
}

#[tokio::test]
async fn a_recreated_shell_pane_gets_a_card_but_no_message() {
    let home = TestHome::new("shell");
    let (mut app, terminal_id) = app_in(&home.0.join("plain"));
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .cwd_recreated = true;
    app.note_recreated_cwds();
    assert!(
        !app.state.terminals[&terminal_id].cwd_recreated,
        "taken once"
    );
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
    let now = Instant::now();
    assert!(!app.deliver_missing_cwd_notices(now, crate::coordinator::now_unix()));
    assert!(app.message_queue.entries.is_empty());
    assert_eq!(app.agent_notices.list().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_deferred_resume_whose_directory_cannot_be_recreated_keeps_the_card() {
    let _home = TestHome::new("deferred-outside");
    let missing = std::env::temp_dir().join(format!(
        "herdr-missing-cwd-deferred-not-home-{}",
        std::process::id()
    ));
    let (mut app, terminal_id) = app_in(&missing);
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
        agent: "claude".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        dedupe_key: "missing-cwd-outside".into(),
    });
    app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true);
    assert!(!missing.exists());
    assert!(app.terminal_runtimes.get(&terminal_id).is_none());
    assert!(app.state.terminals[&terminal_id].restore_error.is_some());
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
}
