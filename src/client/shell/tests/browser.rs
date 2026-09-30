//! The herdr browser on the client: the `tabs` sidebar's pinned Browser row
//! (above News), the `◎` marker on tab rows, the overlay, its menu, and
//! when `browser.get` is pulled.

use super::*;
use crate::api::schema::{
    BrowserActor, BrowserGetInfo, BrowserPaneCursor, BrowserProfileInfo, BrowserTabInfo,
    BrowserTouch, Method, ResponseResult,
};
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn tab(tab_id: &str, label: &str, focused: bool) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: AgentStatus::Idle,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn browser_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = vec![
        tab("tab_1", "planner", true),
        tab("tab_2", "reviewer", false),
    ];
    snapshot
}

fn tabs_state(snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn pane_actor(pane_id: &str, tab_id: &str) -> BrowserActor {
    BrowserActor::Pane {
        pane_id: pane_id.into(),
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        tab_label: "planner".into(),
        workspace_label: None,
        agent: Some("claude".into()),
        session: "default".into(),
        gone: false,
    }
}

fn running_info(seq: u64, touched_at: u64) -> BrowserGetInfo {
    BrowserGetInfo {
        seq,
        unchanged: false,
        enabled: true,
        host: Default::default(),
        profiles: vec![BrowserProfileInfo {
            name: "main".into(),
            state: "running".into(),
            pid: Some(7),
            port: Some(9),
            tabs: 2,
            agents: 1,
            ..Default::default()
        }],
        tabs: vec![
            BrowserTabInfo {
                id: "main:t2".into(),
                profile: "main".into(),
                target_id: "T2".into(),
                url: "https://github.com/x/pull/1".into(),
                title: "PR #1".into(),
                selected: false,
                opened_by: pane_actor("pane_1", "tab_1"),
                last: Some(BrowserTouch {
                    actor: pane_actor("pane_1", "tab_1"),
                    op: "read".into(),
                    detail: "markdown".into(),
                    at: touched_at,
                    ok: true,
                }),
                last_actor: pane_actor("pane_1", "tab_1"),
                users: vec!["pane_1".into()],
                dialog_open: false,
                console_errors: 0,
                active: true,
                state: "open".into(),
                closed_at: None,
            },
            BrowserTabInfo {
                id: "main:t1".into(),
                profile: "main".into(),
                target_id: "T1".into(),
                url: "about:blank".into(),
                title: String::new(),
                selected: true,
                opened_by: BrowserActor::User,
                last: None,
                last_actor: BrowserActor::User,
                users: vec![],
                dialog_open: false,
                console_errors: 0,
                active: false,
                state: "open".into(),
                closed_at: None,
            },
        ],
        recent_panes: vec![BrowserPaneCursor {
            pane_id: "pane_1".into(),
            tab_id: Some("tab_1".into()),
            current: "main:t2".into(),
            last_at: touched_at,
        }],
    }
}

fn stopped_info(seq: u64) -> BrowserGetInfo {
    BrowserGetInfo {
        seq,
        unchanged: false,
        enabled: true,
        profiles: vec![BrowserProfileInfo {
            name: "main".into(),
            state: "stopped".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn tick(state: &mut ClientShellState, at: std::time::Instant) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_browser(at, &mut outcome);
    outcome
}

fn endpoint_requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Pull `browser.get` on the tick and answer it with `info`.
fn deliver(state: &mut ClientShellState, info: BrowserGetInfo) {
    let outcome = tick(state, std::time::Instant::now());
    let requests = endpoint_requests(&outcome);
    let [(request_id, Method::BrowserGet(_))] = &requests[..] else {
        panic!("expected one browser.get, got {requests:?}");
    };
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::BrowserGet { browser: info }),
    );
    assert!(repaint);
}

fn row_text(frame: &FrameData, rect: ratatui::layout::Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect::<String>()
}

fn frame_text(frame: &FrameData) -> String {
    (0..frame.height)
        .map(|y| row_text(frame, ratatui::layout::Rect::new(0, y, frame.width, 1)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_browser_row_is_pulled_on_attach_and_shows_the_running_state() {
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.browser_row, ratatui::layout::Rect::default());
    let list_height = state.hits.agent_body.height;

    deliver(&mut state, running_info(5, now()));
    assert!(
        tick(&mut state, std::time::Instant::now())
            .actions
            .is_empty(),
        "just pulled: no second pull"
    );
    let frame = state.compose(106, 20).expect("composed frame");
    let row = state.hits.browser_row;
    assert_ne!(row, ratatui::layout::Rect::default());
    assert_eq!(
        row.y,
        state.hits.agent_body.bottom(),
        "right under the list"
    );
    assert_eq!(state.hits.agent_body.height, list_height - 1);
    let text = row_text(&frame, row);
    assert!(text.contains("Browser"), "{text:?}");
    assert!(text.contains("2 tabs"), "{text:?}");
    assert!(text.trim_start().starts_with('◎'), "{text:?}");

    // while a profile runs the record is pulled again after 2 s, with since_seq
    let later = tick(
        &mut state,
        std::time::Instant::now() + super::super::browser::BROWSER_REFRESH_ACTIVE,
    );
    let requests = endpoint_requests(&later);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserGet(params))] if params.since_seq == Some(5)),
        "{requests:?}"
    );
    // an unchanged answer keeps the record
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(ResponseResult::BrowserGet {
            browser: BrowserGetInfo {
                seq: 5,
                unchanged: true,
                enabled: true,
                ..Default::default()
            },
        }),
    );
    assert!(state
        .browser_row()
        .is_some_and(|row| row.status == "2 tabs · 1 agent"));
}

