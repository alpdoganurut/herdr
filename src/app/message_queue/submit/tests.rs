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
    assert_eq!(app.message_queue.entries[0].attempts, 1);
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
    assert_eq!(app.message_queue.entries[0].attempts, 1);
    let t = t + SUBMIT_WINDOW + Duration::from_secs(1) + SETTLE;
    pass(&mut app, t);
    assert!(written(&mut rx).contains("ma2"), "the second attempt");
    pass(&mut app, t + SUBMIT_WINDOW + Duration::from_millis(100));
    assert_eq!(app.message_queue.entries[0].attempts, MAX_SUBMIT_ATTEMPTS);
    pass(&mut app, t + SUBMIT_WINDOW + SETTLE * 3);
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
    app.note_submitted_turn(terminal_id(&app).as_str());
    assert_eq!(count(&app, "ma1", OUTCOME_CONFIRMED), 1);
    app.note_submitted_turn(terminal_id(&app).as_str());
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
    app.note_submitted_turn(terminal_id(&app).as_str());
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
