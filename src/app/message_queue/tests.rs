use super::*;
use crate::api::schema::{Method, Request};
use crate::config::Config;
use crate::detect::{Agent, AgentState};
use crate::workspace::Workspace;
use bytes::Bytes;
use serde_json::Value;

const CLAUDE_EMPTY: &[u8] =
    "\r\n─────\r\n❯ \x1b[2mTry \"something\"\x1b[0m\r\n─────\r\n  footer".as_bytes();

fn app() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("messages")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    app
}

fn pane(app: &App) -> crate::layout::PaneId {
    app.state.workspaces[0].tabs[0].root_pane
}

fn terminal(app: &mut App) -> &mut crate::terminal::TerminalState {
    let pane_id = pane(app);
    let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    app.state.terminals.get_mut(&terminal_id).unwrap()
}

/// A Claude agent named `rev` in `state`, with a fresh runtime whose writes
/// come out of the returned receiver.
fn rev(app: &mut App, state: AgentState) -> tokio::sync::mpsc::Receiver<Bytes> {
    let terminal = terminal(app);
    terminal.set_agent_name("rev".into());
    terminal.set_detected_state(Some(Agent::Claude), state);
    fresh_runtime(app)
}

fn fresh_runtime(app: &mut App) -> tokio::sync::mpsc::Receiver<Bytes> {
    let (runtime, rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(40, 8, 0, b"", 4);
    runtime.test_process_pty_bytes(CLAUDE_EMPTY);
    let pane_id = pane(app);
    app.state.insert_test_runtime(pane_id, runtime);
    rx
}

fn set_state(app: &mut App, state: AgentState) {
    terminal(app).set_detected_state(Some(Agent::Claude), state);
    app.message_queue.mark_due();
}

fn params(id: &str, body: &str) -> AgentMessageSendParams {
    AgentMessageSendParams {
        target: "rev".into(),
        id: id.into(),
        envelope: format!("[herdr+ message {id} from lead (w9:p1) 10:00 — another agent]\n{body}"),
        text: body.into(),
        unix: 0,
        from_pane: Some("w9:p1".into()),
        from_name: Some("lead".into()),
        ..AgentMessageSendParams::default()
    }
}

fn send(app: &mut App, id: &str, body: &str) -> Value {
    let response = app.handle_agent_message_send("req".into(), params(id, body));
    serde_json::from_str(&response).unwrap()
}

/// Everything written so far (the test runtime writes from its own thread).
fn typed(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> String {
    std::thread::sleep(Duration::from_millis(100));
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

fn outcomes(app: &App) -> Vec<(String, String, bool)> {
    app.message_queue
        .written
        .iter()
        .map(|line| {
            (
                line.id.clone().unwrap_or_default(),
                line.outcome.clone(),
                line.is_update(),
            )
        })
        .collect()
}

fn now_unix() -> u64 {
    crate::coordinator::now_unix()
}

#[tokio::test]
async fn an_idle_target_gets_the_message_now() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let reply = send(&mut app, "ma1", "hello");
    assert_eq!(reply["result"]["outcome"], "sent", "{reply}");
    assert!(typed(&mut rx).contains("hello"));
    assert!(app.message_queue.is_empty());
    assert!(
        app.message_queue.written.is_empty(),
        "the sender logs a sent message"
    );
}

#[tokio::test]
async fn a_busy_target_queues_and_gets_it_once_idle_and_settled() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    let reply = send(&mut app, "ma1", "please review");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], "working");
    assert!(
        typed(&mut rx).is_empty(),
        "nothing typed into a working agent"
    );
    assert_eq!(outcomes(&app), [("ma1".into(), "queued".into(), false)]);
    let line = &app.message_queue.written[0];
    assert_eq!(line.text, "please review");
    assert_eq!(line.from_pane.as_deref(), Some("w9:p1"));

    // Still working: nothing, however often the queue looks.
    let t0 = Instant::now();
    assert!(!app.message_queue_pass(t0, now_unix()));
    assert!(typed(&mut rx).is_empty());

    // Idle: it waits to settle, then goes in.
    set_state(&mut app, AgentState::Idle);
    assert!(!app.message_queue_pass(t0, now_unix()));
    assert_eq!(app.next_message_queue_deadline(t0), Some(t0 + SETTLE));
    assert!(!app.message_queue_pass(t0 + Duration::from_secs(1), now_unix()));
    assert!(typed(&mut rx).is_empty(), "not settled yet");
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let text = typed(&mut rx);
    assert!(text.contains("[herdr+ message ma1"), "{text:?}");
    assert!(app.message_queue.is_empty());
    assert_eq!(
        outcomes(&app),
        [
            ("ma1".into(), "queued".into(), false),
            ("ma1".into(), "delivered".into(), true),
        ]
    );
    assert_eq!(app.next_message_queue_deadline(t0), None);
}

