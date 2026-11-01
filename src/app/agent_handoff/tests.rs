use super::*;
use crate::api::schema::{PaneReportTurnParams, Request, TurnEvent};
use crate::config::Config;
use crate::detect::{Agent, AgentState};
use crate::workspace::Workspace;
use bytes::Bytes;
use serde_json::Value;

const CLAUDE_EMPTY: &[u8] =
    "\r\n─────\r\n❯ \x1b[2mTry \"something\"\x1b[0m\r\n─────\r\n  footer".as_bytes();
const SESSION: &str = "0f3e9c1a-handoff";

struct Setup {
    app: App,
    rx: tokio::sync::mpsc::Receiver<Bytes>,
    terminal_id: TerminalId,
    pane: String,
    base: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn setup(agent: Agent) -> Setup {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("handoff")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "herdr-app-handoff-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&base).unwrap();
    app.agent_handoff.root = Some(base.join("handoffs"));
    let transcript = base.join("live.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"content\":\"build the parser\"}}\n",
    )
    .unwrap();
    let pane_id = app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    terminal.set_agent_name("rev".into());
    terminal.set_detected_state(Some(agent), AgentState::Idle);
    terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
        source: format!("herdr:{}", agent_key(agent)),
        agent: agent_key(agent).into(),
        session_ref: crate::agent_resume::AgentSessionRef::id(SESSION).unwrap(),
        transcript_path: Some(transcript),
    });
    let (runtime, rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(40, 8, 0, b"", 4);
    runtime.test_process_pty_bytes(CLAUDE_EMPTY);
    app.state.insert_test_runtime(pane_id, runtime);
    let pane = app.public_pane_id(0, pane_id).unwrap();
    Setup {
        app,
        rx,
        terminal_id,
        pane,
        base,
    }
}

fn agent_key(agent: Agent) -> &'static str {
    match agent {
        Agent::Codex => "codex",
        _ => "claude",
    }
}

/// One step of the tick at the real clock.
fn tick(app: &mut App) {
    app.agent_handoff.deadline = Some(Instant::now());
    app.handle_agent_handoff_tasks(Instant::now());
}

/// Tick until the worker's jobs came back.
fn wait_worker(app: &mut App) {
    for _ in 0..200 {
        if app.agent_handoff.in_flight == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
        app.agent_handoff.deadline = Some(Instant::now());
        let outcomes = app
            .agent_handoff
            .worker
            .as_ref()
            .map(files::Worker::drain)
            .unwrap_or_default();
        for outcome in outcomes {
            app.agent_handoff.in_flight -= 1;
            app.apply_handoff_outcome(outcome, Instant::now());
        }
    }
    panic!("the handoff worker did not answer");
}

/// The agent has been free for the settle time.
fn settle(s: &mut Setup) {
    if let Some(run) = s.app.agent_handoff.runs.get_mut(&s.terminal_id) {
        run.free_since = Some(Instant::now() - SETTLE - Duration::from_millis(1));
    }
}

fn phase(s: &Setup) -> Option<&'static str> {
    s.app
        .agent_handoff
        .runs
        .get(&s.terminal_id)
        .map(|run| run.phase.name())
}

/// Everything typed so far (each submission's Enter follows its text after
/// `AGENT_PROMPT_SUBMIT_DELAY`), without the Enters.
fn typed(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> String {
    std::thread::sleep(crate::app::api::AGENT_PROMPT_SUBMIT_DELAY + Duration::from_millis(150));
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out.replace('\r', "")
}

fn handoff_api(app: &mut App, caller: Option<&str>, target: &str) -> Value {
    let response = app.handle_api_request(Request {
        id: "req".into(),
        method: Method::AgentsHandoff(AgentsHandoffParams {
            caller_pane: caller.map(str::to_string),
            target: target.into(),
        }),
    });
    serde_json::from_str(&response).unwrap()
}

fn report_turn(s: &mut Setup, event: TurnEvent, prompt: &str) {
    let seq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let response = s.app.handle_api_request(Request {
        id: "turn".into(),
        method: Method::PaneReportTurn(PaneReportTurnParams {
            pane_id: s.pane.clone(),
            agent: "claude".into(),
            event,
            prompt_id: Some(prompt.into()),
            seq,
        }),
    });
    assert!(!response.contains("\"error\""), "{response}");
}

fn report_compact(s: &mut Setup, event: CompactEvent, trigger: Option<&str>) -> Value {
    let response = s.app.handle_api_request(Request {
        id: "compact".into(),
        method: Method::PaneReportCompact(PaneReportCompactParams {
            pane_id: s.pane.clone(),
            agent: "claude".into(),
            event,
            trigger: trigger.map(str::to_string),
            session_id: Some(SESSION.into()),
            transcript_path: None,
        }),
    });
    serde_json::from_str(&response).unwrap()
}

fn set_state(s: &mut Setup, state: AgentState) {
    s.app
        .state
        .terminals
        .get_mut(&s.terminal_id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), state);
}

