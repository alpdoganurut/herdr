//! Verify before Enter and delivery confirmation (submit.rs), on the test
//! runtime's channel: its writes come out of a receiver, and the screen the
//! queue reads is drawn by the test.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::Value;

use super::*;
use crate::api::schema::agent_messages::AgentMessageSendParams;
use crate::api::schema::agents_model::AgentsReadMessagesParams;
use crate::app::message_queue::SUBMITTING_REASON;
use crate::config::Config;
use crate::coordinator::messages::OUTCOME_QUEUED;
use crate::detect::{Agent, AgentState};
use crate::workspace::Workspace;

/// The empty box (marker, dim suggestion) with bracketed paste on.
const CLAUDE_EMPTY: &str =
    "\x1b[?2004h\r\n─────\r\n❯ \x1b[2mTry \"something\"\x1b[0m\r\n─────\r\n  footer";

fn app() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("submit")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
    app.message_queue.delivery.enabled_for_test = true;
    app
}

fn pane(app: &App) -> crate::layout::PaneId {
    app.state.workspaces[0].tabs[0].root_pane
}

fn terminal_id(app: &App) -> crate::terminal::TerminalId {
    let pane_id = pane(app);
    app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone()
}

fn runtime(app: &App) -> &crate::terminal::TerminalRuntime {
    app.lookup_runtime_sender(0, pane(app)).unwrap()
}

/// A Claude agent `rev` in `state` with a fresh runtime showing an empty box.
fn rev(app: &mut App, state: AgentState) -> tokio::sync::mpsc::Receiver<Bytes> {
    let id = terminal_id(app);
    let terminal = app.state.terminals.get_mut(&id).unwrap();
    terminal.set_agent_name("rev".into());
    terminal.set_detected_state(Some(Agent::Claude), state);
    let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
        40, 12, 0, b"", 16,
    );
    runtime.test_process_pty_bytes(CLAUDE_EMPTY.as_bytes());
    let pane_id = pane(app);
    app.state.insert_test_runtime(pane_id, runtime);
    rx
}

/// `rev`'s herdr_agents server reads messages by id: pointer lines.
fn reader(app: &mut App) {
    let id = terminal_id(app);
    app.note_message_reader(&id, 4242);
    app.agents_model.readers_unchecked = true;
}

/// Redraw `rev`'s screen with `text` typed into its box.
fn draw_box(app: &App, text: &str) {
    let screen = format!("\x1b[2J\x1b[H\r\n─────\r\n❯ {text}\r\n─────\r\n  footer");
    runtime(app).test_process_pty_bytes(screen.as_bytes());
}

fn draw_empty_box(app: &App) {
    let screen = format!("\x1b[2J\x1b[H{CLAUDE_EMPTY}");
    runtime(app).test_process_pty_bytes(screen.as_bytes());
}

fn send(app: &mut App, id: &str, body: &str) -> Value {
    let params = AgentMessageSendParams {
        target: "rev".into(),
        id: id.into(),
        envelope: format!("[herdr+ message {id} from lead (w9:p1) 10:00 — another agent]\n{body}"),
        text: body.into(),
        unix: 0,
        from_pane: Some("w9:p1".into()),
        from_name: Some("lead".into()),
        ..AgentMessageSendParams::default()
    };
    let response = app.handle_agent_message_send("req".into(), params);
    serde_json::from_str(&response).unwrap()
}

