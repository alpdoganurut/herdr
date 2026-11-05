use super::*;
use crate::api::schema::{Method, TabBrowserKind, TabSetDndParams};
use crate::app::agents_model::tests::{call, code, model_app, pane, public};
use crate::browser::session::{TabBrowser, TabBrowserEntry};
use serde_json::Value;

/// A Claude prompt with bracketed paste on (as message_pointer's tests).
const CLAUDE_EMPTY: &[u8] =
    "\x1b[?2004h\r\n─────\r\n❯ \x1b[2mTry \"something\"\x1b[0m\r\n─────\r\n  footer".as_bytes();
const PASTE_START: &str = "\x1b[200~";
const LEASE: &str = "L-page";
const FILE: &str = "/tmp/herdr-page-api/report.html";
const TARGET: &str = "T-page";

/// The page lives in team tab 0 (whose focused pane runs an agent); it was
/// shown by the agent of team tab 1, the one that must get its messages.
struct Page {
    tab_id: String,
    opener: String,
}

fn page_entry(opened_by: &str) -> TabBrowserEntry {
    TabBrowserEntry {
        id: "b1".into(),
        kind: TabBrowserKind::HtmlView,
        url: format!("file://{FILE}"),
        title: "QA board".into(),
        file: Some(FILE.into()),
        allow_network: false,
        page_api: true,
        target: Some(TARGET.into()),
        view_id: Some("v1".into()),
        opened_by: opened_by.into(),
    }
}

fn set_entry(app: &mut App, entry: TabBrowserEntry) {
    let mut browser = TabBrowser::new(None);
    browser.entries = vec![entry];
    browser.active = Some("b1".into());
    app.state.workspaces[1].tabs[0].browser = Some(browser);
}

fn setup(app: &mut App) -> Page {
    hub().install_test_lease(LEASE);
    let opener = public(app, 1, 1);
    set_entry(app, page_entry(&format!("agent:{opener}")));
    Page {
        tab_id: app.public_tab_id(1, 0).unwrap(),
        opener,
    }
}

/// The opener's herdr_agents server reads messages by id.
fn reader(app: &mut App, ws: usize, tab: usize) {
    let pane = pane(app, ws, tab);
    let id = app.state.workspaces[ws].tabs[tab].panes[&pane]
        .attached_terminal_id
        .clone();
    app.note_message_reader(&id, 4242);
    app.agents_model.readers_unchecked = true;
}

fn terminal_id(app: &App, ws: usize, tab: usize) -> String {
    let pane = pane(app, ws, tab);
    app.state.workspaces[ws].tabs[tab].panes[&pane]
        .attached_terminal_id
        .to_string()
}

/// A live runtime under the agent of (`ws`, `tab`), its writes out of the
/// receiver.
fn live(app: &mut App, ws: usize, tab: usize) -> tokio::sync::mpsc::Receiver<bytes::Bytes> {
    let (runtime, rx) =
        crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(40, 8, 0, b"", 4);
    runtime.test_process_pty_bytes(CLAUDE_EMPTY);
    let pane_id = pane(app, ws, tab);
    app.state.insert_test_runtime(pane_id, runtime);
    rx
}