/// Start a run as the user and step it to the typed prompt.
fn to_prompt_typed(s: &mut Setup) -> String {
    let pane = s.pane.clone();
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(reply["result"]["handoff"]["outcome"], "started", "{reply}");
    wait_worker(&mut s.app);
    tick(&mut s.app);
    assert_eq!(phase(s), Some("await_prompt"), "not settled yet");
    assert!(typed(&mut s.rx).is_empty());
    settle(s);
    tick(&mut s.app);
    assert_eq!(phase(s), Some("await_handoff"));
    typed(&mut s.rx)
}

#[tokio::test]
async fn a_manual_run_types_the_prompt_then_compact_then_the_resume_line() {
    let mut s = setup(Agent::Claude);
    let typed_prompt = to_prompt_typed(&mut s);
    let run = s.app.agent_handoff.runs[&s.terminal_id].clone();
    assert!(typed_prompt.contains("write a handoff"), "{typed_prompt}");
    assert!(
        typed_prompt.contains(&run.paths.handoff.display().to_string()),
        "{typed_prompt}"
    );
    // The transcript was copied before the prompt went in.
    assert_eq!(run.transcript_saved, Some(true));
    assert!(std::fs::read_to_string(&run.paths.transcript)
        .unwrap()
        .contains("build the parser"));
    assert!(!run.paths.latest.exists(), "no handoff yet");
    // Messages wait from here on.
    assert!(s
        .app
        .agent_handoff
        .holds_messages(&s.terminal_id.to_string()));

    // The agent works and writes the file; its Stop hook ends the turn.
    set_state(&mut s, AgentState::Working);
    report_turn(&mut s, TurnEvent::Start, "p1");
    std::fs::write(&run.paths.handoff, "# Handoff\nstate: parser half done\n").unwrap();
    report_turn(&mut s, TurnEvent::End, "p1");
    tick(&mut s.app);
    wait_worker(&mut s.app);
    assert_eq!(phase(&s), Some("await_compact"));
    assert_eq!(
        std::fs::read_to_string(&run.paths.latest).unwrap().trim(),
        run.paths.dir.display().to_string()
    );
    let saved = &s.app.agent_handoff.saved[&s.terminal_id];
    assert_eq!(saved.kind, Kind::Agent);
    assert!(s.app.agent_handoff_saved_unix(&s.terminal_id).is_some());

    // /compact goes in once the screen reads idle and settled.
    tick(&mut s.app);
    assert!(typed(&mut s.rx).is_empty(), "still working on screen");
    set_state(&mut s, AgentState::Idle);
    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    assert_eq!(typed(&mut s.rx), "/compact");
    assert_eq!(phase(&s), Some("compacting"));

    // Claude's PreCompact for herdr's own /compact saves nothing more; the
    // compacted session's start gets the handoff in its context.
    let pre = report_compact(&mut s, CompactEvent::Pre, Some("manual"));
    assert!(pre["result"]["context"].is_null(), "{pre}");
    assert_eq!(phase(&s), Some("compacting"));
    let post = report_compact(&mut s, CompactEvent::Post, None);
    let context = post["result"]["context"].as_str().expect("context text");
    assert!(context.starts_with(handoff::INJECTION_PREFIX), "{context}");
    assert!(context.contains(&run.paths.handoff.display().to_string()));
    assert!(context.contains(&run.paths.transcript.display().to_string()));
    assert_eq!(phase(&s), Some("await_resume"));

    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    let line = typed(&mut s.rx);
    assert!(line.starts_with("herdr: compacted"), "{line}");
    assert!(line.contains(&run.paths.handoff.display().to_string()));
    assert_eq!(phase(&s), None, "done");
    assert!(!s
        .app
        .agent_handoff
        .holds_messages(&s.terminal_id.to_string()));
    // The final meta.json is written on the worker (no outcome comes back).
    let mut meta = String::new();
    for _ in 0..200 {
        meta = std::fs::read_to_string(&run.paths.meta).unwrap_or_default();
        if meta.contains("\"outcome\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(meta.contains("\"outcome\": \"ok\""), "{meta}");
    assert!(meta.contains("\"kind\": \"agent\""), "{meta}");
}

