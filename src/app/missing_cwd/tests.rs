use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Notify;

use super::{
    home_is_creation_root, missing_cwd_message, recreate_missing_cwd_under, set_test_home,
};
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

#[cfg(unix)]
#[tokio::test]
async fn an_agent_detected_minutes_after_the_restart_still_gets_exactly_one_notice() {
    let home = TestHome::new("slow-detect");
    let missing = home.0.join("worktrees/slow");
    let (mut app, terminal_id) = app_in(&missing);
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
        agent: "claude".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        dedupe_key: "missing-cwd-slow".into(),
    });
    // Restore records the session it resumes.
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("slow-session").unwrap(),
            transcript_path: None,
        });
    assert!(app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true));
    let detect = |app: &mut App, agent: Option<Agent>| {
        let state = if agent.is_some() {
            AgentState::Idle
        } else {
            AgentState::Unknown
        };
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(agent, state);
        app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    };
    // The agent shows up once, the notice is queued, then the resumed
    // session takes minutes to be detected again.
    detect(&mut app, Some(Agent::Claude));
    let start = Instant::now();
    let start_unix = crate::coordinator::now_unix();
    assert!(app.deliver_missing_cwd_notices(start, start_unix));
    assert_eq!(app.message_queue.entries.len(), 1);
    let id = app.message_queue.entries[0].message.id.clone();
    detect(&mut app, None);
    let mut at = start;
    for minute in 1..=2u64 {
        at = start + std::time::Duration::from_secs(60 * minute);
        app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
        app.message_queue_pass(at, start_unix + 60 * minute);
        assert_eq!(
            app.message_queue.entries.len(),
            1,
            "held past the 60 s grace at minute {minute}"
        );
    }
    assert!(
        app.message_queue.entries[0].expires_unix
            >= start_unix + crate::app::message_queue::MESSAGE_TTL_USER_BLOCKED_S,
        "kept for the user-blocked hold, not the 2 h message TTL"
    );

    // Detected after 2 minutes: the same one notice, nothing new queued.
    detect(&mut app, Some(Agent::Claude));
    assert!(!app.deliver_missing_cwd_notices(at, start_unix + 120));
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    app.message_queue_pass(at, start_unix + 120);
    assert_eq!(app.message_queue.entries.len(), 1);
    assert_eq!(app.message_queue.entries[0].message.id, id);
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
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

#[test]
fn a_home_without_a_named_component_is_no_creation_root() {
    assert!(!home_is_creation_root(Path::new("/")));
    assert!(
        home_is_creation_root(Path::new("/root")),
        "a one-component home"
    );
    assert!(home_is_creation_root(Path::new("/home/u")));
    // HOME=/ would let every absolute path in.
    let anywhere = std::env::temp_dir().join(format!(
        "herdr-missing-cwd-root-home-{}",
        std::process::id()
    ));
    assert!(!recreate_missing_cwd_under(&anywhere, Path::new("/")));
    assert!(!anywhere.exists());
}