#[tokio::test]
async fn queued_messages_go_in_fifo_as_one_paste_and_later_ones_queue_behind() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    send(&mut app, "ma1", "first");
    send(&mut app, "ma2", "second");
    set_state(&mut app, AgentState::Idle);
    // A message sent while others wait for the same target queues behind
    // them even though the target is idle now.
    let reply = send(&mut app, "ma3", "third");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert!(typed(&mut rx).is_empty());

    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let text = typed(&mut rx);
    let (a, b, c) = (
        text.find("first").unwrap(),
        text.find("second").unwrap(),
        text.find("third").unwrap(),
    );
    assert!(a < b && b < c, "{text}");
    assert_eq!(
        text.matches("[herdr+ message").count(),
        3,
        "each keeps its envelope"
    );
    assert!(app.message_queue.is_empty());
    let delivered: Vec<String> = outcomes(&app)
        .into_iter()
        .filter(|(_, outcome, _)| outcome == "delivered")
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(delivered, ["ma1", "ma2", "ma3"]);
}

#[tokio::test]
async fn the_typing_guard_holds_queued_messages_until_the_user_is_quiet() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let pane_id = pane(&app);
    app.lookup_runtime_sender(0, pane_id)
        .unwrap()
        .note_user_input(Instant::now());
    let reply = send(&mut app, "ma1", "hi");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], "its user is typing in it");
    assert!(typed(&mut rx).is_empty());

    // Settled, but the user still typed recently: held, retried later.
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(!app.message_queue_pass(t0 + SETTLE, now_unix()));
    assert!(
        typed(&mut rx).is_empty(),
        "never typed while the user types"
    );
    assert_eq!(
        app.next_message_queue_deadline(t0 + SETTLE),
        Some(t0 + SETTLE + RETRY)
    );

    // Quiet again (a runtime without recent input): typed in at the retry.
    let mut rx = fresh_runtime(&mut app);
    assert!(app.message_queue_pass(t0 + SETTLE + RETRY, now_unix()));
    assert!(typed(&mut rx).contains("hi"));
}

#[tokio::test]
async fn blocked_and_suspended_targets_are_never_typed_into() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Blocked);
    let reply = send(&mut app, "ma1", "hi");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    let t0 = Instant::now();
    for step in 0..4 {
        app.message_queue.mark_due();
        app.message_queue_pass(t0 + SETTLE * step, now_unix());
    }
    assert!(typed(&mut rx).is_empty(), "no typing into a dialog");
    assert_eq!(app.message_queue.entries.len(), 1);
}

#[tokio::test]
async fn an_agent_herdr_is_starting_gets_its_message_queued_not_refused() {
    let mut app = app();
    let mut rx = fresh_runtime(&mut app);
    // Launched (agents_open_tab, agent start) but not detected yet.
    terminal(&mut app).begin_managed_agent(
        "rev".into(),
        Agent::Claude,
        Instant::now(),
        Duration::from_secs(1),
        Duration::from_secs(30),
    );
    assert!(terminal(&mut app).effective_known_agent().is_none());
    let reply = send(&mut app, "ma1", "welcome");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], "starting");
    assert!(typed(&mut rx).is_empty());
    assert_eq!(app.message_queue.entries.len(), 1);
}

#[tokio::test]
async fn queued_messages_expire_after_two_hours() {
    let mut app = app();
    rev(&mut app, AgentState::Working);
    send(&mut app, "ma1", "hi");
    let entry = &app.message_queue.entries[0];
    assert!(entry.expires_unix >= now_unix() + MESSAGE_TTL_S - 5);
    let later = entry.expires_unix;
    let now = Instant::now();
    assert!(app.next_message_queue_deadline(now).is_some());
    assert!(app.message_queue_pass(now, later));
    assert!(app.message_queue.is_empty());
    assert_eq!(outcomes(&app)[1], ("ma1".into(), "expired".into(), true));
}

#[tokio::test]
async fn a_gone_target_drops_its_messages_after_the_grace() {
    let mut app = app();
    rev(&mut app, AgentState::Working);
    send(&mut app, "ma1", "hi");
    // The agent exits (the pane is a bare shell again).
    terminal(&mut app).set_detected_state(None, AgentState::Unknown);
    app.message_queue.mark_due();
    let t0 = Instant::now();
    assert!(!app.message_queue_pass(t0, now_unix()));
    assert_eq!(
        app.message_queue.entries.len(),
        1,
        "a restart may bring it back"
    );
    assert_eq!(app.next_message_queue_deadline(t0), Some(t0 + GONE_GRACE));
    assert!(app.message_queue_pass(t0 + GONE_GRACE, now_unix()));
    assert!(app.message_queue.is_empty());
    assert_eq!(outcomes(&app)[1], ("ma1".into(), "dropped".into(), true));
}

