use super::*;
use crate::api::schema::{
    BrowserHostEntry, BrowserHostSyncGroup, BrowserHostSyncParams, Method, TabSetDndParams,
};
use crate::app::agents_model::tests::{call, code, model_app, pane, public, terminal_mut};
use crate::detect::{Agent, AgentState};
use serde_json::Value;

/// One process-global hub per test process (nextest): a live test lease.
fn leased(lease: &str) {
    hub().install_test_lease(lease);
}

fn entry(view: &str, url: &str) -> BrowserHostEntry {
    BrowserHostEntry {
        view_id: Some(view.into()),
        target: Some(format!("T-{view}")),
        url: url.into(),
        title: format!("title {url}"),
        ..Default::default()
    }
}

fn sync(
    app: &mut App,
    lease: &str,
    seq: u64,
    tab_id: &str,
    entries: Vec<BrowserHostEntry>,
) -> Value {
    call(
        app,
        Method::BrowserHostSync(BrowserHostSyncParams {
            lease_id: lease.into(),
            seq,
            groups: vec![BrowserHostSyncGroup {
                tab_id: tab_id.into(),
                key: None,
                active: None,
                entries,
            }],
        }),
    )
}

fn send_page(app: &mut App, tab_id: &str) -> Value {
    call(
        app,
        Method::BrowserSendPage(BrowserSendPageParams {
            tab_id: tab_id.into(),
            url: "https://docs.test/page".into(),
            title: "Docs ```".into(),
            selection: Some("selected words".into()),
            note: Some("look at this".into()),
            lease_id: None,
        }),
    )
}

#[test]
fn a_stale_sync_seq_changes_nothing() {
    let mut app = model_app();
    leased("L1");
    let tab_id = app.public_tab_id(1, 0).unwrap();
    let first = sync(
        &mut app,
        "L1",
        5,
        &tab_id,
        vec![entry("v1", "https://a.test/")],
    );
    assert_eq!(code(&first), "ok", "{first}");
    let key = first["result"]["keys"][&tab_id]
        .as_str()
        .unwrap()
        .to_string();
    assert!(key.starts_with("g-"));
    assert_eq!(first["result"]["keys"][format!("{tab_id}/v1")], "b1");
    let before = app.state.workspaces[1].tabs[0].browser.clone();
    // An older (or the same) seq: refused, nothing applied.
    for seq in [5, 3] {
        let stale = sync(
            &mut app,
            "L1",
            seq,
            &tab_id,
            vec![entry("v9", "https://b.test/")],
        );
        assert_eq!(code(&stale), "stale_seq", "{stale}");
    }
    assert_eq!(app.state.workspaces[1].tabs[0].browser, before);
    // Another lease: lease_unknown.
    let other = sync(&mut app, "L-old", 9, &tab_id, vec![]);
    assert_eq!(code(&other), "lease_unknown");
    // A herdr tab that is gone is answered, not created.
    let gone = sync(
        &mut app,
        "L1",
        6,
        "w9:t9",
        vec![entry("v2", "https://c.test/")],
    );
    assert_eq!(gone["result"]["unknown_tabs"][0], "w9:t9");
    // host_state lists every tab, the group's key and entries.
    let state = call(
        &mut app,
        Method::BrowserHostState(crate::api::schema::BrowserHostStateParams {
            lease_id: "L1".into(),
            tab_ids: None,
        }),
    );
    let tabs = state["result"]["state"]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 5);
    let ours = tabs
        .iter()
        .find(|tab| tab["tab_id"] == tab_id.as_str())
        .unwrap();
    assert_eq!(ours["key"], key.as_str());
    assert_eq!(ours["entries"][0]["id"], "b1");
    assert!(tabs
        .iter()
        .filter(|tab| tab["tab_id"] != tab_id.as_str())
        .all(|tab| tab.get("key").is_none()));
    assert!(state["result"]["state"]["focused_tab_id"].is_string());
    // browser.get's sessions (built here; browser.get itself also runs the
    // doctor's checks, which look at the real home directory).
    let sessions = app.browser_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].tab_id, tab_id);
    assert_eq!(sessions[0].pages, 1);
    assert_eq!(sessions[0].title, "title https://a.test/");
    assert!(!sessions[0].needs_you);
}

