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
        times: vec!["08:00".into(), "13:00".into(), "19:00".into()],
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
        pending_notifications: 0,
        last_read_edition: None,
        new_stories: None,
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
    let mut later = ClientShellInput::default();
    state.tick_news(
        std::time::Instant::now() + super::super::news::NEWS_REFRESH_INTERVAL,
        &mut later,
    );
    assert!(
        matches!(&endpoint_requests(&later)[..], [(_, Method::NewsGet(_))]),
        "a minute later the record is pulled again (changes made from a shell)"
    );
    let (_, _) = state.handle_endpoint_result(
        "boot-1",
        &endpoint_requests(&later)[0].0,
        Ok(ResponseResult::NewsGet {
            news: info(Some("tab_2")),
        }),
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
        text.contains(&format!(
            "next {}",
            super::super::news::local_hhmm(1_800_000_000)
        )),
        "scheduled rows show the next run's time: {text:?}"
    );
    assert!(text.trim_start().starts_with('○'), "{text:?}");
}

#[test]
fn ago_is_compact() {
    use super::super::news::ago;
    assert_eq!(ago(1000, 1030), "now");
    assert_eq!(ago(1000, 1000 + 17 * 60), "17m");
    assert_eq!(ago(1000, 1000 + 3 * 3600 + 5), "3h");
    assert_eq!(ago(1000, 1000 + 2 * 86_400), "2d");
    assert_eq!(ago(2000, 1000), "now", "a clock skew never goes negative");
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
    state.news.info = Some(paused.clone());
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(
        text.contains("d ago"),
        "paused rows say how long ago the last run was: {text:?}"
    );
    assert!(text.trim_start().starts_with('◌'), "{text:?}");

    paused.last_run = None;
    state.news.info = Some(paused);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("paused"), "no run yet: {text:?}");
}

#[test]
fn the_unread_row_counts_new_stories_when_news_get_has_them() {
    let mut state = tabs_state(news_snapshot());
    state.compose(106, 20).expect("composed frame");
    let mut snapshot = news_snapshot();
    snapshot.tabs[1].important = true;
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let mut counted = info(Some("tab_2"));
    counted.last_read_edition = Some(2);
    counted.new_stories = Some(3);
    deliver(&mut state, counted);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("3 new"), "{text:?}");
    assert!(text.trim_start().starts_with('●'), "{text:?}");
    // zero new (the important mark is older than the count) falls back to "unread"
    let mut zero = info(Some("tab_2"));
    zero.new_stories = Some(0);
    state.news.info = Some(zero);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("unread"), "{text:?}");
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

    // The tab is closed with news enabled: the row stays, without a tab.
    let mut snapshot = news_snapshot();
    snapshot.tabs.remove(1);
    snapshot.tab_bar_right = vec![crate::protocol::ClientShellTabStatusSegment {
        text: "cpu 12%".into(),
        accent: false,
    }];
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    let outcome = tick(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(request_id, Method::NewsGet(_))] = &requests[..] else {
        panic!("a tab set change pulls again: {requests:?}");
    };
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, row, "the row stays while enabled");
    assert_eq!(state.news_row().unwrap().tab_id, None);
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_3"]);

    // With news disabled and no tab the row disappears and the list grows
    // back.
    let mut disabled = info(None);
    disabled.enabled = false;
    state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::NewsGet { news: disabled }),
    );
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert_eq!(state.hits.agent_body.bottom(), row.y + 1);
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_3"]);
}

/// A snapshot without a News tab.
fn snapshot_without_news() -> ClientShellSnapshot {
    let mut snapshot = news_snapshot();
    snapshot.tabs.remove(1);
    snapshot
}

#[test]
fn with_news_enabled_the_row_shows_without_a_tab() {
    let mut state = tabs_state(snapshot_without_news());
    state.compose(106, 20).expect("composed frame");
    let list_height = state.hits.agent_body.height;
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());

    deliver(&mut state, info(None));
    let frame = state.compose(106, 20).expect("composed frame");
    let row = state.hits.news_row;
    assert_ne!(row, ratatui::layout::Rect::default(), "the row shows");
    assert_eq!(state.hits.agent_body.height, list_height - 1);
    assert_eq!(row.y, state.hits.agent_body.bottom());
    assert_eq!(listed_tab_ids(&state), ["tab_1", "tab_3"]);
    let text = row_text(&frame, row);
    assert!(text.contains("News"), "{text:?}");
    assert!(
        text.contains(&format!(
            "next {}",
            super::super::news::local_hhmm(1_800_000_000)
        )),
        "the next scheduled run: {text:?}"
    );
    let news_row = state.news_row().expect("row");
    assert_eq!(news_row.tab_id, None);
    assert!(!news_row.focused);

    // No slot scheduled: the last run's time, else `never run`.
    let mut unscheduled = info(None);
    unscheduled.next_run_at = None;
    state.news.info = Some(unscheduled.clone());
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(
        text.contains(&super::super::news::local_hhmm(1_790_000_300)),
        "{text:?}"
    );
    unscheduled.last_run = None;
    state.news.info = Some(unscheduled);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("never run"), "{text:?}");
    assert!(text.trim_start().starts_with('○'), "{text:?}");

    // A run in flight before the tab exists still reads `running`.
    let mut running = info(None);
    running.run = Some(NewsRunInfo {
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 60,
        trigger: "scheduled".into(),
        phase: "starting".into(),
    });
    state.news.info = Some(running);
    let frame = state.compose(106, 20).expect("composed frame");
    let text = row_text(&frame, state.hits.news_row);
    assert!(text.contains("running 1m"), "{text:?}");
}