#[cfg(unix)]
#[test]
fn fork_smoke_a_symlink_under_the_home_does_not_lead_the_creation_out_of_it() {
    let home = TestHome::new("symlink-escape");
    let outside = std::env::temp_dir().join(format!(
        "herdr-missing-cwd-symlink-target-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, home.0.join("link")).unwrap();
    let through = home.0.join("link/worktrees/gone");
    assert!(!recreate_missing_cwd_under(&through, &home.0));
    assert!(
        !outside.join("worktrees").exists(),
        "nothing created outside"
    );

    // A symlink that stays under the home is fine.
    std::fs::create_dir_all(home.0.join("real")).unwrap();
    std::os::unix::fs::symlink(home.0.join("real"), home.0.join("inside")).unwrap();
    assert!(recreate_missing_cwd_under(
        &home.0.join("inside/worktrees/gone"),
        &home.0
    ));
    assert!(home.0.join("real/worktrees/gone").is_dir());
    let _ = std::fs::remove_dir_all(&outside);
}

/// The terminal's agent runs and is detected: its notice can be queued.
fn agent_up(app: &mut App, terminal_id: &TerminalId) {
    app.terminal_runtimes.insert(
        terminal_id.clone(),
        crate::terminal::TerminalRuntime::test_with_scrollback_bytes(80, 24, 1024, b""),
    );
    app.state
        .terminals
        .get_mut(terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentDetected);
}

#[tokio::test]
async fn an_agent_parked_past_give_up_still_gets_its_notice_after_activation() {
    let home = TestHome::new("parked-long");
    let (mut app, terminal_id) = app_in(&home.0.join("parked"));
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.restore_suspended_agent(
        "claude".into(),
        Some("parked".into()),
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("parked-long").unwrap(),
            transcript_path: None,
        },
    );
    terminal.cwd_recreated = true;
    app.note_recreated_cwds();
    let start = Instant::now();
    let now_unix = crate::coordinator::now_unix();
    // One pass while suspended, then nothing for longer than GIVE_UP.
    assert!(!app.deliver_missing_cwd_notices(start, now_unix));
    assert_eq!(app.message_queue.missing_cwd.next_deadline(), None);

    let later = start + super::GIVE_UP + std::time::Duration::from_secs(60);
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .suspended_agent = None;
    agent_up(&mut app, &terminal_id);
    assert!(
        app.deliver_missing_cwd_notices(later, now_unix),
        "the parked time does not count toward GIVE_UP"
    );
    assert_eq!(app.message_queue.entries.len(), 1);
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
    app.terminal_runtimes.remove(&terminal_id);
}

#[cfg(unix)]
#[tokio::test]
async fn a_resume_retried_after_a_failed_send_still_gives_one_card_and_one_notice() {
    let home = TestHome::new("retry");
    let missing = home.0.join("worktrees/retry");
    let (mut app, terminal_id) = app_in(&missing);
    let plan = crate::agent_resume::AgentResumePlan {
        agent: "claude".into(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()],
        dedupe_key: "missing-cwd-retry".into(),
    };
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = Some(plan.clone());
    assert!(app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true));
    assert!(missing.is_dir());
    // What a failed send leaves: no runtime, the plan still pending, the
    // directory there now.
    if let Some(runtime) = app.terminal_runtimes.remove(&terminal_id) {
        runtime.shutdown();
    }
    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = Some(plan);
    assert!(app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true));
    assert_eq!(app.agent_notices.list().len(), 1, "one card");
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 1);

    app.state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentDetected);
    let now_unix = crate::coordinator::now_unix();
    assert!(app.deliver_missing_cwd_notices(Instant::now(), now_unix));
    app.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    assert!(!app.deliver_missing_cwd_notices(Instant::now() + super::POLL, now_unix));
    assert_eq!(app.message_queue.entries.len(), 1, "one notice");
}

/// `snapshot` restored into a test app (as a restart does).
fn restored_app(snapshot: &SessionSnapshot) -> App {
    let (workspaces, terminals, runtimes) = restore(snapshot, true);
    let mut app = test_app();
    app.state.workspaces = workspaces;
    app.state.terminals = terminals;
    app.terminal_runtimes = runtimes.into();
    app.state.active = Some(0);
    app
}

fn root_terminal(app: &App) -> TerminalId {
    let tab = &app.state.workspaces[0].tabs[0];
    tab.terminal_id(tab.root_pane).unwrap().clone()
}