/// Everything written so far (the test channel writes from its own thread).
fn written(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> String {
    std::thread::sleep(Duration::from_millis(150));
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

fn outcomes(app: &App) -> Vec<(String, String)> {
    app.message_queue
        .written
        .iter()
        .map(|line| (line.id.clone().unwrap_or_default(), line.outcome.clone()))
        .collect()
}

fn count(app: &App, id: &str, outcome: &str) -> usize {
    outcomes(app)
        .iter()
        .filter(|(line, done)| line == id && done == outcome)
        .count()
}

fn now_unix() -> u64 {
    crate::coordinator::now_unix()
}

fn invariants(app: &App) {
    app.message_queue
        .delivery
        .assert_invariants_for_test(&app.message_queue.entries);
}

/// A pass `after` the real now (the batch's clock is the real one).
fn pass(app: &mut App, after: Duration) -> bool {
    let changed = app.message_queue_pass(Instant::now() + after, now_unix());
    invariants(app);
    changed
}

#[tokio::test]
async fn fork_smoke_enter_goes_in_only_after_the_pointer_line_shows_in_the_box() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let reply = send(&mut app, "ma1", "hello");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(reply["result"]["reason"], SUBMITTING_REASON);
    let line = written(&mut rx);
    assert!(line.starts_with("herdr+ message ma1"), "{line:?}");
    assert!(!line.contains('\r'), "no Enter with the text: {line:?}");
    invariants(&app);
    // A second message waits behind the one in flight.
    let second = send(&mut app, "ma2", "again");
    assert_eq!(second["result"]["outcome"], "queued", "{second}");
    assert_eq!(
        second["result"]["reason"],
        "earlier messages are waiting for it"
    );

    // Before the first look, and while the box is still empty: no Enter.
    pass(&mut app, Duration::from_millis(50));
    pass(&mut app, Duration::from_millis(400));
    assert_eq!(written(&mut rx), "", "nothing until the text shows");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);

    // The box shows the line, wrapped over three rows at 40 columns.
    draw_box(&app, &line);
    assert!(pass(&mut app, Duration::from_millis(600)));
    assert_eq!(written(&mut rx), "\r", "the Enter alone");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert!(
        app.message_queue
            .delivered
            .iter()
            .any(|kept| kept.id == "ma1"),
        "the envelope is kept for agents_messages only after the Enter"
    );
    assert!(app.message_queue.delivery.awaiting_confirmation("ma1"));
    // Later passes never type it again.
    pass(&mut app, Duration::from_millis(800));
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert!(app
        .message_queue
        .entries
        .iter()
        .all(|entry| entry.id() != "ma1"));
}

#[tokio::test]
async fn a_paste_placeholder_counts_as_the_text() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let reply = send(&mut app, "ma1", "line one\nline two\nline three");
    assert_eq!(reply["result"]["reason"], SUBMITTING_REASON, "{reply}");
    let paste = written(&mut rx);
    assert!(paste.starts_with("\x1b[200~"), "{paste:?}");
    assert!(!paste.ends_with('\r'));
    draw_box(&app, "[Pasted text #1 +3 lines]");
    assert!(pass(&mut app, Duration::from_millis(200)));
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

#[tokio::test]
async fn an_empty_box_at_the_deadline_requeues_the_same_id_then_holds() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let first = written(&mut rx);
    assert!(first.contains("ma1"));

    // The box stays empty through the window: no Enter, still queued,
    // same id, one attempt.
    assert!(pass(&mut app, SUBMIT_WINDOW + Duration::from_millis(100)));
    assert_eq!(written(&mut rx), "");
    assert_eq!(app.message_queue.entries.len(), 1);
    assert_eq!(app.message_queue.entries[0].id(), "ma1");
    assert_eq!(
        app.message_queue.entries[0].attempts, 0,
        "counted when the next look finds the box still empty"
    );
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 0);

    // Typed again after a fresh SETTLE; this time it shows: delivered once.
    let later = SUBMIT_WINDOW + Duration::from_millis(100) + SETTLE;
    pass(&mut app, later);
    let again = written(&mut rx);
    assert_eq!(again, first, "the same line, the same id");
    draw_box(&app, &again);
    pass(&mut app, later + Duration::from_millis(300));
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_QUEUED), 1);

    // A message whose text never shows is held after the second attempt.
    draw_empty_box(&app);
    let t = later + Duration::from_secs(1);
    send(&mut app, "ma2", "lost twice");
    written(&mut rx);
    pass(&mut app, t + SUBMIT_WINDOW + Duration::from_secs(1));
    assert_eq!(app.message_queue.entries[0].attempts, 0);
    let t = t + SUBMIT_WINDOW + Duration::from_secs(1) + SETTLE;
    pass(&mut app, t);
    assert!(written(&mut rx).contains("ma2"), "the second attempt");
    pass(&mut app, t + SUBMIT_WINDOW + Duration::from_millis(100));
    assert_eq!(app.message_queue.entries[0].attempts, 1);
    pass(&mut app, t + SUBMIT_WINDOW + SETTLE * 3);
    assert_eq!(app.message_queue.entries[0].attempts, MAX_SUBMIT_ATTEMPTS);
    assert_eq!(written(&mut rx), "", "held: not typed a third time");
    assert_eq!(count(&app, "ma2", OUTCOME_DELIVERED), 0);
    assert_eq!(app.message_queue.entries.len(), 1, "still queued");
}

#[tokio::test]
async fn other_text_at_the_deadline_is_unconfirmed_without_an_enter() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    written(&mut rx);
    draw_box(&app, "something else entirely");
    pass(&mut app, Duration::from_millis(300));
    assert_eq!(written(&mut rx), "", "not before the deadline either");
    assert!(pass(&mut app, SUBMIT_WINDOW + Duration::from_millis(100)));
    assert_eq!(written(&mut rx), "", "no Enter");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    assert!(app.message_queue.entries.is_empty(), "no retry");
    // Never typed again.
    pass(&mut app, SUBMIT_WINDOW + SETTLE * 2);
    assert_eq!(written(&mut rx), "");
}