#[test]
fn the_row_shows_crashed_dialog_starting_running_and_stopped_in_that_order() {
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    let mut info = running_info(1, now());
    deliver(&mut state, info.clone());
    let states = |state: &mut ClientShellState, info: &BrowserGetInfo| {
        state.browser.info = Some(info.clone());
        let row = state.browser_row().expect("a row");
        (row.state, row.status)
    };
    info.profiles[0].state = "crashed".into();
    info.tabs[0].dialog_open = true;
    assert_eq!(
        states(&mut state, &info),
        (
            super::super::browser::BrowserRowState::Crashed,
            "crashed".into()
        )
    );
    info.profiles[0].state = "running".into();
    assert_eq!(
        states(&mut state, &info),
        (
            super::super::browser::BrowserRowState::Dialog,
            "dialog open".into()
        )
    );
    info.tabs[0].dialog_open = false;
    info.profiles[0].state = "starting".into();
    assert_eq!(
        states(&mut state, &info),
        (
            super::super::browser::BrowserRowState::Starting,
            "starting".into()
        )
    );
    info.profiles[0].state = "running".into();
    assert_eq!(
        states(&mut state, &info).0,
        super::super::browser::BrowserRowState::Running
    );
    assert_eq!(
        states(&mut state, &stopped_info(2)),
        (
            super::super::browser::BrowserRowState::Stopped,
            "stopped".into()
        )
    );
    // disabled and never ran: no row; disabled after a profile ran: still a row
    let disabled = BrowserGetInfo {
        enabled: false,
        ..Default::default()
    };
    state.browser.seen_running = false;
    state.browser.info = Some(disabled.clone());
    assert!(state.browser_row().is_none());
    state.browser.seen_running = true;
    assert!(state.browser_row().is_some());
}

#[test]
fn the_browser_row_sits_above_the_news_row_and_news_keeps_its_row() {
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, running_info(1, now()));
    // a news record without a tab, enabled: the News row shows too
    state.news.info = Some(crate::api::schema::NewsGetInfo {
        enabled: true,
        times: vec![],
        quiet_hours: String::new(),
        model: None,
        tab_id: None,
        pane_id: None,
        next_run_at: None,
        run: None,
        last_run: None,
        unread: false,
        consecutive_failures: 0,
        pending_notifications: 0,
    });
    let frame = state.compose(106, 20).expect("composed frame");
    let browser = state.hits.browser_row;
    let news = state.hits.news_row;
    assert_ne!(browser, ratatui::layout::Rect::default());
    assert_ne!(news, ratatui::layout::Rect::default());
    assert_eq!(browser.y, state.hits.agent_body.bottom());
    assert_eq!(news.y, browser.y + 1, "News directly under Browser");
    assert!(row_text(&frame, browser).contains("Browser"));
    assert!(row_text(&frame, news).contains("News"));
}

