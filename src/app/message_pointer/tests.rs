use super::*;
use crate::agents_model::envelope::PointerRelation;
use crate::api::schema::agent_messages::AgentMessageSendParams;
use crate::api::schema::agents_model::AgentsActorParams;
use crate::api::schema::{AgentPromptParams, Method, Request};
use crate::config::Config;
use crate::detect::{Agent, AgentState};
use crate::workspace::Workspace;
use bytes::Bytes;
use serde_json::Value;
use std::time::{Duration, Instant};

/// A Claude prompt with bracketed paste on, so a paste is visible as one.
const CLAUDE_EMPTY: &[u8] =
    "\x1b[?2004h\r\n─────\r\n❯ \x1b[2mTry \"something\"\x1b[0m\r\n─────\r\n  footer".as_bytes();
const PASTE_START: &str = "\x1b[200~";

fn app() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("pointers")];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    app.state.selected = 0;
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

fn public(app: &App) -> String {
    app.public_pane_id(0, pane(app)).unwrap()
}

/// A Claude agent named `rev`, with a fresh runtime whose writes come out
/// of the returned receiver.
fn rev(app: &mut App, state: AgentState) -> tokio::sync::mpsc::Receiver<Bytes> {
    let id = terminal_id(app);
    let terminal = app.state.terminals.get_mut(&id).unwrap();
    terminal.set_agent_name("rev".into());
    terminal.set_detected_state(Some(Agent::Claude), state);
    let (runtime, rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(40, 8, 0, b"", 4);
    runtime.test_process_pty_bytes(CLAUDE_EMPTY);
    let pane_id = pane(app);
    app.state.insert_test_runtime(pane_id, runtime);
    rx
}

/// `rev`'s herdr_agents server reads messages by id.
fn reader(app: &mut App) {
    let id = terminal_id(app);
    app.note_message_reader(&id, 4242);
    app.agents_model.readers_unchecked = true;
}

fn set_state(app: &mut App, state: AgentState) {
    let id = terminal_id(app);
    app.state
        .terminals
        .get_mut(&id)
        .unwrap()
        .set_detected_state(Some(Agent::Claude), state);
    app.message_queue.mark_due();
}

fn envelope(id: &str, body: &str) -> String {
    format!("[herdr+ message {id} from lead (w9:p1, claude, teammate) 10:00 \u{2014} your teammate, not your user]\n{body}\n[answer with agents_send_message to=\"w9:p1\" reply_to=\"{id}\"]")
}

fn send(app: &mut App, id: &str, body: &str) -> Value {
    let params = AgentMessageSendParams {
        target: "rev".into(),
        id: id.into(),
        envelope: envelope(id, body),
        text: body.into(),
        unix: 0,
        from_pane: Some("w9:p1".into()),
        from_name: Some("lead".into()),
        team: Some("w9".into()),
        ..AgentMessageSendParams::default()
    };
    let response = app.handle_agent_message_send("req".into(), params);
    serde_json::from_str(&response).unwrap()
}

fn typed(rx: &mut tokio::sync::mpsc::Receiver<Bytes>) -> String {
    std::thread::sleep(Duration::from_millis(400));
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

fn read(app: &mut App, caller: &str, ids: &[&str]) -> Value {
    let response = app.handle_agents_read_messages(
        "r".into(),
        AgentsReadMessagesParams {
            caller_pane: caller.into(),
            ids: ids.iter().map(|id| id.to_string()).collect(),
        },
    );
    serde_json::from_str(&response).unwrap()
}

#[tokio::test]
async fn a_target_without_a_reading_server_gets_the_paste() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    let reply = send(&mut app, "mp1", "hello");
    assert_eq!(reply["result"]["outcome"], "sent", "{reply}");
    let text = typed(&mut rx);
    assert!(text.starts_with(PASTE_START), "{text:?}");
    assert!(text.contains("[herdr+ message mp1 from lead"), "{text:?}");
    assert!(!text.contains("agents_messages id="));
    assert!(
        app.message_queue.delivered.is_empty(),
        "a paste keeps nothing"
    );
}

#[tokio::test]
async fn a_reading_target_gets_one_typed_line_and_reads_the_envelope_once() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let reply = send(&mut app, "mp1", "run the tests");
    assert_eq!(reply["result"]["outcome"], "sent", "{reply}");
    let text = typed(&mut rx);
    assert!(!text.contains(PASTE_START), "plain keys: {text:?}");
    assert_eq!(
        text.trim_end_matches(['\r', '\n']),
        "herdr+ message mp1 from lead (teammate, w9:p1): read it with agents_messages id=mp1"
    );
    assert!(
        !text.contains("run the tests"),
        "the body is read, not typed"
    );

    let me = public(&app);
    let out = read(&mut app, &me, &["mp1", "mp9"]);
    let messages = &out["result"]["messages"];
    assert_eq!(messages[0]["found"], true, "{out}");
    assert_eq!(messages[0]["text"], envelope("mp1", "run the tests"));
    assert_eq!(messages[0]["first_read"], true);
    assert_eq!(messages[1]["found"], false, "an unknown id");
    let again = read(&mut app, &me, &["mp1"]);
    assert_eq!(again["result"]["messages"][0]["first_read"], false);

    // Only the agent it was typed into reads it.
    app.message_queue.delivered[0].terminal_id = "term_other".into();
    let other = read(&mut app, &me, &["mp1"]);
    assert_eq!(other["result"]["messages"][0]["found"], false);

    let bad = read(&mut app, &me, &["not an id"]);
    assert_eq!(bad["error"]["code"], "invalid_params", "{bad}");
}