fn shutdown_runtimes(app: &mut App) {
    let ids: Vec<TerminalId> = app.state.terminals.keys().cloned().collect();
    for id in ids {
        if let Some(runtime) = app.terminal_runtimes.remove(&id) {
            runtime.shutdown();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn fork_smoke_a_pending_notice_survives_a_restart_and_is_delivered_once() {
    let home = TestHome::new("survives");
    let missing = home.0.join("worktrees/survives");
    let snapshot = snapshot_with(&home.0, &missing, true);
    let store = home.0.join("state/message_queue.json");

    let mut first = restored_app(&snapshot);
    first.message_queue.persist_for_test(store.clone());
    first.note_recreated_cwds();
    assert_eq!(first.message_queue.missing_cwd.pending_len(), 1);
    let saved = (0..300).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::read_to_string(&store).is_ok_and(|text| text.contains("\"missing_cwd\""))
    });
    assert!(saved, "the pending notice is saved with the queue");
    let old_terminal = root_terminal(&first);
    shutdown_runtimes(&mut first);
    drop(first);

    // The restart: the directory is there now (no recreation, no card), and
    // the pane has a new terminal id under the same public pane id.
    let mut second = restored_app(&snapshot);
    let terminal_id = root_terminal(&second);
    assert_ne!(terminal_id, old_terminal);
    assert!(!second.state.terminals[&terminal_id].cwd_recreated);
    second.message_queue.persist_for_test(store.clone());
    second.message_queue.load(store.clone());
    second.rearm_missing_cwd_notices(Instant::now(), crate::coordinator::now_unix());
    second.note_recreated_cwds();
    assert_eq!(second.message_queue.missing_cwd.pending_len(), 1);
    assert!(second.agent_notices.list().is_empty(), "no second card");

    // Its resume starts (here: a test runtime) and its agent shows up.
    second
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = None;
    agent_up(&mut second, &terminal_id);
    let now_unix = crate::coordinator::now_unix();
    assert!(second.deliver_missing_cwd_notices(Instant::now(), now_unix));
    assert_eq!(second.message_queue.entries.len(), 1);
    assert_eq!(
        second.message_queue.entries[0].message.text,
        missing_cwd_message(&missing)
    );
    second.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    assert!(!second.deliver_missing_cwd_notices(Instant::now() + super::POLL, now_unix));
    assert_eq!(second.message_queue.entries.len(), 1, "delivered once");
    assert!(second.message_queue.missing_cwd.saved().is_empty());
    shutdown_runtimes(&mut second);
}

#[test]
fn the_queue_file_keeps_its_format_with_or_without_saved_notices() {
    let home = TestHome::new("format");
    // A file from before the field: loads, nothing to re-arm.
    let old = home.0.join("old.json");
    std::fs::write(&old, br#"{"version":1,"messages":[]}"#).unwrap();
    let mut queue = crate::app::message_queue::MessageQueue::default();
    queue.persist_for_test(home.0.join("unused.json"));
    queue.load(old);
    assert!(queue.entries.is_empty());
    assert!(queue.missing_cwd.saved().is_empty());

    // A file with the field: it is read back as written.
    let new = home.0.join("new.json");
    std::fs::write(
        &new,
        br#"{"version":1,"messages":[],"missing_cwd":[{"pane":"w1:p1","path":"/home/u/wt","since_unix":7}]}"#,
    )
    .unwrap();
    queue.load(new);
    assert_eq!(
        queue.missing_cwd.saved(),
        vec![super::SavedMissingCwdNotice {
            pane: "w1:p1".into(),
            path: PathBuf::from("/home/u/wt"),
            since_unix: 7,
            parked: false,
        }]
    );
    let entry = serde_json::to_value(super::SavedMissingCwdNotice {
        pane: "w1:p1".into(),
        path: PathBuf::from("/x"),
        since_unix: 1,
        parked: false,
    })
    .unwrap();
    assert_eq!(
        entry.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["pane", "path", "since_unix"]
    );
    // A parked notice says so; it reads back parked.
    let parked = super::SavedMissingCwdNotice {
        pane: "w1:p1".into(),
        path: PathBuf::from("/x"),
        since_unix: 1,
        parked: true,
    };
    let text = serde_json::to_string(&parked).unwrap();
    assert!(text.contains("\"parked\":true"));
    assert_eq!(
        serde_json::from_str::<super::SavedMissingCwdNotice>(&text).unwrap(),
        parked
    );
}

/// Poll `store` until its saved queue satisfies `ok`.
fn saved_queue_until(store: &Path, ok: impl Fn(&serde_json::Value) -> bool) -> bool {
    (0..300).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::read(store)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|value| ok(&value))
    })
}