#[tokio::test]
async fn a_handoff_that_never_comes_is_written_by_herdr_and_compaction_goes_on() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let run = s.app.agent_handoff.runs.get_mut(&s.terminal_id).unwrap();
    run.phase = Phase::AwaitHandoff {
        typed_at: Instant::now() - handoff::HANDOFF_TIMEOUT,
        checked: None,
        checking: false,
    };
    run.phase_since = Instant::now() - handoff::HANDOFF_TIMEOUT;
    let paths = run.paths.clone();
    tick(&mut s.app);
    assert_eq!(phase(&s), Some("writing_fallback"));
    wait_worker(&mut s.app);
    assert_eq!(phase(&s), Some("await_compact"));
    let text = std::fs::read_to_string(&paths.handoff).unwrap();
    assert!(text.contains("written by herdr"), "{text}");
    assert!(text.contains("- build the parser"), "{text}");
    assert_eq!(
        s.app.agent_handoff.saved[&s.terminal_id].kind,
        Kind::Mechanical
    );
    settle(&mut s);
    tick(&mut s.app);
    assert_eq!(typed(&mut s.rx), "/compact");
}

#[tokio::test]
async fn an_empty_handoff_file_waits_for_the_next_turn_end() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    report_turn(&mut s, TurnEvent::Start, "p1");
    report_turn(&mut s, TurnEvent::End, "p1");
    tick(&mut s.app);
    wait_worker(&mut s.app);
    assert_eq!(phase(&s), Some("await_handoff"), "no file yet");
    tick(&mut s.app);
    assert_eq!(
        s.app.agent_handoff.in_flight, 0,
        "the same end is not checked twice"
    );
}

#[tokio::test]
async fn nothing_is_typed_into_a_blocked_working_or_typing_agent() {
    let mut s = setup(Agent::Claude);
    let pane = s.pane.clone();
    handoff_api(&mut s.app, None, &pane);
    wait_worker(&mut s.app);
    for state in [AgentState::Blocked, AgentState::Working] {
        set_state(&mut s, state);
        tick(&mut s.app);
        settle(&mut s);
        tick(&mut s.app);
        assert_eq!(phase(&s), Some("await_prompt"), "{state:?}");
        assert!(typed(&mut s.rx).is_empty(), "{state:?}");
    }
    set_state(&mut s, AgentState::Idle);
    // The user typed into the pane a moment ago: the guard refuses.
    let pane_id = s.app.state.workspaces[0].tabs[0].root_pane;
    s.app
        .lookup_runtime_sender(0, pane_id)
        .unwrap()
        .note_user_input(Instant::now());
    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    assert_eq!(phase(&s), Some("await_prompt"));
    assert!(typed(&mut s.rx).is_empty());
}

#[tokio::test]
async fn suspended_coordinator_codex_and_switched_off_runs_are_refused() {
    let mut s = setup(Agent::Codex);
    let pane = s.pane.clone();
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(reply["error"]["code"], "unsupported_agent", "{reply}");

    let mut s = setup(Agent::Claude);
    let pane = s.pane.clone();
    s.app.state.coordinator_terminal_id = Some(s.terminal_id.clone());
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(reply["error"]["code"], "protected_tab", "{reply}");
    // The automatic trigger never fires for it either.
    s.app
        .state
        .terminals
        .get_mut(&s.terminal_id)
        .unwrap()
        .agent_context = Some(crate::agent_context::ContextUsage {
        used: 190_000,
        window: 200_000,
    });
    s.app
        .note_agent_handoff_context(&s.terminal_id.clone(), Instant::now());
    assert_eq!(phase(&s), None);

    let mut s = setup(Agent::Claude);
    let session = s.app.state.terminals[&s.terminal_id]
        .persistable_agent_session()
        .unwrap();
    s.app
        .state
        .terminals
        .get_mut(&s.terminal_id)
        .unwrap()
        .begin_agent_suspend(session, Instant::now() + Duration::from_secs(30));
    let pane = s.pane.clone();
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(reply["error"]["code"], "agent_suspended", "{reply}");

    let mut s = setup(Agent::Claude);
    let pane = s.pane.clone();
    s.app.agents_config.handoff.enabled = false;
    let reply = handoff_api(&mut s.app, None, &pane);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("enabled = false"));
}

#[tokio::test]
async fn a_second_request_reports_the_run_already_on() {
    let mut s = setup(Agent::Claude);
    let pane = s.pane.clone();
    handoff_api(&mut s.app, None, &pane);
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(
        reply["result"]["handoff"]["outcome"], "already_running",
        "{reply}"
    );
    assert_eq!(reply["result"]["handoff"]["pane_id"], pane.as_str());
    wait_worker(&mut s.app);
}

#[tokio::test]
async fn an_agent_may_hand_off_itself() {
    let mut s = setup(Agent::Claude);
    let pane = s.pane.clone();
    let reply = handoff_api(&mut s.app, Some(&pane), &pane);
    assert_eq!(reply["result"]["handoff"]["outcome"], "started", "{reply}");
    wait_worker(&mut s.app);
}