#[test]
fn with_news_disabled_and_no_tab_there_is_no_row() {
    let mut state = tabs_state(snapshot_without_news());
    state.compose(106, 20).expect("composed frame");
    let mut disabled = info(None);
    disabled.enabled = false;
    deliver(&mut state, disabled.clone());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.hits.news_row, ratatui::layout::Rect::default());
    assert!(state.news_row().is_none());

    // A stale tab id (the snapshot no longer lists it) is no tab either.
    disabled.tab_id = Some("tab_2".into());
    state.news.info = Some(disabled);
    assert!(state.news_row().is_none());
}

#[test]
fn a_click_on_the_row_without_a_tab_opens_news_and_its_menu_works() {
    let mut state = tabs_state(snapshot_without_news());
    state.compose(106, 20).expect("composed frame");
    deliver(&mut state, info(None));
    state.compose(106, 20).expect("composed frame");
    let row = state.hits.news_row;
    assert_ne!(row, ratatui::layout::Rect::default());

    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(&requests[..], [(_, Method::NewsOpen(params))] if params.edition.is_none()),
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
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::NewsSetEnabled(params))] if !params.enabled
    ));
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

fn settings_frame_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("settings frame");
    frame_rows(&frame).join("\n")
}

/// Tab through the sections to `news`; returns the last press's outcome.
fn open_news_section(state: &mut ClientShellState) -> ClientShellInput {
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::News)
        .expect("news section");
    let mut outcome = ClientShellInput::default();
    for _ in 0..index {
        outcome = state.handle_input_bytes(b"\t");
    }
    outcome
}

/// The reply to a `news.set_times` (or `news.set_enabled`) request: the same
/// record with the given times.
fn reply_times(state: &mut ClientShellState, request_id: &str, times: &[&str]) {
    let mut record = info(Some("tab_2"));
    record.times = times.iter().map(|time| (*time).to_string()).collect();
    state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::NewsGet { news: record }),
    );
}

fn set_times_request(outcome: &ClientShellInput) -> (String, Vec<String>) {
    let requests = endpoint_requests(outcome);
    match &requests[..] {
        [(id, Method::NewsSetTimes(params))] => (id.clone(), params.times.clone()),
        other => panic!("expected one news.set_times, got {other:?}"),
    }
}

fn selected_row(state: &ClientShellState) -> usize {
    match &state.overlay {
        Some(ClientShellOverlay::Settings(settings)) => settings.selected,
        _ => panic!("settings overlay"),
    }
}

