//! The News tab on the client: the `tabs` sidebar's pinned row, its menu,
//! and when `news.get` is pulled.

use super::*;
use crate::api::schema::{Method, NewsGetInfo, NewsLastRun, NewsRunInfo, ResponseResult};
use crate::config::{Config, SidebarLayoutConfig};
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config
}

fn tab(tab_id: &str, label: &str, focused: bool, status: AgentStatus) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: status,
        color: None,
        important: false,
        remind_every: None,
    }
}

/// Three tabs, the News tab in the middle so skipping it is visible.
fn news_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = vec![
        tab("tab_1", "reviewer", true, AgentStatus::Working),
        tab("tab_2", "News", false, AgentStatus::Idle),
        tab("tab_3", "planner", false, AgentStatus::Idle),
    ];
    snapshot
}

fn info(tab_id: Option<&str>) -> NewsGetInfo {
    NewsGetInfo {
        enabled: true,
        interval_hours: 6,
        quiet_hours: "00:00-08:00".into(),
        model: None,
        tab_id: tab_id.map(str::to_string),
        pane_id: None,
        next_run_at: Some(1_800_000_000),
        run: None,
        last_run: Some(NewsLastRun {
            started_at: 1_790_000_000,
            ended_at: Some(1_790_000_300),
            trigger: "scheduled".into(),
            outcome: "ok".into(),
            edition: Some(3),
            changed: true,
            summary: None,
            error: None,
        }),
        unread: false,
        consecutive_failures: 0,
    }
}

fn tabs_state(snapshot: ClientShellSnapshot) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state
}

fn tick(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.tick_news(std::time::Instant::now(), &mut outcome);
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

/// Pull `news.get` on the tick and answer it with `info`.
fn deliver(state: &mut ClientShellState, info: NewsGetInfo) {
    let outcome = tick(state);
    let requests = endpoint_requests(&outcome);
    let [(request_id, Method::NewsGet(_))] = &requests[..] else {
        panic!("expected one news.get, got {requests:?}");
    };
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::NewsGet { news: info }),
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

fn listed_tab_ids(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.clone())
        .collect()
}

#[test]
fn the_news_row_is_pulled_on_attach_and_pins_the_tab_out_of_the_list() {
    let mut state = tabs_state(news_snapshot());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_2", "tab_3"]);
    let list_height = state.hits.agent_body.height;

    deliver(&mut state, info(Some("tab_2")));
    assert!(
        tick(&mut state).actions.is_empty(),
        "nothing changed: no second pull"
    );
    let frame = state.compose(106, 20).expect("composed frame");
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_3"]);
    let row = state.hits.news_row;
    assert_ne!(row, ratatui::layout::Rect::default());
    assert_eq!(
        row.y,
        state.hits.agent_body.bottom(),
        "right under the list"
    );
    assert_eq!(state.hits.agent_body.height, list_height - 1);
    let text = row_text(&frame, row);
    assert!(text.contains("News"), "{text:?}");
    assert!(
        text.contains(&super::super::news::local_hhmm(1_790_000_300)),
        "idle rows show the last run's time: {text:?}"
    );
    assert!(text.trim_start().starts_with('○'), "{text:?}");
}

#[test]
fn the_row_shows_running_unread_failed_and_paused_in_that_order() {
    let mut state = tabs_state(news_snapshot());
    state.compose(106, 20).expect("composed frame");
    let mut running = info(Some("tab_2"));
    running.run = Some(NewsRunInfo {
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 125,
        trigger: "manual".into(),
        phase: "running".into(),
    });
    deliver(&mut state, running.clone());
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("running 2m"), "{text:?}");
    assert!(text.trim_start().starts_with('◐'), "{text:?}");
    assert!(
        state
            .next_news_deadline(std::time::Instant::now())
            .is_some(),
        "the tick wakes for the next minute while running"
    );

    // Unread comes from the snapshot's important flag, not from news.get.
    let mut snapshot = news_snapshot();
    snapshot.tabs[1].important = true;
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    deliver(&mut state, info(Some("tab_2")));
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("unread"), "{text:?}");
    assert!(text.trim_start().starts_with('●'), "{text:?}");
    assert!(
        state
            .next_news_deadline(std::time::Instant::now())
            .is_none(),
        "no minute clock without a run"
    );

    let snapshot = news_snapshot();
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let mut failed = info(Some("tab_2"));
    failed.last_run.as_mut().unwrap().outcome = "invalid".into();
    failed.enabled = false;
    deliver(&mut state, failed);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("failed"), "failed outranks paused: {text:?}");
    assert!(text.trim_start().starts_with('×'), "{text:?}");

    let mut paused = info(Some("tab_2"));
    paused.enabled = false;
    state.news.info = Some(paused);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("paused"), "{text:?}");
    assert!(text.trim_start().starts_with('◌'), "{text:?}");
}