#[tokio::test]
async fn a_late_render_gets_the_enter_alone() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let line = written(&mut rx);
    // The box goes off screen (no marker) through the window.
    runtime(&app).test_process_pty_bytes(b"\x1b[2J\x1b[H  Do you prefer tea or coffee?");
    let deadline = SUBMIT_WINDOW + Duration::from_millis(100);
    pass(&mut app, deadline);
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert_eq!(written(&mut rx), "");
    // Then the box shows exactly the line herdr typed: Enter only.
    draw_box(&app, &line);
    assert!(pass(&mut app, deadline + LATE_RENDER_POLL));
    assert_eq!(
        written(&mut rx),
        "\r",
        "the Enter alone, not the text again"
    );
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    pass(&mut app, deadline + LATE_RENDER_POLL * 3);
    assert_eq!(written(&mut rx), "");
}

#[tokio::test]
async fn user_input_after_the_text_means_no_enter() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let line = written(&mut rx);
    draw_box(&app, &line);
    runtime(&app).note_user_input(Instant::now());
    assert!(pass(&mut app, Duration::from_millis(200)));
    assert_eq!(written(&mut rx), "", "no Enter after the user typed");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    // Nor later, though the box still shows the line.
    pass(&mut app, Duration::from_millis(200) + LATE_RENDER_POLL);
    assert_eq!(written(&mut rx), "");
}

#[tokio::test]
async fn a_queued_message_waits_for_its_text_too_and_a_claim_skips_it() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    assert_eq!(written(&mut rx), "");
    let id = terminal_id(&app);
    app.state
        .terminals
        .get_mut(&id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.message_queue.mark_due();
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    assert!(app.message_queue_pass(t0 + SETTLE, now_unix()));
    let line = written(&mut rx);
    assert!(line.contains("ma1") && !line.contains('\r'), "{line:?}");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    // In flight: a claim does not take it.
    let claim = app.handle_agent_message_claim(
        "c".into(),
        crate::api::schema::AgentMessageClaimParams {
            id: "ma1".into(),
            pane: "rev".into(),
        },
    );
    assert!(claim.contains("\"claimed\":false"), "{claim}");
    draw_box(&app, &line);
    app.message_queue_pass(t0 + SETTLE + Duration::from_millis(200), now_unix());
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

fn read(app: &mut App, ids: &[&str]) {
    let caller = app.public_pane_id(0, pane(app)).unwrap();
    let response = app.handle_agents_read_messages(
        "r".into(),
        AgentsReadMessagesParams {
            caller_pane: caller,
            ids: ids.iter().map(|id| id.to_string()).collect(),
        },
    );
    assert!(response.contains("\"found\":true"), "{response}");
}

/// Deliver `id` as a pointer line through the two writes.
fn deliver_pointer(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    id: &str,
) -> Instant {
    send(app, id, "hello");
    let line = written(rx);
    draw_box(app, &line);
    let at = Instant::now() + Duration::from_millis(200);
    assert!(app.message_queue_pass(at, now_unix()));
    assert_eq!(written(rx), "\r");
    at
}

#[tokio::test]
async fn fork_smoke_a_read_by_id_confirms_a_pointer_message_once() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let at = deliver_pointer(&mut app, &mut rx, "ma1");
    read(&mut app, &["ma1"]);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    assert!(!app.message_queue.delivery.awaiting_confirmation("ma1"));
    // A second read and the window's end add nothing.
    read(&mut app, &["ma1"]);
    app.message_queue_pass(at + CONFIRM_READ_WITHIN * 2, now_unix());
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 0);
}

#[tokio::test]
async fn no_read_within_the_window_is_unconfirmed_and_a_late_read_confirms() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let at = deliver_pointer(&mut app, &mut rx, "ma1");
    assert_eq!(
        app.next_message_queue_deadline(at),
        Some(at + CONFIRM_READ_WITHIN),
        "the window's end is a queue deadline with nothing queued"
    );
    assert!(!app.message_queue_pass(at + Duration::from_secs(30), now_unix()));
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 0);
    assert!(app.message_queue_pass(at + CONFIRM_READ_WITHIN, now_unix()));
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert_eq!(written(&mut rx), "", "never redelivered");
    assert!(app.message_queue.entries.is_empty());
    read(&mut app, &["ma1"]);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
}