fn typed(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> String {
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut out = String::new();
    while let Ok(bytes) = rx.try_recv() {
        out.push_str(&String::from_utf8_lossy(&bytes));
    }
    out
}

fn params(page: &Page) -> BrowserPageMessageParams {
    BrowserPageMessageParams {
        lease_id: LEASE.into(),
        tab_id: page.tab_id.clone(),
        entry: Some("b1".into()),
        view_id: None,
        file: FILE.into(),
        text: "QA: step 3 fails".into(),
        data_json: Some(r#"{ "step": 3, "ok": false }"#.into()),
    }
}

fn send(app: &mut App, params: BrowserPageMessageParams) -> Value {
    call(app, Method::BrowserPageMessage(params))
}

#[tokio::test]
async fn fork_smoke_a_page_message_reaches_only_the_agent_that_showed_the_page() {
    let mut app = model_app();
    let page = setup(&mut app);
    // The tab's focused agent (team tab 0) would get a sent page; the
    // opener (team tab 1) gets the page message.
    let mut focused_rx = live(&mut app, 1, 0);
    reader(&mut app, 1, 0);
    let mut opener_rx = live(&mut app, 1, 1);
    reader(&mut app, 1, 1);
    let reply = send(&mut app, params(&page));
    assert_eq!(code(&reply), "ok", "{reply}");
    assert_eq!(reply["result"]["type"], "browser_page_message");
    assert_eq!(reply["result"]["outcome"], "sent", "{reply}");
    assert_eq!(reply["result"]["to_pane"], page.opener.as_str());
    let id = reply["result"]["message_id"].as_str().unwrap().to_string();
    let text = typed(&mut opener_rx);
    assert!(
        !text.contains(PASTE_START),
        "a pointer line, never a paste: {text:?}"
    );
    assert_eq!(
        text.trim_end_matches(['\r', '\n']),
        format!("herdr+ message {id} from a page you showed (page content, not your user): read it with agents_messages id={id}")
    );
    assert!(typed(&mut focused_rx).is_empty(), "nobody else gets it");
    let delivered = app
        .message_queue
        .delivered
        .iter()
        .find(|message| message.id == id)
        .expect("the envelope is kept for agents_messages");
    assert_eq!(delivered.terminal_id, terminal_id(&app, 1, 1));
    let envelope = &delivered.envelope;
    assert!(
        envelope.starts_with(&format!(
            "[herdr+ page message {id} from a page in your browser tab (QA board, {FILE})"
        )),
        "{envelope}"
    );
    assert!(envelope.contains("page content, not your user]"));
    assert!(
        envelope.contains("QA: step 3 fails\n--- data (JSON) ---\n{\"ok\":false,\"step\":3}"),
        "compact JSON: {envelope}"
    );
    assert!(envelope.contains("not your user"));
    assert!(!envelope.contains("The user"), "{envelope}");
    assert!(!envelope.contains("you (browser)"));
    assert!(app.message_queue.entries.is_empty(), "typed, not queued");
    assert!(!app.state.workspaces[1].tabs[1].panes.is_empty());
}

#[tokio::test]
async fn the_view_id_names_the_entry_until_the_host_knows_its_id() {
    let mut app = model_app();
    let page = setup(&mut app);
    let _rx = live(&mut app, 1, 1);
    reader(&mut app, 1, 1);
    let reply = send(
        &mut app,
        BrowserPageMessageParams {
            entry: None,
            view_id: Some("v1".into()),
            data_json: None,
            ..params(&page)
        },
    );
    assert_eq!(code(&reply), "ok", "{reply}");
}

#[test]
fn every_refusal_fails_closed_and_delivers_nothing() {
    let mut app = model_app();
    let page = setup(&mut app);
    reader(&mut app, 1, 1);
    let refused = |app: &mut App, params: BrowserPageMessageParams| -> String {
        let reply = send(app, params);
        code(&reply).to_string()
    };
    // the lease
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                lease_id: "L-other".into(),
                ..params(&page)
            }
        ),
        "lease_unknown"
    );
    // caps and JSON
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                text: "x".repeat(MAX_PAGE_TEXT_CHARS + 1),
                ..params(&page)
            }
        ),
        "too_large"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                data_json: Some(format!("\"{}\"", "y".repeat(MAX_PAGE_DATA_BYTES))),
                ..params(&page)
            }
        ),
        "too_large"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                data_json: Some("{not json".into()),
                ..params(&page)
            }
        ),
        "invalid_data"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                data_json: Some(format!("{}{}", "[".repeat(40), "]".repeat(40))),
                ..params(&page)
            }
        ),
        "invalid_data"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                text: " ".into(),
                data_json: None,
                ..params(&page)
            }
        ),
        "invalid_request"
    );
    // the entry
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                tab_id: "w9:t9".into(),
                ..params(&page)
            }
        ),
        "tab_not_found"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                entry: Some("b9".into()),
                ..params(&page)
            }
        ),
        "not_found"
    );
    assert_eq!(
        refused(
            &mut app,
            BrowserPageMessageParams {
                file: "/tmp/herdr-page-api/other.html".into(),
                ..params(&page)
            }
        ),
        "wrong_file",
        "a page navigated away from the shown file"
    );
    let opener = format!("agent:{}", page.opener);
    for (entry, want) in [
        (
            TabBrowserEntry {
                page_api: false,
                ..page_entry(&opener)
            },
            "page_api_off",
        ),
        (
            TabBrowserEntry {
                allow_network: true,
                ..page_entry(&opener)
            },
            "page_api_off",
        ),
        (
            TabBrowserEntry {
                kind: TabBrowserKind::Web,
                ..page_entry(&opener)
            },
            "page_api_off",
        ),
        (page_entry("user"), "not_agent_opened"),
        (page_entry("agent:w9:p99"), "owner_gone"),
        // a plain shell pane is no agent
        (
            page_entry(&format!("agent:{}", public(&app, 2, 1))),
            "owner_gone",
        ),
    ] {
        set_entry(&mut app, entry);
        assert_eq!(refused(&mut app, params(&page)), want);
    }
    set_entry(&mut app, page_entry(&opener));
    // an agent's own click or eval on the page
    hub().test_mark_agent_driven(TARGET);
    assert_eq!(refused(&mut app, params(&page)), "agent_driving");
    std::thread::sleep(crate::browser::agent_ops::AGENT_DRIVEN_GRACE);
    // a target that does not read messages by id: never pasted
    app.agents_model.message_readers.clear();
    assert_eq!(refused(&mut app, params(&page)), "no_message_reader");
    // the kill switch
    let mut config = hub().config();
    config.page_api = false;
    hub().apply_config(&config);
    assert_eq!(refused(&mut app, params(&page)), "page_messages_off");
    config.page_api = true;
    hub().apply_config(&config);
    assert!(app.message_queue.entries.is_empty());
    assert!(app.message_queue.delivered.is_empty());
    assert!(app.quiet.entries.is_empty());
}