#[tokio::test]
async fn the_automatic_trigger_fires_once_and_rearms_after_the_compaction() {
    let mut s = setup(Agent::Claude);
    s.app.agents_config.handoff.at_percent = 30;
    let read = |s: &mut Setup, used: u64| {
        s.app
            .state
            .terminals
            .get_mut(&s.terminal_id)
            .unwrap()
            .agent_context = Some(crate::agent_context::ContextUsage {
            used,
            window: 20_000,
        });
        let id = s.terminal_id.clone();
        s.app.note_agent_handoff_context(&id, Instant::now());
    };
    read(&mut s, 5_000);
    assert_eq!(phase(&s), None, "25%");
    read(&mut s, 7_000);
    assert_eq!(phase(&s), Some("await_prompt"), "35%");
    assert!(matches!(
        s.app.agent_handoff.runs[&s.terminal_id].trigger,
        Trigger::Auto { percent: 35 }
    ));
    wait_worker(&mut s.app);
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Ok("test".into()), Instant::now());
    read(&mut s, 9_000);
    assert_eq!(phase(&s), None, "still over it: the same cycle");
    read(&mut s, 2_000);
    read(&mut s, 8_000);
    assert_eq!(phase(&s), Some("await_prompt"), "re-armed below 30%");
    wait_worker(&mut s.app);
    // at_percent 0: never.
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Ok("test".into()), Instant::now());
    s.app.agents_config.handoff.at_percent = 0;
    read(&mut s, 1_000);
    read(&mut s, 19_000);
    assert_eq!(phase(&s), None);
}

#[tokio::test]
async fn a_compaction_herdr_did_not_start_gets_a_copy_and_a_mechanical_handoff() {
    let mut s = setup(Agent::Claude);
    let pre = report_compact(&mut s, CompactEvent::Pre, Some("auto"));
    assert!(pre["result"]["context"].is_null(), "{pre}");
    assert_eq!(phase(&s), Some("compacting"));
    let paths = s.app.agent_handoff.runs[&s.terminal_id].paths.clone();
    wait_worker(&mut s.app);
    assert!(std::fs::read_to_string(&paths.transcript)
        .unwrap()
        .contains("build the parser"));
    assert!(std::fs::read_to_string(&paths.handoff)
        .unwrap()
        .contains("Claude compacted the session on its own"));
    let post = report_compact(&mut s, CompactEvent::Post, None);
    let context = post["result"]["context"].as_str().expect("context");
    assert!(context.contains("written by herdr"), "{context}");
    // Claude's own auto-compact carries its turn on: no resume line.
    assert_eq!(phase(&s), None);
    // A later SessionStart(compact) without a run still finds the handoff.
    let again = report_compact(&mut s, CompactEvent::Post, None);
    assert!(again["result"]["context"].as_str().is_some(), "{again}");

    // The user's own /compact: the resume line follows.
    let mut s = setup(Agent::Claude);
    report_compact(&mut s, CompactEvent::Pre, Some("manual"));
    wait_worker(&mut s.app);
    report_compact(&mut s, CompactEvent::Post, None);
    assert_eq!(phase(&s), Some("await_resume"));
}

#[tokio::test]
async fn a_run_holds_queued_messages_until_it_ends() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let target = s
        .app
        .resolve_terminal_target(&s.terminal_id.to_string())
        .unwrap();
    assert_eq!(
        s.app.message_check(&target),
        super::super::message_queue::Check::Wait("handoff and compaction in progress".into())
    );
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Err("test".into()), Instant::now());
    assert!(matches!(
        s.app.message_check(&target),
        super::super::message_queue::Check::Ready { .. }
    ));
    // A failure shows the user a card from the pane.
    assert!(s
        .app
        .agent_notices
        .list()
        .iter()
        .any(|card| card.title.contains("Handoff")));
}

fn message(s: &mut Setup, id: &str, body: &str) -> Value {
    let response = s.app.handle_agent_message_send(
        "req".into(),
        crate::api::schema::AgentMessageSendParams {
            target: "rev".into(),
            id: id.into(),
            envelope: format!(
                "[herdr+ message {id} from lead (w9:p1) 10:00 — another agent]\n{body}"
            ),
            text: body.into(),
            unix: 0,
            from_pane: Some("w9:p1".into()),
            from_name: Some("lead".into()),
            ..crate::api::schema::AgentMessageSendParams::default()
        },
    );
    serde_json::from_str(&response).unwrap()
}

/// The agent writes `text` to the handoff file and its Stop hook ends the
/// turn; the run checks the file.
fn agent_writes_handoff(s: &mut Setup, text: &str, prompt: &str) {
    let path = s.app.agent_handoff.runs[&s.terminal_id]
        .paths
        .handoff
        .clone();
    report_turn(s, TurnEvent::Start, prompt);
    std::fs::write(&path, text).unwrap();
    report_turn(s, TurnEvent::End, prompt);
    tick(&mut s.app);
    wait_worker(&mut s.app);
}