#[tokio::test]
async fn a_turn_start_confirms_a_paste() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    send(&mut app, "ma1", "line one\nline two");
    written(&mut rx);
    draw_box(&app, "[Pasted text #1 +2 lines]");
    let at = Instant::now() + Duration::from_millis(200);
    app.message_queue_pass(at, now_unix());
    assert_eq!(written(&mut rx), "\r");
    assert!(app.message_queue.delivery.awaiting_confirmation("ma1"));
    // A pointer read does not confirm a paste; its turn does.
    app.note_message_read("ma1");
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 0);
    app.note_submitted_turn(terminal_id(&app).as_str(), true);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    app.note_submitted_turn(terminal_id(&app).as_str(), true);
    app.message_queue_pass(at + CONFIRM_TURN_WITHIN * 2, now_unix());
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 0);
}

#[tokio::test]
async fn a_paste_without_a_turn_is_unconfirmed_after_its_window() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    send(&mut app, "ma1", "line one\nline two");
    written(&mut rx);
    draw_box(&app, "[Pasted text #1 +2 lines]");
    let at = Instant::now() + Duration::from_millis(200);
    app.message_queue_pass(at, now_unix());
    assert_eq!(written(&mut rx), "\r");
    assert!(app.message_queue_pass(at + CONFIRM_TURN_WITHIN, now_unix()));
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    // A turn after the window confirms nothing.
    app.note_submitted_turn(terminal_id(&app).as_str(), true);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 0);
}

#[test]
fn draft_matching_ignores_wrapping_and_knows_paste_placeholders() {
    use crate::app::typing_guard::{draft_is, draft_shows};
    let line = "herdr+ message ma1 from lead (w9:p1): read it with agents_messages id=ma1";
    let wrapped = "herdr+ message ma1 from lead (w9:p1): r ead it with agents_messages id=ma1";
    assert!(draft_shows(wrapped, line, false));
    assert!(draft_is(wrapped, line, false));
    assert!(!draft_shows("herdr+ message ma1", line, false));
    assert!(!draft_shows("[Pasted text #1 +3 lines]", line, false));
    assert!(draft_shows("[Pasted text #2 +3 lines]", "a\nb\nc", true));
    assert!(draft_shows("[Pasted text #2]", "a\nb\nc", true));
    assert!(draft_shows("[Pasted Content 1520 chars]", "a\nb\nc", true));
    assert!(!draft_shows("[Pasted text #x +3 lines]", "a\nb\nc", true));
    assert!(draft_is("[Pasted text #1 +3 lines]", "a\nb\nc", true));
    assert!(!draft_is(
        "hi [Pasted text #1 +3 lines] [Pasted text #2 +1 lines]",
        "a\nb\nc",
        true
    ));
    // A long paste the box scrolled shows a long enough part of it.
    let envelope = "[herdr+ message ma1 from lead]\nthe body of a long message that scrolled";
    assert!(draft_shows(
        "the body of a long message that scrolled",
        envelope,
        true
    ));
    assert!(!draft_shows("the body", envelope, true));
}

/// A restart or a live handoff: the queue file round-trips, the runtime
/// delivery checks start empty.
fn restart(app: &mut App) {
    let saved = serde_json::to_string(&app.message_queue.entries).unwrap();
    app.message_queue.entries = serde_json::from_str(&saved).unwrap();
    app.message_queue.delivery = DeliveryWatch {
        enabled_for_test: true,
        ..DeliveryWatch::default()
    };
    app.message_queue.ready_since.clear();
    app.message_queue.mark_due();
}

/// Type `id` as a pointer line and restart before any look at the box.
fn typed_then_restarted(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    id: &str,
) -> String {
    send(app, id, "hello");
    let line = written(rx);
    assert!(line.contains(id) && !line.contains('\r'), "{line:?}");
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    restart(app);
    assert!(
        app.message_queue.entries[0].typed_unix.is_some(),
        "saved with the queue"
    );
    line
}

/// Passes through a fresh SETTLE after the restart.
fn settle(app: &mut App) {
    let t0 = Instant::now();
    app.message_queue_pass(t0, now_unix());
    app.message_queue_pass(t0 + SETTLE, now_unix());
    invariants(app);
}

#[tokio::test]
async fn after_a_restart_the_text_still_in_the_box_gets_the_enter_alone() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let line = typed_then_restarted(&mut app, &mut rx, "ma1");
    draw_box(&app, &line);
    settle(&mut app);
    assert_eq!(
        written(&mut rx),
        "\r",
        "the Enter alone, not the text again"
    );
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert!(app.message_queue.entries.is_empty());
    settle(&mut app);
    assert_eq!(written(&mut rx), "");
}