#[test]
fn tabs_whose_panes_use_the_browser_wear_the_marker_within_the_window() {
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, running_info(1, now()));
    assert_eq!(
        state.browser_marked_tabs(),
        std::collections::HashSet::from(["tab_1".to_string()])
    );
    let frame = state.compose(106, 20).expect("composed frame");
    let planner = state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == "tab_1")
        .map(|(rect, _)| *rect)
        .expect("planner row");
    let reviewer = state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == "tab_2")
        .map(|(rect, _)| *rect)
        .expect("reviewer row");
    assert!(
        row_text(&frame, planner).contains('◎'),
        "{:?}",
        row_text(&frame, planner)
    );
    assert!(!row_text(&frame, reviewer).contains('◎'));
    // outside the window: no marker
    state.browser.info = Some(running_info(2, now() - 500));
    assert!(state.browser_marked_tabs().is_empty());
    // the window is the config value
    state.config.browser_active_glyph_secs = 1000;
    assert_eq!(state.browser_marked_tabs().len(), 1);
    state.config.browser_active_glyph_secs = 0;
    assert!(state.browser_marked_tabs().is_empty());
}

#[test]
fn no_pull_from_a_server_without_browser_get_and_idle_cadence_is_slow() {
    let mut state = tabs_state(browser_snapshot());
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    let outcome = tick(&mut state, std::time::Instant::now());
    assert!(endpoint_requests(&outcome).is_empty());
    assert!(state.browser_row().is_none());

    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, stopped_info(1));
    let start = std::time::Instant::now();
    assert!(
        endpoint_requests(&tick(&mut state, start + std::time::Duration::from_secs(5))).is_empty(),
        "stopped: nothing after 5 s"
    );
    assert!(
        matches!(
            &endpoint_requests(&tick(
                &mut state,
                start + super::super::browser::BROWSER_REFRESH_IDLE
            ))[..],
            [(_, Method::BrowserGet(_))]
        ),
        "stopped: pulled after 15 s"
    );
}

#[test]
fn the_row_opens_the_overlay_and_its_menu_focuses_starts_and_stops() {
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, running_info(1, now()));
    state.compose(106, 20).expect("composed frame");
    let row = state.hits.browser_row;

    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Browser(_))
    ));
    let pull = tick(&mut state, std::time::Instant::now());
    let requests = endpoint_requests(&pull);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserGet(_))]),
        "opening the overlay pulls again: {requests:?}"
    );
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(ResponseResult::BrowserGet {
            browser: running_info(2, now()),
        }),
    );
    drop(outcome);
    state.overlay = None;

    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let labels = match &state.overlay {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            assert!(matches!(
                &menu.target,
                ClientContextMenuTarget::Browser {
                    running: true,
                    local: true,
                    ..
                }
            ));
            menu.items()
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>()
        }
        other => panic!("expected the Browser menu, got {other:?}"),
    };
    assert_eq!(labels, ["Focus window", "Stop profile", "Open overlay"]);

    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(0, &mut outcome);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserFocus(target))] if target.tab == "t1" && target.profile.as_deref() == Some("main")),
        "the selected tab: {requests:?}"
    );
    state.open_browser_context_menu(row.x, row.y);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(1, &mut outcome);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::BrowserStop(params))] if params.profile.as_deref() == Some("main")
    ));
    // a stopped profile offers Start
    state.browser.info = Some(stopped_info(2));
    state.open_browser_context_menu(row.x, row.y);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(0, &mut outcome);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::BrowserStart(params))] if params.profile.as_deref() == Some("main")
    ));
    // the reply triggers a fresh pull
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &endpoint_requests(&outcome)[0].0,
        Ok(ResponseResult::BrowserGet {
            browser: running_info(3, now()),
        }),
    );
    assert!(matches!(
        &endpoint_requests(&tick(&mut state, std::time::Instant::now()))[..],
        [(_, Method::BrowserGet(_))]
    ));
}