#[tokio::test]
async fn a_notice_dropped_after_one_was_queued_in_the_same_pass_is_saved_gone() {
    let home = TestHome::new("queued-then-gone");
    let store = home.0.join("state/message_queue.json");
    let mut app = test_app();
    let mut workspace = crate::workspace::Workspace::test_new("two");
    let first_pane = workspace.tabs[0].root_pane;
    let second_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
    let ready = workspace.terminal_id(first_pane).unwrap().clone();
    let gone = workspace.terminal_id(second_pane).unwrap().clone();
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.ensure_test_terminals();
    for id in [&ready, &gone] {
        app.state.terminals.get_mut(id).unwrap().cwd = home.0.join("wt");
    }
    app.message_queue.persist_for_test(store.clone());
    app.note_recreated_cwd(&ready, true);
    app.note_recreated_cwd(&gone, true);
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 2);
    agent_up(&mut app, &ready);
    app.state.terminals.get_mut(&gone).unwrap().restore_error = Some("gone".into());
    // One pass: one notice is queued and the other dropped, in either order.
    assert!(app.deliver_missing_cwd_notices(Instant::now(), crate::coordinator::now_unix()));
    assert_eq!(app.message_queue.missing_cwd.pending_len(), 0);
    assert!(
        saved_queue_until(&store, |queue| {
            queue["messages"].as_array().is_some_and(|m| m.len() == 1)
                && queue.get("missing_cwd").is_none()
        }),
        "the saved queue has the queued notice and no pending one"
    );
    app.terminal_runtimes.remove(&ready);
}

#[cfg(unix)]
#[tokio::test]
async fn fork_smoke_a_pending_notice_follows_its_pane_to_another_space_across_a_restart() {
    use crate::api::schema::{Method, PaneMoveDestination, PaneMoveParams, Request};
    let home = TestHome::new("moved");
    let missing = home.0.join("worktrees/moved");
    let store = home.0.join("state/message_queue.json");
    let mut first = restored_app(&snapshot_with(&home.0, &missing, true));
    first.message_queue.persist_for_test(store.clone());
    first.note_recreated_cwds();
    let terminal = root_terminal(&first);
    let before = first.message_queue.missing_cwd.saved()[0].pane.clone();

    let destination = first.public_workspace_id(1);
    let response = first.handle_api_request(Request {
        id: "move".into(),
        method: Method::PaneMove(PaneMoveParams {
            pane_id: before.clone(),
            destination: PaneMoveDestination::NewTab {
                workspace_id: Some(destination.clone()),
                label: None,
            },
            focus: false,
        }),
    });
    let success: crate::api::schema::SuccessResponse =
        serde_json::from_str(&response).unwrap_or_else(|_| panic!("{response}"));
    let crate::api::schema::ResponseResult::PaneMove { move_result } = success.result else {
        panic!("expected a pane move: {response}");
    };
    assert!(move_result.changed);
    let target = first
        .resolve_terminal_target(&terminal.to_string())
        .unwrap();
    let after = first.public_pane_id(target.ws_idx, target.pane_id).unwrap();
    assert!(after.starts_with(&destination), "{after}");
    assert_ne!(after, before);
    assert_eq!(first.message_queue.missing_cwd.saved()[0].pane, after);
    assert!(
        saved_queue_until(&store, |queue| queue["missing_cwd"][0]["pane"]
            == after.as_str()),
        "the move is saved"
    );
    let snapshot = crate::persist::capture(
        &first.state.workspaces,
        &first.state.terminals,
        &first.terminal_runtimes,
        first.state.active,
        first.state.selected,
    );
    shutdown_runtimes(&mut first);
    drop(first);

    let mut second = restored_app(&snapshot);
    second.message_queue.persist_for_test(store.clone());
    second.message_queue.load(store.clone());
    second.rearm_missing_cwd_notices(Instant::now(), crate::coordinator::now_unix());
    assert_eq!(second.message_queue.missing_cwd.pending_len(), 1);
    let moved = second.resolve_terminal_target(&after).unwrap().terminal_id;
    let moved: TerminalId = second.state.terminals[moved.as_str()].id.clone();
    second
        .state
        .terminals
        .get_mut(&moved)
        .unwrap()
        .pending_agent_resume_plan = None;
    agent_up(&mut second, &moved);
    let now_unix = crate::coordinator::now_unix();
    assert!(second.deliver_missing_cwd_notices(Instant::now(), now_unix));
    assert_eq!(second.message_queue.entries.len(), 1);
    assert_eq!(
        second.message_queue.entries[0].terminal_id,
        moved.to_string()
    );
    second.note_message_queue_event(&crate::api::schema::EventKind::PaneAgentStatusChanged);
    assert!(!second.deliver_missing_cwd_notices(Instant::now() + super::POLL, now_unix));
    assert_eq!(second.message_queue.entries.len(), 1, "delivered once");
    shutdown_runtimes(&mut second);
}