#[tokio::test]
async fn after_a_restart_a_read_by_id_means_its_user_sent_it_and_it_is_not_retyped() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    typed_then_restarted(&mut app, &mut rx, "ma1");
    // The user pressed Enter; the agent reads the message by id.
    draw_empty_box(&app);
    read(&mut app, &["ma1"]);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    settle(&mut app);
    assert_eq!(written(&mut rx), "", "never typed again");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

#[tokio::test]
async fn after_a_restart_a_turn_start_means_its_user_sent_it_and_it_is_not_retyped() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    typed_then_restarted(&mut app, &mut rx, "ma1");
    draw_empty_box(&app);
    app.note_submitted_turn(terminal_id(&app).as_str(), true);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    settle(&mut app);
    assert_eq!(written(&mut rx), "", "never typed again");
}

#[tokio::test]
async fn after_a_restart_an_empty_box_without_evidence_is_typed_again_once() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let line = typed_then_restarted(&mut app, &mut rx, "ma1");
    draw_empty_box(&app);
    settle(&mut app);
    assert_eq!(written(&mut rx), line, "typed again, once");
    assert_eq!(app.message_queue.entries[0].attempts, 1);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    // This time it shows: the Enter, one delivery.
    draw_box(&app, &line);
    pass(&mut app, SETTLE + Duration::from_millis(300));
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

#[tokio::test]
async fn after_a_restart_other_text_in_the_box_is_unconfirmed_without_an_enter() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    typed_then_restarted(&mut app, &mut rx, "ma1");
    draw_box(&app, "the user's own draft");
    // The typing guard reads a draft: the recovery looks before it.
    settle(&mut app);
    assert_eq!(written(&mut rx), "", "no Enter, no retype");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert!(app.message_queue.entries.is_empty());
}

#[tokio::test]
async fn a_read_by_id_after_the_user_typed_over_it_confirms_and_the_sender_is_not_told() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let line = written(&mut rx);
    draw_box(&app, &format!("{line} and more"));
    runtime(&app).note_user_input(Instant::now());
    pass(&mut app, Duration::from_millis(200));
    assert_eq!(written(&mut rx), "", "no Enter");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert!(app.message_queue.delivery.awaiting_confirmation("ma1"));
    // The user sent it; the agent reads it by id.
    read(&mut app, &["ma1"]);
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    pass(&mut app, CONFIRM_READ_WITHIN * 2);
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1, "no second line");
}

#[tokio::test]
async fn a_late_first_copy_after_a_retry_gets_the_enter_alone() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let line = written(&mut rx);
    let deadline = SUBMIT_WINDOW + Duration::from_millis(100);
    pass(&mut app, deadline);
    assert_eq!(written(&mut rx), "", "the box stayed empty: no Enter");
    // The first copy renders late.
    draw_box(&app, &line);
    pass(&mut app, deadline + SETTLE);
    assert_eq!(written(&mut rx), "\r", "the Enter alone, no second copy");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert!(app.message_queue.entries.is_empty());
}

/// Queue an urgent message for `rev` ahead of the others.
fn queue_urgent(app: &mut App, id: &str) {
    let target = app.message_target(terminal_id(app).as_str(), None).unwrap();
    let mut pointer = crate::agents_model::envelope::PointerFrom::from_envelope(
        id,
        &format!("[herdr+ message {id} from lead (w9:p1)]\nurgent"),
    );
    pointer.urgent = true;
    let message = AgentMessage {
        unix: now_unix(),
        id: Some(id.into()),
        to_pane: "w1:p1".into(),
        from_pane: Some("w9:p1".into()),
        ..AgentMessage::default()
    };
    let entry = app.queued_message(
        &target,
        message,
        format!("[herdr+ URGENT message {id} from lead (w9:p1)]\nurgent"),
        Some(pointer),
        false,
        None,
        false,
        now_unix(),
    );
    app.message_queue.push_at(entry, true);
}

#[tokio::test]
async fn an_urgent_message_goes_ahead_of_a_held_one_and_alongside_a_typed_one() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    // A typed batch whose box stayed empty, then an urgent one ahead of it.
    send(&mut app, "ma1", "hello");
    written(&mut rx);
    let deadline = SUBMIT_WINDOW + Duration::from_millis(100);
    pass(&mut app, deadline);
    queue_urgent(&mut app, "mu1");
    assert_eq!(app.message_queue.entries[0].id(), "mu1");
    pass(&mut app, deadline + SETTLE);
    let line = written(&mut rx);
    assert!(
        line.contains("mu1") && line.contains("ma1"),
        "typed once, together: {line:?}"
    );
    assert_eq!(line.matches("ma1").count(), 2, "one pointer naming ma1");
    // Held: the urgent one is not kept out by it.
    let mut app = self::app();
    let mut rx = rev(&mut app, AgentState::Working);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    app.message_queue.entries[0].attempts = MAX_SUBMIT_ATTEMPTS;
    queue_urgent(&mut app, "mu1");
    terminal_state(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.message_queue.mark_due();
    settle(&mut app);
    let line = written(&mut rx);
    assert!(line.contains("mu1") && !line.contains("ma1"), "{line:?}");
}