#[tokio::test]
async fn a_message_queued_during_the_run_waits_and_does_not_keep_compact_out() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let reply = message(&mut s, "mh1", "please review");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    agent_writes_handoff(&mut s, "# Handoff\n", "p1");
    assert_eq!(phase(&s), Some("await_compact"));
    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    assert_eq!(
        typed(&mut s.rx),
        "/compact",
        "the queued message does not hold it"
    );
    // Nor does the queue type its message into the compaction.
    let t0 = Instant::now();
    s.app.message_queue.mark_due();
    s.app
        .message_queue_pass(t0 + SETTLE, crate::notes::now_unix());
    assert!(typed(&mut s.rx).is_empty());
    report_compact(&mut s, CompactEvent::Post, None);
    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    assert!(typed(&mut s.rx).starts_with("herdr: compacted"));
    assert_eq!(phase(&s), None);
    // The run is over: the message goes in.
    let t0 = Instant::now();
    s.app.message_queue_pass(t0, crate::notes::now_unix());
    s.app
        .message_queue_pass(t0 + SETTLE, crate::notes::now_unix());
    let text = typed(&mut s.rx);
    assert!(text.contains("[herdr+ message mh1"), "{text:?}");
}

#[tokio::test]
async fn a_prompt_through_the_api_is_refused_while_the_run_holds_the_pane() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(s.app.handle_deferred_agent_api_request(
        Request {
            id: "p".into(),
            method: Method::AgentPrompt(crate::api::schema::AgentPromptParams {
                target: s.pane.clone(),
                text: "hello".into(),
                wait: None,
                guard_user_typing: false,
            }),
        },
        respond_to,
    ));
    let response: Value =
        serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(response["error"]["code"], "agent_not_ready", "{response}");
    assert!(typed(&mut s.rx).is_empty());
}

#[tokio::test]
async fn claudes_own_compaction_during_the_run_keeps_the_agents_handoff() {
    // In the wait for /compact: the saved handoff and its files go on.
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    agent_writes_handoff(&mut s, "# Handoff by the agent\n", "p1");
    assert_eq!(phase(&s), Some("await_compact"));
    let paths = s.app.agent_handoff.runs[&s.terminal_id].paths.clone();
    report_compact(&mut s, CompactEvent::Pre, Some("auto"));
    assert_eq!(phase(&s), Some("compacting"));
    let run = &s.app.agent_handoff.runs[&s.terminal_id];
    assert_eq!(run.paths, paths, "the run's own files");
    assert_eq!(run.kind, Some(Kind::Agent));
    wait_worker(&mut s.app);
    assert_eq!(
        std::fs::read_to_string(&paths.handoff).unwrap(),
        "# Handoff by the agent\n"
    );
    let post = report_compact(&mut s, CompactEvent::Post, None);
    let context = post["result"]["context"].as_str().expect("context");
    assert!(
        context.contains(&paths.handoff.display().to_string()),
        "{context}"
    );
    assert!(!context.contains("written by herdr"), "{context}");

    // In the handoff turn, the file written but not checked yet: kept too.
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let paths = s.app.agent_handoff.runs[&s.terminal_id].paths.clone();
    std::fs::write(&paths.handoff, "# Mine\n").unwrap();
    report_compact(&mut s, CompactEvent::Pre, Some("auto"));
    wait_worker(&mut s.app);
    assert_eq!(std::fs::read_to_string(&paths.handoff).unwrap(), "# Mine\n");
    assert_eq!(s.app.agent_handoff.saved[&s.terminal_id].kind, Kind::Agent);
    assert_eq!(
        s.app.agent_handoff.runs[&s.terminal_id].kind,
        Some(Kind::Agent)
    );

    // In the handoff turn without a file: herdr writes its own there.
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let paths = s.app.agent_handoff.runs[&s.terminal_id].paths.clone();
    report_compact(&mut s, CompactEvent::Pre, Some("manual"));
    wait_worker(&mut s.app);
    assert!(std::fs::read_to_string(&paths.handoff)
        .unwrap()
        .contains("written by herdr"));
    // The user's /compact: the resume line follows.
    report_compact(&mut s, CompactEvent::Post, None);
    assert_eq!(phase(&s), Some("await_resume"));
}

#[tokio::test]
async fn a_turn_end_without_the_file_gets_one_reminder_then_herdrs_handoff() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    report_turn(&mut s, TurnEvent::Start, "p1");
    report_turn(&mut s, TurnEvent::End, "p1");
    tick(&mut s.app);
    wait_worker(&mut s.app);
    let back = |s: &mut Setup| {
        let run = s.app.agent_handoff.runs.get_mut(&s.terminal_id).unwrap();
        run.missing_since = Some(Instant::now() - handoff::MISSING_GRACE);
    };
    back(&mut s);
    tick(&mut s.app);
    settle(&mut s);
    tick(&mut s.app);
    let reminder = typed(&mut s.rx);
    assert!(reminder.contains("still missing"), "{reminder}");
    assert_eq!(phase(&s), Some("await_handoff"));
    // The reminder's turn ends without the file too.
    std::thread::sleep(Duration::from_millis(5));
    report_turn(&mut s, TurnEvent::Start, "p2");
    report_turn(&mut s, TurnEvent::End, "p2");
    tick(&mut s.app);
    wait_worker(&mut s.app);
    back(&mut s);
    tick(&mut s.app);
    assert_eq!(phase(&s), Some("writing_fallback"));
    wait_worker(&mut s.app);
    assert_eq!(phase(&s), Some("await_compact"));
}