#[tokio::test]
async fn a_claim_by_the_target_takes_it_off_the_queue() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    send(&mut app, "ma1", "hi");
    let target = app.public_pane_id(0, pane(&app)).unwrap();
    let claim = |app: &mut App, pane: &str| -> Value {
        let response = app.handle_agent_message_claim(
            "c".into(),
            AgentMessageClaimParams {
                id: "ma1".into(),
                pane: pane.into(),
            },
        );
        serde_json::from_str(&response).unwrap()
    };
    assert_eq!(
        claim(&mut app, "w9:p9")["result"]["claimed"],
        false,
        "not its target"
    );
    assert_eq!(claim(&mut app, &target)["result"]["claimed"], true);
    assert!(app.message_queue.is_empty());
    assert_eq!(outcomes(&app)[1], ("ma1".into(), "delivered".into(), true));
    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    app.message_queue_pass(t0 + SETTLE, now_unix());
    assert!(
        typed(&mut rx).is_empty(),
        "a claimed message is not typed in"
    );
}

#[tokio::test]
async fn the_queue_survives_a_restart() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    let dir = std::env::temp_dir().join(format!("herdr-message-queue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join(FILE_NAME);
    app.message_queue.store = Some(path.clone());
    app.message_queue.log_dir = None;
    send(&mut app, "ma1", "survives");
    // The writer thread saves it.
    let saved = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("ma1"))
    });
    assert!(saved, "message_queue.json written");

    // A new server process loads it and delivers once the target is free.
    let mut restarted = MessageQueue::default();
    restarted.load(path.clone());
    assert_eq!(restarted.entries.len(), 1);
    assert_eq!(restarted.entries[0].message.text, "survives");
    app.message_queue = restarted;
    app.message_queue.log_dir = None;
    terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    assert!(typed(&mut rx).contains("survives"));
    let emptied = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        std::fs::read_to_string(&path).is_ok_and(|text| !text.contains("ma1"))
    });
    assert!(emptied, "the delivered message left the file");
    let _ = std::fs::remove_dir_all(&dir);
}

fn deferred_prompt(app: &mut App, id: &str, text: &str, guard: bool) -> Value {
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_agent_api_request(
        Request {
            id: id.into(),
            method: Method::AgentPrompt(AgentPromptParams {
                target: "rev".into(),
                text: text.into(),
                wait: None,
                guard_user_typing: guard,
            }),
        },
        respond_to,
    ));
    let response = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("a response");
    serde_json::from_str(&response).unwrap()
}

#[tokio::test]
async fn an_older_mcp_servers_prompt_is_guarded_and_queued_as_a_success() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let pane_id = pane(&app);
    app.lookup_runtime_sender(0, pane_id)
        .unwrap()
        .note_user_input(Instant::now());
    let envelope = "[herdr+ message mold1 from lead (w9:p1) 10:00 — another agent]\nold hello";

    // The oldest servers send no guard: it is applied anyway, and the
    // message is queued but answered like a typed-in prompt.
    let reply = deferred_prompt(&mut app, "coordinator:agent.prompt", envelope, false);
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(reply["result"]["type"], "agent_prompted", "{reply}");
    assert!(
        typed(&mut rx).is_empty(),
        "nothing typed while the user types"
    );
    assert_eq!(app.message_queue.entries.len(), 1);
    assert!(app.message_queue.entries[0].legacy);
    // The old sender logs the message itself: only an update line here.
    assert_eq!(outcomes(&app), [("mold1".into(), "queued".into(), true)]);

    // A working target too (the POC's ids as well).
    terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
    let reply = deferred_prompt(
        &mut app,
        "plus:agent.prompt",
        "[herdr+ message mold2 from lead]\nsecond",
        true,
    );
    assert!(reply.get("error").is_none(), "{reply}");
    assert_eq!(app.message_queue.entries.len(), 2);

    // Free and quiet: both go in, in order.
    let mut rx = fresh_runtime(&mut app);
    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let text = typed(&mut rx);
    assert!(
        text.find("old hello").unwrap() < text.find("second").unwrap(),
        "{text}"
    );
    assert!(app.message_queue.is_empty());
}

#[tokio::test]
async fn a_users_own_prompt_is_not_queued_or_guarded() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let pane_id = pane(&app);
    app.lookup_runtime_sender(0, pane_id)
        .unwrap()
        .note_user_input(Instant::now());
    let reply = deferred_prompt(&mut app, "cli:agent:prompt", "my own", false);
    assert_eq!(reply["result"]["type"], "agent_prompted", "{reply}");
    assert!(typed(&mut rx).contains("my own"));
    assert!(app.message_queue.is_empty());
    assert!(!is_legacy_agent_message("cli:agent:prompt"));
    assert!(is_legacy_agent_message("coordinator:agent.prompt"));
}