#[test]
fn the_news_settings_section_shows_the_desk_and_edits_the_config() {
    let dir = std::env::temp_dir().join(format!("herdr-news-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[news]\nenabled = false\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
    let written = || -> crate::config::Config {
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    };

    let mut state = tabs_state(news_snapshot());
    state.compose(106, 30).expect("composed frame");
    let outcome = open_news_section(&mut state);
    let requests = endpoint_requests(&outcome);
    let [(request_id, Method::NewsGet(_))] = &requests[..] else {
        panic!("entering the section pulls news.get, got {requests:?}");
    };
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::News,
            ..
        }))
    ));
    let text = settings_frame_text(&mut state);
    assert!(text.contains("news desk"), "{text}");
    assert!(text.contains("loading news status"), "{text}");
    assert!(
        !text.contains("↵ apply"),
        "no primary button before the record"
    );

    let mut record = info(Some("tab_2"));
    record.model = Some("opus".into());
    let (repaint, _) = state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::NewsGet { news: record }),
    );
    assert!(repaint);
    let text = settings_frame_text(&mut state);
    for expected in [
        "scheduled runs: on",
        "08:00",
        "13:00",
        "19:00",
        "add time",
        "quiet hours 00:00-08:00 (notifications)",
        "run now",
        "model",
        "opus",
        "last run",
        "ok · edition 3 · changed",
        "next run",
        "↵ apply",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    assert!(
        text.contains(&super::super::news::local_hhmm(1_800_000_000)),
        "the next run's local time: {text}"
    );

    // Row 0: the toggle asks the active server (news.set_enabled), which
    // writes its own config; the local file is not touched.
    let outcome = state.handle_input_bytes(b"\r");
    let requests = endpoint_requests(&outcome);
    assert!(
        matches!(
            &requests[..],
            [(_, Method::NewsSetEnabled(params))] if !params.enabled
        ),
        "on -> off through the server: {requests:?}"
    );
    assert!(!written().news.enabled, "the local config is untouched");
    let (request_id, _) = &requests[0];
    let mut off = info(Some("tab_2"));
    off.enabled = false;
    state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(ResponseResult::NewsGet { news: off }),
    );
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("scheduled runs: off"),
        "the reply refreshes the row: {text}"
    );

    // Row 1 (08:00): Enter opens the time picker on the time; Esc leaves it
    // alone, Enter on another time sends the whole sorted list.
    state.handle_input_bytes(b"\x1b[B");
    state.handle_input_bytes(b"\r");
    let text = settings_frame_text(&mut state);
    assert!(text.contains("run at"), "{text}");
    assert!(text.contains("08:00 ✓"), "{text}");
    state.handle_input_bytes(b"\x1b");
    let text = settings_frame_text(&mut state);
    assert!(
        text.contains("news desk"),
        "esc returns to the rows: {text}"
    );
    assert_eq!(selected_row(&state), 1);
    state.handle_input_bytes(b"\r");
    state.handle_input_bytes(b"\x1b[B");
    let outcome = state.handle_input_bytes(b"\r");
    let (request_id, times) = set_times_request(&outcome);
    assert_eq!(times, ["08:30", "13:00", "19:00"]);
    assert_eq!(
        written().news.times,
        ["08:00", "13:00", "19:00"],
        "the local config is untouched: the server owns the times"
    );
    assert!(
        settings_frame_text(&mut state).contains("news desk"),
        "the picker closed"
    );
    assert_eq!(selected_row(&state), 1);
    reply_times(&mut state, &request_id, &["08:30", "13:00", "19:00"]);
    let text = settings_frame_text(&mut state);
    // (`00:00-08:00` in the quiet-hours row is not a time row.)
    assert!(
        text.contains("08:30") && !text.contains("  08:00 "),
        "{text}"
    );

    // The `add time` row (after the three times) opens the picker at 12:00;
    // Enter appends and the list comes back sorted.
    for _ in 0..3 {
        state.handle_input_bytes(b"\x1b[B");
    }
    assert_eq!(selected_row(&state), 4);
    state.handle_input_bytes(b"\r");
    let text = settings_frame_text(&mut state);
    assert!(text.contains("add a time"), "{text}");
    assert!(
        text.contains("▸ 12:00"),
        "the cursor starts at noon: {text}"
    );
    let outcome = state.handle_input_bytes(b"\r");
    let (request_id, times) = set_times_request(&outcome);
    assert_eq!(times, ["08:30", "12:00", "13:00", "19:00"]);
    reply_times(
        &mut state,
        &request_id,
        &["08:30", "12:00", "13:00", "19:00"],
    );
    let text = settings_frame_text(&mut state);
    assert!(text.contains("12:00"), "{text}");

    // Delete on a time row removes it; Backspace too.
    state.handle_input_bytes(b"\x1b[A");
    assert_eq!(selected_row(&state), 3, "the 13:00 row");
    let outcome = state.handle_input_bytes(b"\x1b[3~");
    let (request_id, times) = set_times_request(&outcome);
    assert_eq!(times, ["08:30", "12:00", "19:00"]);
    reply_times(&mut state, &request_id, &["08:30", "12:00", "19:00"]);
    let text = settings_frame_text(&mut state);
    assert!(!text.contains("13:00"), "{text}");
    assert_eq!(selected_row(&state), 3, "now the 19:00 row");
    let outcome = state.handle_input_bytes(b"\x7f");
    let (request_id, times) = set_times_request(&outcome);
    assert_eq!(times, ["08:30", "12:00"]);
    reply_times(&mut state, &request_id, &["08:30", "12:00"]);
    assert_eq!(
        selected_row(&state),
        3,
        "the add row, clamped into the list"
    );
    let text = settings_frame_text(&mut state);
    assert!(text.contains("▸ add time"), "{text}");
    assert!(
        endpoint_requests(&state.handle_input_bytes(b"\x1b[3~")).is_empty(),
        "Delete does nothing off a time row"
    );

    // Quiet hours (after `add time`): the picker writes the local config.
    state.handle_input_bytes(b"\x1b[B");
    state.handle_input_bytes(b"\r");
    let text = settings_frame_text(&mut state);
    assert!(text.contains("quiet hours"), "{text}");
    assert!(text.contains("notification waits"), "{text}");
    assert!(text.contains("off"), "{text}");
    assert!(text.contains("00:00-08:00 ✓"), "{text}");
    state.handle_input_bytes(b"\x1b[A");
    let outcome = state.handle_input_bytes(b"\r");
    assert!(endpoint_requests(&outcome)
        .iter()
        .any(|(_, method)| matches!(method, Method::ServerReloadConfig(_))));
    assert_eq!(written().news.quiet_hours, "");
    assert_eq!(selected_row(&state), 4, "back on the quiet-hours row");

    // Run now, the last row.
    state.handle_input_bytes(b"\x1b[B");
    let outcome = state.handle_input_bytes(b"\r");
    assert!(matches!(
        &endpoint_requests(&outcome)[..],
        [(_, Method::NewsRun(_))]
    ));

    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);
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