#[tokio::test]
async fn a_handoff_turn_stuck_on_a_prompt_tells_the_user_once() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    set_state(&mut s, AgentState::Blocked);
    tick(&mut s.app);
    s.app
        .agent_handoff
        .runs
        .get_mut(&s.terminal_id)
        .unwrap()
        .blocked_since = Some(Instant::now() - handoff::BLOCKED_NOTICE);
    tick(&mut s.app);
    tick(&mut s.app);
    let cards = s
        .app
        .agent_notices
        .list()
        .iter()
        .filter(|card| card.title.contains("waits on you"))
        .count();
    assert_eq!(cards, 1);
    assert_eq!(phase(&s), Some("await_handoff"), "the run goes on");
}

#[tokio::test]
async fn the_automatic_trigger_retries_after_a_refusal_or_a_stopped_run() {
    let mut s = setup(Agent::Claude);
    s.app.agents_config.handoff.at_percent = 30;
    let read = |s: &mut Setup, used: u64, now: Instant| {
        s.app
            .state
            .terminals
            .get_mut(&s.terminal_id)
            .unwrap()
            .agent_context = Some(crate::agent_context::ContextUsage {
            used,
            window: 20_000,
        });
        let id = s.terminal_id.clone();
        s.app.note_agent_handoff_context(&id, now);
    };
    let t0 = Instant::now();
    read(&mut s, 7_000, t0);
    assert_eq!(phase(&s), Some("await_prompt"));
    wait_worker(&mut s.app);
    // It stops before /compact: armed again, after a while.
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Err("never free".into()), t0);
    read(&mut s, 7_000, t0 + Duration::from_secs(5));
    assert_eq!(phase(&s), None, "not right away");
    read(&mut s, 7_000, t0 + AUTO_RETRY);
    assert_eq!(phase(&s), Some("await_prompt"), "tried again");
    wait_worker(&mut s.app);
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Ok("test".into()), t0);

    // A refused start (the Claude session is not known yet) stays armed
    // for a retry.
    let mut s = setup(Agent::Claude);
    s.app.agents_config.handoff.at_percent = 30;
    let session = |agent: &str| crate::agent_resume::PersistedAgentSession {
        source: format!("herdr:{agent}"),
        agent: agent.into(),
        session_ref: crate::agent_resume::AgentSessionRef::id(SESSION).unwrap(),
        transcript_path: None,
    };
    let terminal_id = s.terminal_id.clone();
    let set_session = |s: &mut Setup, agent: &str| {
        s.app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(session(agent));
    };
    set_session(&mut s, "codex");
    read(&mut s, 7_000, t0);
    assert_eq!(phase(&s), None);
    assert_eq!(s.app.agent_handoff.armed.get(&s.terminal_id), Some(&true));
    set_session(&mut s, "claude");
    read(&mut s, 7_000, t0 + Duration::from_secs(5));
    assert_eq!(phase(&s), None, "not on every read");
    read(&mut s, 7_000, t0 + AUTO_RETRY);
    assert_eq!(phase(&s), Some("await_prompt"));
    wait_worker(&mut s.app);
}

#[tokio::test]
async fn a_saved_handoff_is_offered_only_to_its_own_session() {
    let mut s = setup(Agent::Claude);
    report_compact(&mut s, CompactEvent::Pre, Some("auto"));
    wait_worker(&mut s.app);
    report_compact(&mut s, CompactEvent::Post, None);
    assert_eq!(phase(&s), None);
    assert!(s.app.agent_handoff_saved_unix(&s.terminal_id).is_some());
    // /clear: a new session in the pane.
    let response = s.app.handle_api_request(Request {
        id: "c".into(),
        method: Method::PaneReportCompact(PaneReportCompactParams {
            pane_id: s.pane.clone(),
            agent: "claude".into(),
            event: CompactEvent::Post,
            trigger: None,
            session_id: Some("another-session".into()),
            transcript_path: None,
        }),
    });
    let reply: Value = serde_json::from_str(&response).unwrap();
    assert!(reply["result"]["context"].is_null(), "{reply}");
    s.app
        .state
        .terminals
        .get_mut(&s.terminal_id)
        .unwrap()
        .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("another-session").unwrap(),
            transcript_path: None,
        });
    assert_eq!(s.app.agent_handoff_saved_unix(&s.terminal_id), None);
}