#[test]
fn only_agent_events_mark_a_non_empty_queue_due() {
    use crate::api::schema::EventKind;
    let mut app = app();
    app.note_message_queue_event(&EventKind::PaneAgentStatusChanged);
    assert!(!app.message_queue.due, "nothing to do while empty");
    app.message_queue.entries.push(QueuedMessage {
        message: AgentMessage::default(),
        envelope: "x".into(),
        terminal_id: "t".into(),
        session: None,
        expires_unix: u64::MAX,
        legacy: false,
        from_terminal: None,
        pointer: None,
        stuck_notified: false,
    });
    app.note_message_queue_event(&EventKind::TabRenamed);
    assert!(!app.message_queue.due);
    app.note_message_queue_event(&EventKind::PaneAgentStatusChanged);
    assert!(app.message_queue.due);
    let now = Instant::now();
    assert_eq!(app.next_message_queue_deadline(now), Some(now));
}

#[test]
fn envelope_ids_are_read_from_the_header_only() {
    assert_eq!(
        envelope_id("[herdr+ message mab12 (reply to mx) from a]\nbody").as_deref(),
        Some("mab12")
    );
    assert_eq!(envelope_id("hello [herdr+ message mab12"), None);
    assert_eq!(envelope_id("[herdr+ message NOPE]"), None);
}

#[tokio::test]
async fn a_target_in_voice_mode_gets_its_messages_queued_until_voice_mode_ends() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let pane_id = pane(&app);
    app.apply_agent_voice(
        pane_id,
        crate::detect::AgentVoice::Off,
        crate::detect::AgentVoice::Live,
    );
    let reply = send(&mut app, "ma1", "hi");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], "voice mode");
    assert!(
        typed(&mut rx).is_empty(),
        "never typed into a voice session"
    );

    // Muted is still voice mode: held.
    app.apply_agent_voice(
        pane_id,
        crate::detect::AgentVoice::Live,
        crate::detect::AgentVoice::Muted,
    );
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(!app.message_queue_pass(t0 + SETTLE, now_unix()));
    assert!(typed(&mut rx).is_empty(), "never typed while muted either");
    assert_eq!(app.message_queue.entries.len(), 1);

    // Voice mode off: the queue is due again and the message goes in.
    app.apply_agent_voice(
        pane_id,
        crate::detect::AgentVoice::Muted,
        crate::detect::AgentVoice::Off,
    );
    app.message_queue_pass(t0 + SETTLE, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE * 2, now_unix()));
    assert!(typed(&mut rx).contains("hi"));
}

/// The target's own hooks report its turn's start and end.
fn hook_turn(app: &mut App, start: bool, prompt: &str, seq: u64) {
    terminal(app)
        .turn_mut()
        .note_hook_turn(start, Some(prompt.into()), seq, Instant::now());
    app.message_queue.mark_due();
}

#[tokio::test]
async fn a_working_target_whose_hooks_ended_its_turn_gets_it_after_settle() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    hook_turn(&mut app, true, "p1", 10);
    hook_turn(&mut app, false, "p1", 20);
    let reply = send(&mut app, "mh1", "after your turn");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], "its turn just ended");
    assert!(typed(&mut rx).is_empty(), "never typed at once");
    let t0 = Instant::now();
    let now = now_unix();
    assert!(!app.message_queue_pass(t0, now));
    assert!(app.message_queue_pass(t0 + SETTLE, now));
    assert!(typed(&mut rx).contains("after your turn"));
    assert!(app.message_queue.is_empty());
}

#[tokio::test]
async fn a_hook_ended_target_whose_screen_moves_waits_until_it_stood_still_for_settle() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    hook_turn(&mut app, true, "p1", 10);
    hook_turn(&mut app, false, "p1", 20);
    let reply = send(&mut app, "mh3", "after your turn");
    assert_eq!(reply["result"]["reason"], "its turn just ended", "{reply}");
    let t0 = Instant::now();
    let now = now_unix();
    let write = |app: &App, bytes: &str| {
        app.lookup_runtime_sender(0, pane(app))
            .unwrap()
            .test_process_pty_bytes(bytes.as_bytes());
    };
    assert!(!app.message_queue_pass(t0, now));
    // The turn goes on (a blocking Stop hook): its spinner above the
    // prompt box ticks.
    write(&app, "\x1b[1;1H\u{273b} Hashing\u{2026} (1s)");
    app.message_queue.mark_due();
    let t1 = t0 + Duration::from_secs(1);
    assert!(!app.message_queue_pass(t1, now));
    assert!(
        !app.message_queue_pass(t0 + SETTLE, now),
        "the settle restarted"
    );
    assert!(
        typed(&mut rx).is_empty(),
        "never typed into the moving turn"
    );
    assert_eq!(
        app.next_message_queue_deadline(t0 + SETTLE),
        Some(t1 + SETTLE)
    );
    // Still since t1: in at t1 + SETTLE.
    assert!(app.message_queue_pass(t1 + SETTLE, now));
    assert!(typed(&mut rx).contains("after your turn"));
    assert!(app.message_queue.settle_screens.is_empty());
}