fn terminal_state(app: &mut App) -> &mut crate::terminal::TerminalState {
    let id = terminal_id(app);
    app.state.terminals.get_mut(&id).unwrap()
}

#[tokio::test]
async fn after_a_restart_no_input_box_waits_and_looks_again() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let line = typed_then_restarted(&mut app, &mut rx, "ma1");
    runtime(&app).test_process_pty_bytes(b"\x1b[2J\x1b[H  Do you prefer tea or coffee?");
    settle(&mut app);
    assert_eq!(written(&mut rx), "", "nothing typed into a dialog");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 0);
    assert!(
        app.message_queue.entries[0].typed_unix.is_some(),
        "still typed"
    );
    // The dialog closes, the text is still there: the Enter alone.
    draw_box(&app, &line);
    let t = Instant::now() + SETTLE + RETRY_FOR_TEST;
    app.message_queue_pass(t, now_unix());
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

const RETRY_FOR_TEST: Duration = crate::app::message_queue::RETRY;

#[tokio::test]
async fn after_a_restart_user_input_since_the_text_means_no_enter() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let line = typed_then_restarted(&mut app, &mut rx, "ma1");
    draw_box(&app, &line);
    runtime(&app).note_user_input(Instant::now());
    settle(&mut app);
    assert_eq!(written(&mut rx), "", "no Enter after the user typed");
    assert_eq!(count(&app, "ma1", OUTCOME_UNCONFIRMED), 1);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
}

#[tokio::test]
async fn an_agent_prompt_while_a_message_waits_for_its_enter_is_refused_busy() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    written(&mut rx);
    let refused = app
        .validate_agent_prompt(
            "x".into(),
            &crate::api::schema::AgentPromptParams {
                target: "rev".into(),
                text: "hi".into(),
                wait: None,
                guard_user_typing: false,
            },
        )
        .err()
        .unwrap_or_default();
    assert!(refused.contains("\"busy\""), "{refused}");
}

#[tokio::test]
async fn a_first_sighting_or_a_box_still_showing_the_text_does_not_adopt_it() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let line = typed_then_restarted(&mut app, &mut rx, "ma1");
    // Unknown → working after a restart: no evidence.
    draw_empty_box(&app);
    app.note_submitted_turn(terminal_id(&app).as_str(), false);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    // A real turn while the box still shows the text: not sent.
    draw_box(&app, "[Pasted text #1 +2 lines]");
    app.note_submitted_turn(terminal_id(&app).as_str(), true);
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    let _ = line;
}

// ----- verify before Enter with quiet messages and per-tab DND -----

fn set_dnd(app: &mut App, dnd: bool) {
    let tab_id = app.public_tab_id(0, 0).unwrap();
    let reply = app.handle_api_request(crate::api::schema::Request {
        id: "dnd".into(),
        method: crate::api::schema::Method::TabSetDnd(crate::api::schema::TabSetDndParams {
            tab_id,
            dnd,
        }),
    });
    assert!(reply.contains("\"result\""), "{reply}");
}

fn quiet_ids(app: &App) -> Vec<String> {
    app.quiet
        .entries
        .iter()
        .map(|entry| entry.id().to_string())
        .collect()
}

/// Passes `SETTLE` apart from `from` until something is written.
fn pass_until_written(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::Receiver<Bytes>,
    from: Duration,
) -> (String, Duration) {
    let mut at = from;
    for _ in 0..4 {
        app.message_queue.mark_due();
        pass(app, at);
        let out = written(rx);
        if !out.is_empty() {
            return (out, at);
        }
        at += SETTLE;
    }
    (String::new(), at)
}

/// The box showing what herdr typed: a paste's placeholder, else the line.
fn draw_typed(app: &App, typed: &str) {
    if typed.starts_with("\x1b[200~") {
        draw_box(app, "[Pasted text #1 +2 lines]");
    } else {
        draw_box(app, typed);
    }
}