#[test]
fn fork_smoke_send_page_to_a_dnd_tab_is_stored_quietly() {
    let mut app = model_app();
    let tab_id = app.public_tab_id(1, 1).unwrap();
    let reply = call(
        &mut app,
        Method::TabSetDnd(TabSetDndParams {
            tab_id: tab_id.clone(),
            dnd: true,
        }),
    );
    assert_eq!(code(&reply), "ok", "{reply}");
    let quiet_before = app.quiet.entries.len();
    let sent = send_page(&mut app, &tab_id);
    assert_eq!(code(&sent), "ok", "{sent}");
    assert_eq!(sent["result"]["type"], "browser_send_page");
    assert_eq!(sent["result"]["outcome"], "queued");
    assert_eq!(
        sent["result"]["reason"],
        crate::app::quiet_messages::DND_REASON
    );
    assert_eq!(sent["result"]["to_pane"], public(&app, 1, 1).as_str());
    assert_eq!(app.quiet.entries.len(), quiet_before + 1);
    let stored = &app.quiet.entries.last().unwrap().message;
    assert_eq!(stored.from_name.as_deref(), Some(SEND_PAGE_FROM));
    assert!(stored.from_pane.is_none());
    assert!(stored.text.contains("untrusted content"));
    assert!(stored.text.contains("```text\nselected words\n```"));
    assert!(
        !stored.text.contains("Docs ```"),
        "page text cannot close the fence"
    );
    assert!(app.message_queue.entries.is_empty(), "never typed in");
}

#[test]
fn send_page_picks_the_focused_agent_pane_then_the_last_user() {
    let mut app = model_app();
    // plain (w idx 2) tab 0 runs an agent; add a shell pane and focus it.
    let agent_pane = pane(&app, 2, 0);
    app.state.workspaces[2].active_tab = 0;
    let shell = app.state.workspaces[2].test_split(ratatui::layout::Direction::Horizontal);
    app.state.ensure_test_terminals();
    app.state.workspaces[2].tabs[0].layout.focus_pane(shell);
    assert_eq!(
        app.send_page_target(2, 0),
        Some(agent_pane),
        "shell focused: the agent"
    );
    // A second agent that used the active page last: it wins over the
    // first agent while a shell is focused.
    let other = app.state.workspaces[2].test_split(ratatui::layout::Direction::Vertical);
    app.state.ensure_test_terminals();
    terminal_mut(&mut app, other).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    app.state.workspaces[2].tabs[0].layout.focus_pane(shell);
    let other_public = app.public_pane_id(2, other).unwrap();
    let user = app.browser_actor_for_pane(&other_public).expect("actor");
    hub().with_state_mut(|state| {
        state.adopt_tab(
            &crate::browser::state::TabKey::new(COMPANION_PROFILE, "T-last"),
            &crate::browser::state::HostTab {
                target: "T-last".into(),
                url: "https://a.test/".into(),
                ..Default::default()
            },
            &user,
            1,
        )
    });
    let mut browser = TabBrowser::new(None);
    browser.apply_sync(
        "x",
        &BrowserHostSyncGroup {
            tab_id: "x".into(),
            entries: vec![BrowserHostEntry {
                view_id: Some("v1".into()),
                target: Some("T-last".into()),
                url: "https://a.test/".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        &mut Default::default(),
    );
    app.state.workspaces[2].tabs[0].browser = Some(browser);
    assert_eq!(
        app.send_page_target(2, 0),
        Some(other),
        "the last user of the page"
    );
    // An agent focused: the focused one wins.
    terminal_mut(&mut app, shell).set_detected_state(Some(Agent::Claude), AgentState::Idle);
    assert_eq!(app.send_page_target(2, 0), Some(shell));
    // A tab with no agent: no_agent_in_tab.
    let bucket = app.public_tab_id(0, 0).unwrap();
    let refused = send_page(&mut app, &bucket);
    assert_eq!(code(&refused), "no_agent_in_tab");
    assert_eq!(code(&send_page(&mut app, "w9:t9")), "tab_not_found");
    // The body text: fenced selection, the note, the url.
    let text = send_page_text(&BrowserSendPageParams {
        tab_id: String::new(),
        url: "https://x.test/".into(),
        title: "X".into(),
        selection: Some("y".repeat(MAX_SELECTION_CHARS + 5)),
        note: None,
        lease_id: None,
    });
    assert!(text.contains("…(truncated 5 chars)"));
}