#[tokio::test]
async fn a_hook_ended_target_whose_panel_ticks_under_the_prompt_box_gets_it_after_settle() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    hook_turn(&mut app, true, "p1", 10);
    hook_turn(&mut app, false, "p1", 20);
    send(&mut app, "mh4", "after your turn");
    let t0 = Instant::now();
    let now = now_unix();
    assert!(!app.message_queue_pass(t0, now));
    // Claude's background-agent panel (its timer) sits under the box.
    app.lookup_runtime_sender(0, pane(&app))
        .unwrap()
        .test_process_pty_bytes("\x1b[5;1H  general-purpose 48s".as_bytes());
    app.message_queue.mark_due();
    assert!(!app.message_queue_pass(t0 + Duration::from_secs(1), now));
    assert!(app.message_queue_pass(t0 + SETTLE, now));
    assert!(typed(&mut rx).contains("after your turn"));
}

#[tokio::test]
async fn a_hook_ended_turn_never_frees_a_blocked_or_suspended_target() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Blocked);
    hook_turn(&mut app, false, "p1", 20);
    let reply = send(&mut app, "mh2", "approve?");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    let t0 = Instant::now();
    let now = now_unix();
    app.message_queue_pass(t0, now);
    assert!(!app.message_queue_pass(t0 + SETTLE * 2, now));
    assert!(typed(&mut rx).is_empty(), "a permission dialog keeps it");

    set_state(&mut app, AgentState::Working);
    terminal(&mut app).begin_agent_suspend(
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("s1").expect("a session id"),
            transcript_path: None,
        },
        Instant::now() + Duration::from_secs(30),
    );
    app.message_queue_pass(t0 + SETTLE * 3, now);
    assert!(!app.message_queue_pass(t0 + SETTLE * 5, now));
    assert!(typed(&mut rx).is_empty(), "a suspended agent keeps it");
}

/// Queue one message to a working `rev`, 11 minutes old, then sample every
/// minute for 11 minutes; `tick` writes into the screen at each sample.
/// The sender is `rev` itself (the only pane), so its notice is queued.
fn run_stuck_check(tick: impl Fn(&crate::terminal::TerminalRuntime, usize)) -> bool {
    run_stuck_check_from(true, tick)
}

fn run_stuck_check_from(
    sender_alive: bool,
    tick: impl Fn(&crate::terminal::TerminalRuntime, usize),
) -> bool {
    let mut app = app();
    let _rx = rev(&mut app, AgentState::Working);
    let reply = send(&mut app, "ms1", "status?");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    app.message_queue.entries[0].message.unix -= 11 * 60;
    app.message_queue.entries[0].message.from_pane = if sender_alive {
        app.public_pane_id(0, pane(&app))
    } else {
        Some("w9:p9".into())
    };
    let now = now_unix();
    let t0 = Instant::now();
    app.message_queue.mark_due();
    app.message_queue_pass(t0, now);
    for minute in 1..=11u32 {
        let pane_id = pane(&app);
        if let Some(runtime) = app.lookup_runtime_sender(0, pane_id) {
            tick(runtime, minute as usize);
        }
        app.message_queue_pass(t0 + STUCK_SAMPLE * minute, now);
    }
    let notified = app.message_queue.entries[0].stuck_notified;
    // The notice burns the target's slot only when it went somewhere.
    assert_eq!(
        app.message_queue
            .stuck_notice_at
            .contains_key(&app.message_queue.entries[0].terminal_id),
        notified
    );
    notified
}

#[tokio::test]
async fn a_stuck_message_whose_sender_is_gone_keeps_its_notice_unused() {
    assert!(!run_stuck_check_from(false, |_, _| {}));
}

#[tokio::test]
async fn a_screen_frozen_above_its_prompt_box_is_stuck_but_a_live_turn_is_not() {
    // Nothing changes: stuck.
    assert!(run_stuck_check(|_, _| {}));
    // Only a panel under the prompt box ticks (Claude's subagent rows):
    // still stuck.
    assert!(run_stuck_check(|runtime, minute| {
        runtime.test_process_pty_bytes(format!("\x1b[5;1Hpanel {minute}m").as_bytes());
    }));
    // A live turn's spinner above the prompt box changes: never stuck.
    assert!(!run_stuck_check(|runtime, minute| {
        runtime.test_process_pty_bytes(
            format!("\x1b[1;1H\u{273b} Hashing\u{2026} ({minute}m)").as_bytes(),
        );
    }));
}