/// A message to a DND tab is stored quietly and the submit path never
/// types it; leaving DND releases it (a target that takes no quiet
/// messages) into the queue, where it goes through verify-before-Enter
/// like any queued message: text first, Enter only once the box shows it.
#[tokio::test]
async fn a_dnd_message_is_never_typed_and_its_release_is_verified_before_enter() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    app.quiet.claude_hook_for_test = Some(false);
    set_dnd(&mut app, true);
    let reply = send(&mut app, "ma1", "hello");
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    assert_eq!(
        reply["result"]["reason"],
        crate::app::quiet_messages::DND_REASON
    );
    assert_eq!(quiet_ids(&app), vec!["ma1".to_string()]);
    assert!(app.message_queue.entries.is_empty());
    for step in 0..4 {
        app.message_queue.mark_due();
        pass(&mut app, SETTLE * step);
    }
    assert_eq!(written(&mut rx), "", "a stored message is never typed");
    assert!(!app
        .message_queue
        .delivery
        .submitting(terminal_id(&app).to_string().as_str()));

    set_dnd(&mut app, false);
    assert!(app.quiet.is_empty(), "released to the typing queue");
    assert_eq!(app.message_queue.entries.len(), 1);
    assert_eq!(app.message_queue.entries[0].attempts, 0);
    assert!(app.message_queue.entries[0].typed_unix.is_none());
    let (typed, at) = pass_until_written(&mut app, &mut rx, SETTLE * 4);
    assert!(typed.contains("hello"), "{typed:?}");
    assert!(!typed.ends_with('\r'), "no Enter with the text: {typed:?}");
    assert!(app
        .message_queue
        .delivery
        .submitting(terminal_id(&app).to_string().as_str()));
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 0);

    draw_typed(&app, &typed);
    assert!(pass(&mut app, at + Duration::from_millis(300)));
    assert_eq!(written(&mut rx), "\r", "the Enter alone");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    pass(&mut app, at + SETTLE * 2);
    assert_eq!(written(&mut rx), "", "typed once");
}

/// DND turning on while a message's text is in and its Enter waits does
/// not move it into the quiet store: it finishes its own delivery (the
/// Enter goes in once the box shows it) and is typed in once. Messages
/// sent after that are stored quietly.
#[tokio::test]
async fn dnd_on_mid_submit_lets_the_batch_in_flight_finish_once() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let line = written(&mut rx);
    assert!(line.contains("ma1"), "{line:?}");
    let terminal = terminal_id(&app).to_string();
    assert!(app.message_queue.delivery.submitting(&terminal));

    set_dnd(&mut app, true);
    assert!(app.quiet.is_empty(), "the batch in flight stays queued");
    assert!(app.message_queue.delivery.submitting(&terminal));
    let later = send(&mut app, "ma2", "after dnd");
    assert_eq!(
        later["result"]["reason"],
        crate::app::quiet_messages::DND_REASON,
        "{later}"
    );
    assert_eq!(quiet_ids(&app), vec!["ma2".to_string()]);

    draw_box(&app, &line);
    assert!(pass(&mut app, Duration::from_millis(600)));
    assert_eq!(written(&mut rx), "\r", "its Enter goes in");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(
        count(&app, "ma1", crate::coordinator::messages::OUTCOME_QUIET),
        0
    );
    for step in 1..4 {
        app.message_queue.mark_due();
        pass(&mut app, SETTLE * step);
    }
    assert_eq!(written(&mut rx), "", "nothing else typed into the DND tab");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(quiet_ids(&app), vec!["ma2".to_string()]);
}

/// A batch typed with no Enter known (`typed_unix`, the box was empty at
/// the deadline) when DND turns on stays queued, is not retyped while the
/// tab is in DND (the DND wait comes before the recovery look), and its
/// recovery runs once DND is off: one delivery, never quiet.
#[tokio::test]
async fn dnd_on_after_an_empty_box_holds_the_recovery_until_dnd_is_off() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let first = written(&mut rx);
    let window = SUBMIT_WINDOW + Duration::from_millis(100);
    assert!(pass(&mut app, window));
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    let terminal = terminal_id(&app).to_string();
    assert!(!app.message_queue.delivery.submitting(&terminal));

    set_dnd(&mut app, true);
    assert!(app.quiet.is_empty(), "a typed batch is never moved");
    assert_eq!(app.message_queue.entries[0].id(), "ma1");
    for step in 1..5 {
        app.message_queue.mark_due();
        pass(&mut app, window + SETTLE * step);
    }
    assert_eq!(written(&mut rx), "", "no retype while in DND");
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    assert_eq!(app.message_queue.entries[0].attempts, 0);

    set_dnd(&mut app, false);
    let (again, at) = pass_until_written(&mut app, &mut rx, window + SETTLE * 5);
    assert_eq!(again, first, "the recovery types the same line");
    draw_box(&app, &again);
    pass(&mut app, at + Duration::from_millis(300));
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
    assert_eq!(
        count(&app, "ma1", crate::coordinator::messages::OUTCOME_QUIET),
        0
    );
}