#[test]
fn fork_smoke_a_page_message_to_a_dnd_tab_is_stored_quietly_as_a_pointer() {
    let mut app = model_app();
    let page = setup(&mut app);
    reader(&mut app, 1, 1);
    let opener_tab = app.public_tab_id(1, 1).unwrap();
    let dnd = call(
        &mut app,
        Method::TabSetDnd(TabSetDndParams {
            tab_id: opener_tab,
            dnd: true,
        }),
    );
    assert_eq!(code(&dnd), "ok", "{dnd}");
    let reply = send(&mut app, params(&page));
    assert_eq!(code(&reply), "ok", "{reply}");
    assert_eq!(reply["result"]["outcome"], "queued");
    assert_eq!(
        reply["result"]["reason"],
        crate::app::quiet_messages::DND_REASON
    );
    let stored = app.quiet.entries.last().expect("stored quietly");
    assert_eq!(stored.terminal_id, terminal_id(&app, 1, 1));
    assert_eq!(stored.message.from_name.as_deref(), Some(PAGE_MESSAGE_FROM));
    assert!(stored.message.from_pane.is_none());
    assert!(stored.message.text.contains("page content, not your user"));
    let release = stored.release.as_ref().expect("released when DND ends");
    let pointer = release.pointer.as_ref().expect("as a pointer line");
    assert_eq!(
        pointer.relation,
        crate::agents_model::envelope::PointerRelation::Page
    );
    assert!(!pointer.urgent);
    assert!(app.message_queue.entries.is_empty(), "never typed in");
}