#[test]
fn the_row_sits_above_the_status_footer_and_goes_with_the_tab() {
    let mut snapshot = news_snapshot();
    snapshot.tab_bar_right = vec![crate::protocol::ClientShellTabStatusSegment {
        text: "cpu 12%".into(),
        accent: false,
    }];
    let mut state = tabs_state(snapshot);
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, info(Some("tab_2")));
    let frame = state.compose(106, 20).expect("composed frame");
    let row = state.hits.news_row;
    let footer = row_text(
        &frame,
        ratatui::layout::Rect::new(row.x, row.y + 1, row.width, 1),
    );
    assert!(
        footer.contains("cpu 12%"),
        "the footer is under the row: {footer:?}"
    );
    assert_eq!(row.y, state.hits.agent_body.bottom());

    // The tab is closed: the row disappears and the list grows back.
    let mut snapshot = news_snapshot();
    snapshot.tabs.remove(1);
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let outcome = tick(&mut state);
    assert!(
        matches!(&endpoint_requests(&outcome)[..], [(_, Method::NewsGet(_))]),
        "a tab set change pulls again"
    );
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_3"]);
}

#[test]
fn the_news_tab_state_change_and_news_replies_pull_again_but_nothing_else_does() {
    let mut state = tabs_state(news_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, info(Some("tab_2")));
    assert!(tick(&mut state).actions.is_empty());

    // Another tab's status: no pull.
    let mut snapshot = news_snapshot();
    snapshot.tabs[2].agent_status = AgentStatus::Working;
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    assert!(tick(&mut state).actions.is_empty());

    // The News tab starts working: pull.
    let mut snapshot = news_snapshot();
    snapshot.tabs[1].agent_status = AgentStatus::Working;
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let outcome = tick(&mut state);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::NewsGet(_))]),
        "{requests:?}"
    );
    assert!(
        tick(&mut state).actions.is_empty(),
        "one pull per change, even before the reply"
    );
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Err(ClientShellEndpointError {
            code: Some("news_unavailable".into()),
            message: "no home".into(),
        }),
    );
    assert!(repaint);
    assert!(state.news.info.is_none(), "an error clears the record");
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert!(tick(&mut state).actions.is_empty(), "and is not retried");

    // A news.* reply asks for a fresh record.
    state.refresh_news();
    let outcome = tick(&mut state);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::NewsGet(_))]
    ));
}

#[test]
fn no_pull_from_a_server_without_news_get() {
    let mut state = tabs_state(news_snapshot());
    state.set_endpoint_methods(Some(vec!["tab.focus".into()]));
    state.compose(106, 20).expect("composed frame");
    let outcome = tick(&mut state);
    assert!(outcome.actions.is_empty());
    assert!(
        state.visible_endpoint_notice.is_none(),
        "no unsupported notice"
    );
    assert!(tick(&mut state).actions.is_empty());
}

#[test]
fn the_row_focuses_on_click_and_its_menu_runs_opens_and_pauses() {
    let mut state = tabs_state(news_snapshot());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, info(Some("tab_2")));
    state.compose(106, 20).expect("composed frame");
    let row = state.hits.news_row;

    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::TabFocus(target))] if target.tab_id == "tab_2"),
        "{requests:?}"
    );
    assert!(state.tab_press.is_none(), "the pinned row is not dragged");

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
                ClientContextMenuTarget::News { enabled: true }
            ));
            menu.items()
                .iter()
                .map(|item| item.label)
                .collect::<Vec<_>>()
        }
        other => panic!("expected the News menu, got {other:?}"),
    };
    assert_eq!(labels, ["Run now", "Open", "Pause schedule"]);

    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(0, &mut outcome);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::NewsRun(_))]
    ));
    assert!(state.overlay.is_none());

    state.open_news_context_menu(row.x, row.y);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(1, &mut outcome);
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::NewsOpen(params))] if params.edition.is_none()
    ));

    state.open_news_context_menu(row.x, row.y);
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(2, &mut outcome);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::NewsSetEnabled(params))] if !params.enabled),
        "{requests:?}"
    );
    // The reply carries the new record and a fresh pull follows.
    let mut paused = info(Some("tab_2"));
    paused.enabled = false;
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        &requests[0].0,
        Ok(ResponseResult::NewsGet { news: paused }),
    );
    assert!(repaint);
    assert!(!state.news.info.as_ref().unwrap().enabled);
    state.open_news_context_menu(row.x, row.y);
    if let Some(ClientShellOverlay::ContextMenu(menu)) = &state.overlay {
        assert_eq!(menu.items()[2].label, "Resume schedule");
    }
    state.overlay = None;
    assert!(matches!(
        &endpoint_requests(&tick(&mut state))[..],
        [(_, Method::NewsGet(_))]
    ));
}

#[test]
fn the_open_news_binding_requests_news_open_in_either_layout() {
    use crate::input::{KeybindAction, KeybindMatch};
    for config in [tabs_config(), Config::default()] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        let mut outcome = ClientShellInput::default();
        state.record_binding(KeybindMatch::Action(KeybindAction::OpenNews), &mut outcome);
        let requests = endpoint_requests(&outcome);
        assert!(
            matches!(&requests[..], [(_, Method::NewsOpen(params))] if params.edition.is_none()),
            "{requests:?}"
        );
    }
    let bound: Config = toml::from_str("[keys]\nopen_news = \"alt+n\"").unwrap();
    assert!(!bound
        .live_keybinds_with_diagnostics()
        .map(|(keybinds, _)| keybinds.keybinds.open_news.bindings.is_empty())
        .unwrap_or(true));
    assert!(!Config::default().keys.open_news.has_values());
}

#[test]
fn the_spaces_layout_keeps_the_news_tab_in_its_list() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(news_snapshot()));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, info(Some("tab_2")));
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert!(state.hits.tabs.iter().any(|(_, tab_id)| tab_id == "tab_2"));
}