/// A coordinator message typed with no Enter known (`typed_unix`) under a
/// do-not-disturb an agent turned on is not held: its recovery still runs
/// and the Enter goes in.
#[tokio::test]
async fn a_typed_coordinator_message_recovers_through_an_agents_dnd() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    let first = written(&mut rx);
    let window = SUBMIT_WINDOW + Duration::from_millis(100);
    assert!(pass(&mut app, window));
    assert!(app.message_queue.entries[0].typed_unix.is_some());
    app.message_queue.entries[0].from_coordinator = true;

    assert!(app.set_tab_dnd(0, 0, true, true));
    assert!(app.quiet.is_empty(), "a typed batch is never moved");
    let (again, at) = pass_until_written(&mut app, &mut rx, window + SETTLE);
    assert_eq!(again, first, "the recovery types the same line");
    draw_box(&app, &again);
    pass(&mut app, at + Duration::from_millis(300));
    assert_eq!(written(&mut rx), "\r");
    assert_eq!(count(&app, "ma1", OUTCOME_DELIVERED), 1);
}

/// Every hold is a wait: the batch in flight, DND and the OSC 7501
/// blocked report, alone or together, never read Ready.
#[tokio::test]
async fn submitting_dnd_and_the_osc_veto_are_all_waits() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let terminal = terminal_id(&app).to_string();
    let target = app.message_target(&terminal, None).unwrap();
    let reason = |app: &App| match app.message_check_with(&target, true) {
        crate::app::message_queue::Check::Wait(reason) => Some(reason),
        _ => None,
    };
    assert_eq!(reason(&app), None, "free");

    app.state.workspaces[0].tabs[0].dnd = true;
    assert_eq!(
        reason(&app).as_deref(),
        Some(crate::app::tab_dnd::DND_WAIT_REASON)
    );
    runtime(&app).test_process_pty_bytes(b"\x1b]7501;state=blocked:kind=question\x07");
    assert!(reason(&app).is_some(), "DND and blocked");
    app.state.workspaces[0].tabs[0].dnd = false;
    assert!(reason(&app).is_some(), "blocked alone");
    runtime(&app).test_process_pty_bytes(b"\x1b]7501;state=idle\x07");
    assert_eq!(reason(&app), None);

    send(&mut app, "ma1", "hello");
    written(&mut rx);
    assert_eq!(reason(&app).as_deref(), Some(SUBMITTING_REASON));
    app.state.workspaces[0].tabs[0].dnd = true;
    assert!(reason(&app).is_some(), "submitting and DND");
    runtime(&app).test_process_pty_bytes(b"\x1b]7501;state=blocked:kind=question\x07");
    assert!(reason(&app).is_some(), "all three");
    app.state.workspaces[0].tabs[0].dnd = false;
    assert!(reason(&app).is_some(), "submitting and blocked");
}

/// A message in flight cannot be claimed; a quiet one in the same target's
/// inbox can.
#[tokio::test]
async fn a_claim_refuses_the_batch_in_flight_and_takes_a_quiet_message() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    send(&mut app, "ma1", "hello");
    written(&mut rx);
    set_dnd(&mut app, true);
    send(&mut app, "ma2", "fyi");
    assert_eq!(quiet_ids(&app), vec!["ma2".to_string()]);
    let me = app.public_pane_id(0, pane(&app)).unwrap();

    let inbox = app.handle_agents_inbox(
        "req".into(),
        crate::api::schema::agents_model::AgentsInboxParams {
            caller_pane: me.clone(),
        },
    );
    assert!(inbox.contains("\"ma2\""), "{inbox}");

    let claim = |app: &mut App, id: &str| {
        app.handle_agent_message_claim(
            "req".into(),
            crate::api::schema::agent_messages::AgentMessageClaimParams {
                id: id.into(),
                pane: me.clone(),
            },
        )
    };
    let in_flight = claim(&mut app, "ma1");
    assert!(in_flight.contains("\"claimed\":false"), "{in_flight}");
    assert_eq!(app.message_queue.entries[0].id(), "ma1");
    let quiet = claim(&mut app, "ma2");
    assert!(quiet.contains("\"claimed\":true"), "{quiet}");
    assert!(app.quiet.is_empty());
}