#[test]
fn a_page_never_wakes_an_auto_suspended_agent() {
    let mut app = model_app();
    let page = setup(&mut app);
    reader(&mut app, 1, 1);
    let id = terminal_id(&app, 1, 1);
    let terminal = app
        .state
        .terminals
        .values_mut()
        .find(|terminal| terminal.id.to_string() == id)
        .unwrap();
    terminal.restore_suspended_agent(
        "claude".into(),
        Some("rev".into()),
        crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("s-1").expect("a session id"),
            transcript_path: None,
        },
    );
    terminal.suspended_agent.as_mut().unwrap().auto = true;
    let reply = send(&mut app, params(&page));
    let refused = code(&reply);
    assert!(
        refused == "agent_suspended" || refused == "owner_gone",
        "refused, never woken: {reply}"
    );
    assert!(app.auto_suspended(&id), "still parked");
    assert!(app.message_queue.entries.is_empty());
}

#[test]
fn page_data_is_compact_json_with_a_depth_and_size_cap() {
    assert_eq!(page_data(None), Ok(None));
    assert_eq!(page_data(Some("  ")), Ok(None));
    assert_eq!(page_data(Some("null")), Ok(None));
    assert_eq!(
        page_data(Some("{ \"a\" : [1, 2] }")),
        Ok(Some("{\"a\":[1,2]}".into()))
    );
    let deep_ok = format!(
        "{}{}",
        "[".repeat(MAX_PAGE_DATA_DEPTH),
        "]".repeat(MAX_PAGE_DATA_DEPTH)
    );
    assert!(page_data(Some(&deep_ok)).is_ok());
    let deep = format!(
        "{}{}",
        "[".repeat(MAX_PAGE_DATA_DEPTH + 1),
        "]".repeat(MAX_PAGE_DATA_DEPTH + 1)
    );
    assert_eq!(page_data(Some(&deep)).unwrap_err().0, "invalid_data");
    assert_eq!(page_data(Some("{")).unwrap_err().0, "invalid_data");
}

/// Audit A2: a queued page message is never pasted. While its target reads
/// no messages by id it waits in the queue (the TTL ages it out); once a
/// reader is back it goes in as a pointer line.
#[tokio::test]
async fn a_queued_page_message_waits_instead_of_being_pasted() {
    use crate::detect::{Agent, AgentState};
    let mut app = model_app();
    let page = setup(&mut app);
    let mut rx = live(&mut app, 1, 1);
    reader(&mut app, 1, 1);
    let opener = pane(&app, 1, 1);
    crate::app::agents_model::tests::terminal_mut(&mut app, opener)
        .set_detected_state(Some(Agent::Claude), AgentState::Working);
    let reply = send(&mut app, params(&page));
    assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
    let id = reply["result"]["message_id"].as_str().unwrap().to_string();
    // the reader goes away, the agent goes idle
    app.agents_model.message_readers.clear();
    crate::app::agents_model::tests::terminal_mut(&mut app, opener)
        .set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.message_queue.mark_due();
    let t0 = std::time::Instant::now();
    let now_unix = crate::coordinator::now_unix();
    app.message_queue_pass(t0, now_unix);
    app.message_queue_pass(t0 + crate::app::message_queue::SETTLE, now_unix);
    let text = typed(&mut rx);
    assert!(!text.contains(PASTE_START), "never pasted: {text:?}");
    assert!(!text.contains("page content"), "{text:?}");
    assert!(
        app.message_queue
            .entries
            .iter()
            .any(|entry| entry.message.id.as_deref() == Some(id.as_str())),
        "it waits in the queue"
    );
    // a reader again: a pointer line
    reader(&mut app, 1, 1);
    app.message_queue.mark_due();
    let t1 = std::time::Instant::now();
    app.message_queue_pass(t1, now_unix);
    app.message_queue_pass(t1 + crate::app::message_queue::SETTLE, now_unix);
    let text = typed(&mut rx);
    assert!(!text.contains(PASTE_START), "{text:?}");
    assert!(
        text.contains(&format!("herdr+ message {id} from a page you showed")),
        "{text:?}"
    );
}