fn bind_session(s: &mut Setup, session: Option<&str>) {
    s.app
        .state
        .terminals
        .get_mut(&s.terminal_id)
        .unwrap()
        .persisted_agent_session = session.map(|id| crate::agent_resume::PersistedAgentSession {
        source: "herdr:claude".into(),
        agent: "claude".into(),
        session_ref: crate::agent_resume::AgentSessionRef::id(id).unwrap(),
        transcript_path: None,
    });
}

fn messages_ready(s: &Setup) -> bool {
    let target = s
        .app
        .resolve_terminal_target(&s.terminal_id.to_string())
        .unwrap();
    matches!(
        s.app.message_check(&target),
        super::super::message_queue::Check::Ready { .. }
    )
}

#[tokio::test]
async fn a_new_session_in_the_pane_ends_the_run_and_releases_held_messages() {
    // `/clear` (or a resume of another session) during the handoff turn.
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let reply = message(&mut s, "ms1", "please review");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert!(!messages_ready(&s));
    bind_session(&mut s, Some("another-session"));
    tick(&mut s.app);
    assert_eq!(phase(&s), None, "the run ended");
    assert!(messages_ready(&s), "held messages go in");
    assert!(s.app.agent_notices.list().iter().any(|card| card
        .body
        .as_deref()
        .is_some_and(|body| body.contains("session changed"))));

    // During the compaction too (Claude keeps the id across /compact, so
    // a new one there is another session as well).
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    let now = Instant::now();
    s.app
        .agent_handoff
        .runs
        .get_mut(&s.terminal_id)
        .unwrap()
        .enter(Phase::Compacting { typed_at: now }, now);
    bind_session(&mut s, Some("another-session"));
    tick(&mut s.app);
    assert_eq!(phase(&s), None);
    assert!(messages_ready(&s));
}

#[tokio::test]
async fn an_empty_session_binding_does_not_end_the_run() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    // Briefly empty around SessionStart.
    bind_session(&mut s, None);
    assert!(s.app.state.terminals[&s.terminal_id]
        .persistable_agent_session()
        .is_none());
    tick(&mut s.app);
    assert_eq!(phase(&s), Some("await_handoff"));
    // The same session again: the run goes on.
    bind_session(&mut s, Some(SESSION));
    tick(&mut s.app);
    assert_eq!(phase(&s), Some("await_handoff"));
}

#[tokio::test]
async fn a_compaction_report_for_another_session_gets_nothing_from_the_run() {
    let mut s = setup(Agent::Claude);
    to_prompt_typed(&mut s);
    agent_writes_handoff(&mut s, "# Handoff\n", "p1");
    let now = Instant::now();
    s.app
        .agent_handoff
        .runs
        .get_mut(&s.terminal_id)
        .unwrap()
        .enter(Phase::Compacting { typed_at: now }, now);
    let response = s.app.handle_api_request(Request {
        id: "c".into(),
        method: Method::PaneReportCompact(PaneReportCompactParams {
            pane_id: s.pane.clone(),
            agent: "claude".into(),
            event: CompactEvent::Post,
            trigger: None,
            session_id: Some("another-session".into()),
            transcript_path: None,
        }),
    });
    let reply: Value = serde_json::from_str(&response).unwrap();
    assert!(reply["result"]["context"].is_null(), "{reply}");
    assert_eq!(phase(&s), None, "the run ended, no resume line");
    assert!(messages_ready(&s));
    tick(&mut s.app);
    assert!(typed(&mut s.rx).is_empty());
}

#[tokio::test]
async fn each_wait_on_a_hung_worker_aborts_and_releases_held_messages() {
    let waits: [fn(&mut Run, Instant); 3] = [
        // The transcript copy before the prompt.
        |run, now| {
            run.transcript_saved = None;
            run.enter(Phase::AwaitPrompt, now - handoff::WORKER_TIMEOUT);
        },
        // A file check in flight.
        |run, now| {
            run.phase = Phase::AwaitHandoff {
                typed_at: now,
                checked: Some(now),
                checking: true,
            };
            run.phase_since = now;
            run.check_since = Some(now - handoff::WORKER_TIMEOUT);
        },
        // herdr's own handoff.
        |run, now| run.enter(Phase::WritingFallback, now - handoff::WORKER_TIMEOUT),
    ];
    for (n, hang) in waits.into_iter().enumerate() {
        let mut s = setup(Agent::Claude);
        to_prompt_typed(&mut s);
        let reply = message(&mut s, &format!("mw{n}"), "please review");
        assert_eq!(reply["result"]["outcome"], "queued", "{n}: {reply}");
        let now = Instant::now();
        let run = s.app.agent_handoff.runs.get_mut(&s.terminal_id).unwrap();
        hang(run, now);
        let id = run.id;
        s.app.agent_handoff.pending_writes.insert(
            id,
            PendingWrite {
                terminal_id: s.terminal_id.clone(),
                saved: SavedHandoff {
                    session_id: SESSION.into(),
                    paths: run.paths.clone(),
                    kind: Kind::Mechanical,
                    at: now,
                    at_unix: 0,
                    transcript: false,
                },
            },
        );
        tick(&mut s.app);
        assert_eq!(phase(&s), None, "{n}: aborted");
        assert!(messages_ready(&s), "{n}: held messages go in");
        assert!(
            !s.app.agent_handoff.pending_writes.contains_key(&id),
            "{n}: a late write is not recorded for the failed run"
        );
        assert!(
            s.app
                .agent_notices
                .list()
                .iter()
                .any(|card| card.title.contains("Handoff")),
            "{n}: the user is told"
        );
    }
}