#[tokio::test]
async fn queued_messages_for_a_reader_go_in_as_one_pointer_line() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Working);
    reader(&mut app);
    for (id, body) in [("mp1", "first"), ("mp2", "second"), ("mp3", "third")] {
        assert_eq!(send(&mut app, id, body)["result"]["outcome"], "queued");
    }
    set_state(&mut app, AgentState::Idle);
    let t0 = Instant::now();
    app.message_queue_pass(t0, crate::coordinator::now_unix());
    assert!(app.message_queue_pass(
        t0 + crate::app::message_queue::SETTLE,
        crate::coordinator::now_unix()
    ));
    let text = typed(&mut rx);
    assert_eq!(
        text.trim_end_matches(['\r', '\n']),
        "herdr+ 3 messages: mp1 from lead (teammate, w9:p1); mp2 from lead (teammate, w9:p1); mp3 from lead (teammate, w9:p1). Read them with agents_messages id=mp1,mp2,mp3"
    );
    assert!(app.message_queue.is_empty());
    let kept: Vec<&str> = app
        .message_queue
        .delivered
        .iter()
        .map(|message| message.id.as_str())
        .collect();
    assert_eq!(kept, ["mp1", "mp2", "mp3"]);
}

#[tokio::test]
async fn an_older_servers_prompt_to_a_reader_goes_in_as_a_pointer() {
    let mut app = app();
    let mut rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let (respond_to, response_rx) = std::sync::mpsc::channel();
    assert!(app.handle_deferred_agent_api_request(
        Request {
            id: "coordinator:agent.prompt".into(),
            method: Method::AgentPrompt(AgentPromptParams {
                target: "rev".into(),
                text: "[herdr+ message mold1 from lead (w9:p1) 10:00 \u{2014} another agent, not your user]\nold hello".into(),
                wait: None,
                guard_user_typing: true,
            }),
        },
        respond_to,
    ));
    let reply: Value =
        serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(reply["result"]["type"], "agent_prompted", "{reply}");
    let text = typed(&mut rx);
    assert_eq!(
        text.trim_end_matches(['\r', '\n']),
        "herdr+ message mold1 from lead (another agent, w9:p1): read it with agents_messages id=mold1"
    );
    assert!(app.message_queue.delivered[0]
        .envelope
        .ends_with("old hello"));
}