#[tokio::test]
async fn an_idle_target_whose_hooks_report_a_live_turn_waits_for_its_stop() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Idle);
    hook_turn(&mut app, true, "p1", 10);
    let reply = send(&mut app, "ml1", "after your turn");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], HOOK_LIVE_REASON);
    let t0 = Instant::now();
    let now = now_unix();
    app.message_queue_pass(t0, now);
    assert!(!app.message_queue_pass(t0 + SETTLE * 2, now));
    assert!(typed(&mut rx).is_empty(), "the hooks say the turn is live");
    // The bounded hold schedules its own recheck.
    assert!(app
        .next_message_queue_deadline(t0)
        .is_some_and(|at| at <= t0 + crate::agents_model::turn::HOOK_LIVE_HOLD));
    hook_turn(&mut app, false, "p1", 20);
    let t1 = t0 + SETTLE * 3;
    app.message_queue_pass(t1, now);
    assert!(app.message_queue_pass(t1 + SETTLE, now));
    assert!(typed(&mut rx).contains("after your turn"));
}

#[test]
fn only_a_shown_blocker_forbids_the_interrupt() {
    let detection = |state, skip, blocker| crate::detect::AgentDetection {
        state,
        skip_state_update: skip,
        visible_idle: false,
        visible_blocker: blocker,
        visible_working: false,
        voice: crate::detect::AgentVoice::Off,
    };
    assert!(!super::detection_blocks_interrupt(&detection(
        AgentState::Working,
        false,
        false
    )));
    assert!(!super::detection_blocks_interrupt(&detection(
        AgentState::Idle,
        false,
        false
    )));
    assert!(super::detection_blocks_interrupt(&detection(
        AgentState::Blocked,
        false,
        false
    )));
    assert!(super::detection_blocks_interrupt(&detection(
        AgentState::Working,
        false,
        true
    )));
    assert!(!super::detection_blocks_interrupt(&detection(
        AgentState::Blocked,
        true,
        false
    )));
}

#[test]
fn stuck_notice_durations_read_in_minutes() {
    assert_eq!(super::minutes(Duration::from_secs(5)), "1m");
    assert_eq!(super::minutes(Duration::from_secs(12 * 60)), "12m");
    assert_eq!(super::minutes(Duration::from_secs(65 * 60)), "1h05m");
}

/// An urgent message to `rev`, as agents.send_message hands it over after
/// its policy and rate-limit checks.
fn urgent(app: &mut App, id: &str, body: &str) -> Result<(MessageDelivery, bool), MessageRefused> {
    urgent_from(app, id, body, "lead (w9:p1)")
}

fn urgent_from(
    app: &mut App,
    id: &str,
    body: &str,
    sender: &str,
) -> Result<(MessageDelivery, bool), MessageRefused> {
    let terminal_id = terminal(app).id.to_string();
    let target = app
        .resolve_agent_target("rev")
        .or_else(|_| app.resolve_terminal_target(&terminal_id))
        .unwrap();
    let check = app.agent_message_check(&target);
    let envelope =
        format!("[herdr+ URGENT message {id} from {sender} 10:00 — another agent]\n{body}");
    let pointer = PointerFrom::from_envelope(id, &envelope);
    assert!(pointer.urgent);
    let message = AgentMessage {
        unix: now_unix(),
        to_pane: "rev".into(),
        text: body.into(),
        outcome: OUTCOME_QUEUED.into(),
        id: Some(id.into()),
        ..AgentMessage::default()
    };
    app.deliver_urgent_message(
        "req",
        &target,
        check,
        message,
        envelope,
        pointer,
        None,
        now_unix(),
    )
}

const ESC: &str = "\u{1b}";

#[tokio::test]
async fn an_urgent_message_to_a_free_target_goes_in_now_without_an_esc() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Idle);
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop the deploy").unwrap();
    assert_eq!(delivery, MessageDelivery::Sent);
    assert!(!interrupted);
    let typed = typed(&mut rx);
    assert!(typed.contains("stop the deploy"), "{typed:?}");
    assert!(!typed.starts_with(ESC), "{typed:?}");
}