#[test]
fn the_loop_guard_holds_only_other_agents_back_for_ten_minutes() {
    let recent = Some(Duration::from_secs(60));
    let old = Some(HANDOFF_COOLDOWN);
    assert_eq!(
        handoff_cooldown_left(true, false, recent),
        Some(HANDOFF_COOLDOWN - Duration::from_secs(60))
    );
    assert_eq!(handoff_cooldown_left(true, false, old), None);
    assert_eq!(handoff_cooldown_left(true, false, None), None);
    assert_eq!(
        handoff_cooldown_left(false, false, recent),
        None,
        "the agent itself or the user"
    );
    assert_eq!(
        handoff_cooldown_left(true, true, recent),
        None,
        "a run on answers already_running"
    );
}

#[tokio::test]
async fn the_coordinator_may_not_hand_off_an_agent_again_within_ten_minutes() {
    let mut s = setup(Agent::Claude);
    // The coordinator in a second space.
    s.app.state.workspaces.push(Workspace::test_new("coord"));
    s.app.state.ensure_test_terminals();
    let coordinator_pane = s.app.state.workspaces[1].tabs[0].root_pane;
    let coordinator_terminal = s.app.state.workspaces[1].tabs[0].panes[&coordinator_pane]
        .attached_terminal_id
        .clone();
    s.app
        .state
        .terminals
        .get_mut(&coordinator_terminal)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    s.app.state.coordinator_terminal_id = Some(coordinator_terminal);
    let coordinator = s.app.public_pane_id(1, coordinator_pane).unwrap();
    let pane = s.pane.clone();

    // A run that compacted just now.
    to_prompt_typed(&mut s);
    let now = Instant::now();
    s.app
        .agent_handoff
        .runs
        .get_mut(&s.terminal_id)
        .unwrap()
        .enter(Phase::Compacting { typed_at: now }, now);
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Ok("compacted".into()), now);
    assert!(s
        .app
        .agent_handoff
        .last_finished
        .contains_key(&s.terminal_id));

    let reply = handoff_api(&mut s.app, Some(&coordinator), &pane);
    assert_eq!(reply["error"]["code"], "loop_guard", "{reply}");
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("10 minutes"));
    assert_eq!(phase(&s), None);

    // The agent itself may.
    let reply = handoff_api(&mut s.app, Some(&pane), &pane);
    assert_eq!(reply["result"]["handoff"]["outcome"], "started", "{reply}");
    wait_worker(&mut s.app);
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Err("test".into()), now);
    // So may the user.
    let reply = handoff_api(&mut s.app, None, &pane);
    assert_eq!(reply["result"]["handoff"]["outcome"], "started", "{reply}");
    wait_worker(&mut s.app);
    s.app
        .finish_agent_handoff(&s.terminal_id.clone(), Err("test".into()), now);

    // Ten minutes on, the coordinator may again.
    s.app
        .agent_handoff
        .last_finished
        .insert(s.terminal_id.clone(), Instant::now() - HANDOFF_COOLDOWN);
    let reply = handoff_api(&mut s.app, Some(&coordinator), &pane);
    assert_eq!(reply["result"]["handoff"]["outcome"], "started", "{reply}");
    wait_worker(&mut s.app);
}

#[test]
fn the_handoff_page_says_how_much_of_a_long_page_was_left_out() {
    assert_eq!(handoff_page("# Plan\n- [ ] ship"), "# Plan\n- [ ] ship");
    let long: String = (0..2000).map(|i| format!("line {i}\n")).collect();
    let page = handoff_page(&long);
    assert!(page.starts_with("line 0\nline 1\n"), "{page}");
    let (head, left_out) = crate::notes::recall::notes_head(&long);
    assert!(left_out > 0);
    assert_eq!(
        page,
        format!("{head}\n(the top; {left_out} more lines: agents_notes_read)")
    );
}