#[test]
fn the_overlay_lists_profiles_and_tabs_and_its_keys_focus_and_toggle() {
    use crate::input::TerminalKey;
    let mut state = tabs_state(browser_snapshot());
    state.compose(106, 30).expect("composed frame");
    deliver(&mut state, running_info(1, now() - 12));
    let mut outcome = ClientShellInput::default();
    state.open_browser_overlay(&mut outcome);
    let frame = state.compose(106, 30).expect("composed frame");
    let text = frame_text(&frame);
    assert!(text.contains("browser"), "{text}");
    assert!(text.contains("main · running"), "{text}");
    assert!(text.contains("t2"), "{text}");
    assert!(text.contains("planner · claude read 12s"), "{text}");
    assert!(text.contains("github.com/x/pull/1"), "{text}");
    assert!(text.contains("t1  you"), "{text}");
    // the actor column is as wide as its widest row, not a fixed gap
    let t2_line = text
        .lines()
        .find(|line| line.contains("planner · claude read 12s"))
        .unwrap();
    let after = t2_line.split("read 12s").nth(1).unwrap();
    assert!(
        after.starts_with("  PR #1") || after.starts_with(" PR #1"),
        "{t2_line:?}"
    );
    let t1_line = text.lines().find(|line| line.contains("t1  you")).unwrap();
    let after = t1_line.split("t1  you").nth(1).unwrap();
    assert!(after.trim_start().starts_with("about:blank"), "{t1_line:?}");
    assert!(after.len() - after.trim_start().len() <= 28, "{t1_line:?}");
    assert!(text.contains("enter focus window"), "{text}");

    let key = |code: crossterm::event::KeyCode| TerminalKey::new(code, KeyModifiers::empty());
    // the cursor starts on the profile header; Down reaches the newest tab
    let mut outcome = ClientShellInput::default();
    state.route_overlay_key(&key(crossterm::event::KeyCode::Down), &mut outcome);
    let mut outcome = ClientShellInput::default();
    state.route_overlay_key(&key(crossterm::event::KeyCode::Enter), &mut outcome);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::BrowserFocus(target))] if target.tab == "t2"),
        "{requests:?}"
    );
    let mut outcome = ClientShellInput::default();
    state.route_overlay_key(&key(crossterm::event::KeyCode::Char('s')), &mut outcome);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::BrowserStop(params))] if params.profile.as_deref() == Some("main")
    ));
    let mut outcome = ClientShellInput::default();
    state.route_overlay_key(&key(crossterm::event::KeyCode::Esc), &mut outcome);
    assert!(state.overlay.is_none());
}

#[test]
fn the_open_browser_binding_opens_the_overlay_in_either_layout() {
    use crate::input::{KeybindAction, KeybindMatch};
    for config in [tabs_config(), Config::default()] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(browser_snapshot()));
        state.set_pane_surface(surface());
        let mut outcome = ClientShellInput::default();
        state.record_binding(
            KeybindMatch::Action(KeybindAction::OpenBrowser),
            &mut outcome,
        );
        assert!(matches!(
            state.overlay,
            Some(ClientShellOverlay::Browser(_))
        ));
        assert!(outcome.repaint);
    }
    let bound: Config = toml::from_str("[keys]\nopen_browser = \"alt+b\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.open_browser.bindings.is_empty())
        .unwrap_or(true));
    assert!(!Config::default().keys.open_browser.has_values());
}

pub(crate) mod fork_smoke {
    use super::*;

    /// The pinned Browser row reaches the tabs sidebar above the footer with
    /// the running state's glyph, count and the agent lighting it up.
    #[test]
    fn browser_row_shows_the_running_browser_above_the_footer() {
        let mut state = tabs_state(browser_snapshot());
        state.compose(106, 20).expect("composed frame");
        deliver(&mut state, running_info(1, now()));
        let frame = state.compose(106, 20).expect("composed frame");
        let row = state.hits.browser_row;
        assert_eq!(row.y, state.hits.agent_body.bottom());
        let text = row_text(&frame, row);
        assert!(text.trim_start().starts_with("◎ Browser"), "{text:?}");
        assert!(text.contains("2 tabs"), "{text:?}");
        let cell = &frame.cells[(row.y * frame.width + row.x + 1) as usize];
        assert_eq!(cell.symbol, "◎");
    }
}