#[tokio::test]
async fn an_urgent_message_interrupts_a_working_claude_and_goes_in_first_once_idle() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    let earlier = send(&mut app, "ma1", "when you have a moment");
    assert_eq!(earlier["result"]["outcome"], "queued", "{earlier}");
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop the deploy").unwrap();
    assert!(interrupted);
    let MessageDelivery::Queued { reason } = delivery else {
        panic!("{delivery:?}");
    };
    assert!(reason.contains("interrupted"), "{reason}");
    assert_eq!(typed(&mut rx), ESC, "one Esc, nothing else yet");
    assert_eq!(app.message_queue.entries[0].id(), "mu1", "ahead of ma1");

    // Another urgent message right after: no second Esc (two open Claude's
    // rewind menu).
    let (_, again) = urgent(&mut app, "mu2", "really, stop").unwrap();
    assert!(!again);
    assert!(typed(&mut rx).is_empty());

    // The interrupted turn ends (the screen reads idle): typed in after SETTLE.
    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(typed(&mut rx).is_empty(), "settling");
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let input = typed(&mut rx);
    assert!(input.contains("URGENT"), "{input:?}");
    assert!(input.contains("stop the deploy"), "{input:?}");
}

#[tokio::test]
async fn an_urgent_message_never_sends_esc_into_a_blocked_suspended_typing_or_other_target() {
    // A permission dialog: Esc would answer it.
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Blocked);
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(
        matches!(delivery, MessageDelivery::Queued { reason } if reason.contains("not interrupted"))
    );
    assert!(typed(&mut rx).is_empty());

    // Suspended.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    terminal(&mut app).begin_agent_suspend(
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("s1").expect("a session id"),
            transcript_path: None,
        },
        Instant::now() + Duration::from_secs(30),
    );
    let (_, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(typed(&mut rx).is_empty());

    // Its user is typing.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    let pane_id = pane(&app);
    app.lookup_runtime_sender(0, pane_id)
        .unwrap()
        .note_user_input(Instant::now());
    let (_, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(typed(&mut rx).is_empty());

    // Its hooks say the turn already ended: no Esc, it goes in on settle.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    hook_turn(&mut app, false, "p1", 20);
    let (_, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(typed(&mut rx).is_empty());

    // An agent without an Esc interrupt.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    terminal(&mut app).set_detected_state(Some(Agent::Gemini), AgentState::Working);
    let (_, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(typed(&mut rx).is_empty());
}

#[tokio::test]
async fn an_urgent_message_never_interrupts_a_turn_herdr_just_started_or_a_fresh_dialog() {
    // A queued message was just typed in and started the turn.
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Idle);
    let first = send(&mut app, "mt1", "please read this");
    assert_eq!(first["result"]["outcome"], "sent", "{first}");
    assert!(typed(&mut rx).contains("mt1"));
    set_state(&mut app, AgentState::Working);
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(
        matches!(&delivery, MessageDelivery::Queued { reason } if reason.contains("a herdr message just started its turn")),
        "{delivery:?}"
    );
    assert!(typed(&mut rx).is_empty(), "no Esc");

    // The detector last said working, but the screen now shows a dialog.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    app.message_queue.screen_blocker_for_test = Some(true);
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert!(
        matches!(&delivery, MessageDelivery::Queued { reason } if reason.contains("blocked on its user")),
        "{delivery:?}"
    );
    assert!(typed(&mut rx).is_empty(), "no Esc into a dialog");
}

#[tokio::test]
async fn an_urgent_message_to_an_idle_target_with_queued_messages_jumps_the_queue_without_an_esc() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    let earlier = send(&mut app, "ma1", "when you have a moment");
    assert_eq!(earlier["result"]["outcome"], "queued", "{earlier}");
    terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    let (delivery, interrupted) = urgent(&mut app, "mu1", "stop").unwrap();
    assert!(!interrupted);
    assert_eq!(
        delivery,
        MessageDelivery::Queued {
            reason: "queued ahead of its other messages".into()
        }
    );
    assert!(typed(&mut rx).is_empty());
    assert_eq!(app.message_queue.entries[0].id(), "mu1");
}

#[tokio::test]
async fn an_urgent_interrupt_after_a_hook_reported_start_goes_in_once_the_screen_reads_idle() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    hook_turn(&mut app, true, "p1", 10);
    let (_, interrupted) = urgent(&mut app, "mu1", "stop the deploy").unwrap();
    assert!(interrupted);
    assert_eq!(typed(&mut rx), ESC);
    // Esc ends the turn without a Stop hook: no hook hold on the idle screen.
    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    assert!(typed(&mut rx).contains("stop the deploy"));
}