#[test]
fn only_a_running_announced_server_makes_a_reader() {
    let mut app = app();
    let id = terminal_id(&app);
    assert!(!app.reads_messages(id.as_str()), "nothing announced");
    // Announced, but no process runs under the pane (the test runtime has
    // none): the paste stays.
    app.note_message_reader(&id, 4242);
    assert!(!app.reads_messages(id.as_str()));
    app.agents_model.readers_unchecked = true;
    assert!(app.reads_messages(id.as_str()));
    // At most four processes per terminal, newest kept.
    for pid in 1..=6 {
        app.note_message_reader(&id, pid);
    }
    assert_eq!(app.agents_model.message_readers[id.as_str()], [3, 4, 5, 6]);
}

#[test]
fn an_announcing_actor_call_records_its_process() {
    let mut app = app();
    let id = terminal_id(&app);
    let me = public(&app);
    let peer = std::process::id();
    app.agents_model.caller_process = Some(peer);
    let call = |app: &mut App, reads_messages: bool| {
        app.handle_agents_actor(
            "a".into(),
            AgentsActorParams {
                caller_pane: me.clone(),
                reads_messages,
                ..AgentsActorParams::default()
            },
        )
    };
    call(&mut app, false);
    assert!(!app.agents_model.message_readers.contains_key(id.as_str()));
    call(&mut app, true);
    assert_eq!(app.agents_model.message_readers[id.as_str()], [peer]);
    // Over the API method's JSON too.
    let request: Request = serde_json::from_value(serde_json::json!({
        "id": "x",
        "method": "agents.actor",
        "params": { "caller_pane": me, "reads_messages": true },
    }))
    .unwrap();
    assert!(matches!(request.method, Method::AgentsActor(ref p) if p.reads_messages));
}

#[test]
fn delivered_envelopes_are_bounded_by_age_and_count() {
    let message = |id: usize, unix: u64| DeliveredMessage {
        id: format!("m{id}"),
        terminal_id: "t".into(),
        session: None,
        envelope: "e".into(),
        unix,
        read_unix: None,
    };
    let now = 10 * DELIVERED_KEEP_S;
    let mut delivered = vec![message(0, now - DELIVERED_KEEP_S), message(1, now - 5)];
    prune_delivered(&mut delivered, now);
    assert_eq!(delivered.len(), 1, "a day old is dropped");
    let mut delivered: Vec<_> = (0..DELIVERED_MAX + 3).map(|i| message(i, now)).collect();
    prune_delivered(&mut delivered, now);
    assert_eq!(delivered.len(), DELIVERED_MAX);
    assert_eq!(delivered[0].id, "m3", "oldest go first");
}

#[tokio::test]
async fn kept_envelopes_survive_a_restart_with_the_queue_file() {
    let mut app = app();
    let _rx = rev(&mut app, AgentState::Idle);
    reader(&mut app);
    let dir = std::env::temp_dir().join(format!("herdr-message-pointer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("message_queue.json");
    app.message_queue.store = Some(path.clone());
    send(&mut app, "mp1", "survives");
    let saved = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("survives"))
    });
    assert!(saved, "the envelope is saved with the queue");
    let mut restarted = crate::app::message_queue::MessageQueue::default();
    restarted.load(path);
    assert_eq!(restarted.delivered.len(), 1);
    assert_eq!(restarted.delivered[0].envelope, envelope("mp1", "survives"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pointer_relation_reads_back_from_an_unknown_value() {
    let pointer: PointerFrom = serde_json::from_value(serde_json::json!({
        "id": "m1", "name": "x", "pane": "w1:p1", "relation": "something_new"
    }))
    .unwrap();
    assert_eq!(pointer.relation, PointerRelation::Agent);
}