#[cfg(unix)]
#[tokio::test]
async fn a_notice_parked_past_give_up_and_activated_survives_a_restart_before_detection() {
    let home = TestHome::new("parked-restart");
    let missing = home.0.join("worktrees/parked");
    let snapshot = snapshot_with(&home.0, &missing, true);
    let store = home.0.join("state/message_queue.json");
    let mut first = restored_app(&snapshot);
    first.message_queue.persist_for_test(store.clone());
    let terminal_id = root_terminal(&first);
    let terminal = first.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.pending_agent_resume_plan = None;
    terminal.restore_suspended_agent(
        "claude".into(),
        Some("parked".into()),
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("parked-restart").unwrap(),
            transcript_path: None,
        },
    );
    first.note_recreated_cwds();
    let now_unix = crate::coordinator::now_unix();
    // Armed long ago and parked since: passes while suspended do not move
    // its saved time.
    let long_ago = now_unix - super::GIVE_UP.as_secs() - 3600;
    first
        .message_queue
        .missing_cwd
        .pending
        .get_mut(&terminal_id)
        .unwrap()
        .since_unix = long_ago;
    assert!(!first.deliver_missing_cwd_notices(Instant::now(), now_unix));
    assert_eq!(
        first.message_queue.missing_cwd.saved()[0].since_unix,
        long_ago
    );
    // Activated, and the queue is saved, but no pass runs before the restart.
    first
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .suspended_agent = None;
    first.message_queue.save();
    assert!(
        saved_queue_until(&store, |queue| {
            queue["missing_cwd"][0]["parked"] == true
                && queue["missing_cwd"][0]["since_unix"] == long_ago
        }),
        "saved parked"
    );
    shutdown_runtimes(&mut first);
    drop(first);

    // The restart resumes the agent (not suspended); it is detected later.
    let mut second = restored_app(&snapshot);
    second.message_queue.persist_for_test(store.clone());
    second.message_queue.load(store.clone());
    second.rearm_missing_cwd_notices(Instant::now(), now_unix);
    assert_eq!(
        second.message_queue.missing_cwd.pending_len(),
        1,
        "its parked time does not count toward GIVE_UP"
    );
    let terminal_id = root_terminal(&second);
    second
        .state
        .terminals
        .get_mut(&terminal_id)
        .unwrap()
        .pending_agent_resume_plan = None;
    agent_up(&mut second, &terminal_id);
    assert!(second.deliver_missing_cwd_notices(Instant::now(), now_unix));
    assert_eq!(second.message_queue.entries.len(), 1);
    shutdown_runtimes(&mut second);
}