#[tokio::test]
async fn the_inbox_lists_messages_queued_for_the_caller_and_claims_nothing() {
    let mut app = app();
    let _rx = rev(&mut app, AgentState::Working);
    send(&mut app, "mi1", "first");
    let mut reply = params("mi2", "second, longer");
    reply.reply_to = Some("mq0".into());
    let response = app.handle_agent_message_send("req".into(), reply);
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["result"]["outcome"], "queued", "{response}");
    let target = app.public_pane_id(0, pane(&app)).unwrap();
    let inbox = |app: &mut App, caller: &str| -> Value {
        let response = app.handle_agents_inbox(
            "i".into(),
            crate::api::schema::agents_model::AgentsInboxParams {
                caller_pane: caller.into(),
            },
        );
        serde_json::from_str(&response).unwrap()
    };
    let out = inbox(&mut app, &target);
    let messages = out["result"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "{out}");
    assert_eq!(messages[0]["id"], "mi1");
    assert_eq!(messages[0]["from_pane"], "w9:p1");
    assert_eq!(messages[0]["from_name"], "lead");
    assert_eq!(messages[0]["chars"], 5);
    assert_eq!(messages[1]["id"], "mi2");
    assert_eq!(messages[1]["reply_to"], "mq0");
    assert_eq!(messages[1]["legacy"], false);
    // Read-only: both stay queued, nothing is logged.
    assert_eq!(app.message_queue.entries.len(), 2);
    let written = app.message_queue.written.len();
    inbox(&mut app, &target);
    assert_eq!(app.message_queue.written.len(), written);
    // Another pane's inbox does not list them; an unknown caller is refused.
    let out = inbox(&mut app, "w9:p9");
    assert!(out["error"].is_object(), "{out}");
}

#[tokio::test]
async fn urgent_messages_keep_their_sending_order_ahead_of_plain_ones() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    send(&mut app, "ma1", "plain one");
    let (_, interrupted) = urgent_from(&mut app, "mu1", "urgent one", "lead (w9:p1)").unwrap();
    assert!(interrupted);
    assert_eq!(typed(&mut rx), ESC);
    let (_, again) = urgent_from(&mut app, "mu2", "urgent two", "ops (w9:p2)").unwrap();
    assert!(!again, "no second Esc");
    send(&mut app, "ma2", "plain two");
    let order: Vec<&str> = app
        .message_queue
        .entries
        .iter()
        .map(|entry| entry.id())
        .collect();
    assert_eq!(order, ["mu1", "mu2", "ma1", "ma2"]);

    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let text = typed(&mut rx);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text:?}"))
    };
    assert!(at("urgent one") < at("urgent two"), "{text}");
    assert!(at("urgent two") < at("plain one"), "{text}");
    assert!(at("plain one") < at("plain two"), "{text}");
}

#[tokio::test]
async fn a_claimed_urgent_message_leaves_no_interrupt_behind_to_skip_the_hook_hold() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let mut rx = rev(&mut app, AgentState::Working);
    let (_, interrupted) = urgent(&mut app, "mu1", "stop the deploy").unwrap();
    assert!(interrupted);
    assert_eq!(typed(&mut rx), ESC);
    // The target reads the urgent message itself (agents_wait_for_message).
    let target = app.public_pane_id(0, pane(&app)).unwrap();
    let claimed = app.handle_agent_message_claim(
        "c".into(),
        AgentMessageClaimParams {
            id: "mu1".into(),
            pane: target,
        },
    );
    let claimed: Value = serde_json::from_str(&claimed).unwrap();
    assert_eq!(claimed["result"]["claimed"], true, "{claimed}");
    assert!(app.message_queue.is_empty());
    // It starts a new turn on its own (its hooks report it) and its screen
    // reads idle mid-turn: a plain message waits for that turn's Stop.
    hook_turn(&mut app, true, "p2", 30);
    set_state(&mut app, AgentState::Idle);
    let reply = send(&mut app, "ma1", "when you are done");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], HOOK_LIVE_REASON);
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(!app.message_queue_pass(t0 + SETTLE * 2, now_unix()));
    assert!(typed(&mut rx).is_empty(), "the hooks say the turn is live");
}

#[tokio::test]
async fn an_interrupt_is_forgotten_once_it_is_stale_or_its_message_is_gone() {
    let mut app = app();
    app.coordinator.assume_shell_ready = true;
    let _rx = rev(&mut app, AgentState::Working);
    urgent(&mut app, "mu1", "stop").unwrap();
    let terminal_id = terminal(&mut app).id.to_string();
    let at = app.message_queue.interrupted_at[&terminal_id];
    // A pass past INTERRUPT_WAIT prunes it while its message still waits.
    app.message_queue.mark_due();
    app.message_queue_pass(at + INTERRUPT_WAIT, now_unix());
    assert!(!app.message_queue.interrupted_at.contains_key(&terminal_id));
    assert_eq!(app.message_queue.entries.len(), 1, "the message stays");
    // The urgent message's expiry forgets a fresh interrupt too.
    app.message_queue
        .interrupted_at
        .insert(terminal_id.clone(), Instant::now());
    let later = app.message_queue.entries[0].expires_unix;
    assert!(app.message_queue_pass(Instant::now(), later));
    assert!(app.message_queue.is_empty());
    assert!(!app.message_queue.interrupted_at.contains_key(&terminal_id));
}
