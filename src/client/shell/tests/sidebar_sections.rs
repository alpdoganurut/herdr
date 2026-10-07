//! Fork (sidebar v3): the tabs sidebar's current row and the Pinned and
//! Scheduled blocks under the list.
//!
//! The fixture is sidebar_active.rs's (a blocked, a live voice, two
//! working, a finished, a parked and an idle tab over the ungrouped bucket
//! and the groups Alpha and Beta, `t_focus` focused). Assertions read the
//! blocks' own hit rects so they hold however the plan places them.

use std::time::{Duration, Instant};

use super::super::sidebar_model::{ModelInputs, SidebarHover, SidebarModel};
use super::sidebar_active::{
    active_snapshot, active_state, active_state_with, cell, detail_text, endpoint_methods, mouse,
    row_text, tabs_config, COLS, ROWS,
};
use super::*;
use crate::api::schema::{Method, TabRemindInterval};
use crate::server::headless::tab_pins::TabPinsPayload;
use crossterm::event::{MouseButton, MouseEventKind};

const MINUTE: Duration = Duration::from_secs(60);

/// Where the fixed rows (Browser, News, coordinator) start: under the list
/// and the Active, Pinned and Scheduled blocks.
pub(super) fn blocks_bottom(hits: &ShellHitMap) -> u16 {
    let last = |rows: &[(Rect, String)]| rows.last().map_or(0, |(rect, _)| rect.bottom());
    [
        hits.agent_body.bottom(),
        hits.sidebar_active_header.bottom(),
        last(&hits.sidebar_active_rows),
        hits.sidebar_active_more.bottom(),
        hits.sidebar_pins_header.bottom(),
        last(&hits.sidebar_pins_rows),
        hits.sidebar_pins_more.bottom(),
        hits.sidebar_scheduled_header.bottom(),
        last(&hits.sidebar_scheduled_rows),
        hits.sidebar_scheduled_more.bottom(),
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
}

fn pins_payload(revision: u64, tab_ids: &[&str]) -> TabPinsPayload {
    TabPinsPayload {
        boot_id: "boot-1".into(),
        revision,
        tab_ids: tab_ids.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn ids(rows: &[(Rect, String)]) -> Vec<&str> {
    rows.iter().map(|(_, id)| id.as_str()).collect()
}

fn row_of(rows: &[(Rect, String)], id: &str) -> Rect {
    rows.iter()
        .find(|(_, row_id)| row_id == id)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("a row for {id}"))
}

fn pinned_state() -> ClientShellState {
    let mut state = active_state();
    assert!(state.receive_tab_pins(
        &ClientEndpointId::Local,
        pins_payload(1, &["t_done", "t_home"])
    ));
    state
}

/// The fixture with scheduled reminders: `t_home` every 5m (ungrouped),
/// `t_work` every 10m, `t_done` daily; the reminder clocks started at `t0`
/// at 08:00 local time (before the default daily time).
fn scheduled_state_with(config: ClientShellConfig, t0: Instant) -> ClientShellState {
    let mut state = active_state_with(config);
    state.set_snapshot(Box::new(scheduled_snapshot()));
    state.reminder_local_time = Some(time::PrimitiveDateTime::new(
        time::Date::from_calendar_date(2026, time::Month::October, 7).expect("date"),
        time::Time::from_hms(8, 0, 0).expect("time"),
    ));
    state.tick_notifications(t0);
    state
}

fn scheduled_snapshot() -> ClientShellSnapshot {
    let mut snapshot = active_snapshot();
    for tab in &mut snapshot.tabs {
        tab.remind_every = match tab.tab_id.as_str() {
            "t_home" => Some(TabRemindInterval::M5),
            "t_work" => Some(TabRemindInterval::M10),
            "t_done" => Some(TabRemindInterval::Daily),
            _ => None,
        };
    }
    snapshot
}

fn scheduled_state(t0: Instant) -> ClientShellState {
    scheduled_state_with(ClientShellConfig::from_config(&tabs_config()), t0)
}

#[test]
fn pins_section_lists_pinned_tabs_in_list_order_and_they_stay_in_their_group() {
    let mut state = pinned_state();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        ids(&state.hits.sidebar_pins_rows),
        ["t_home", "t_done"],
        "snapshot order"
    );
    for id in ["t_home", "t_done"] {
        assert!(
            state.hits.sidebar_tabs.iter().any(|(_, row)| row == id),
            "{id} stays in its group"
        );
    }
    let header = state.hits.sidebar_pins_header;
    let text = row_text(&frame, header);
    assert!(text.contains("Pinned"), "{text:?}");
    assert_eq!(text.trim_end().chars().last(), Some('2'), "{text:?}");
    // Under the list and the Active block, its rule on the row above.
    assert!(header.y > state.hits.sidebar_active_header.y);
    assert!(header.y > state.hits.agent_body.bottom());
    assert_eq!(cell(&frame, header.x + 2, header.y - 1).symbol, "\u{2500}");
    // An entry: the status icon, the label, then its group.
    let done = row_text(&frame, row_of(&state.hits.sidebar_pins_rows, "t_done"));
    assert!(
        done.contains("finished") && done.contains("Beta"),
        "{done:?}"
    );
}

#[test]
fn a_team_group_entry_names_the_team() {
    let mut state = pinned_state();
    let teams = crate::server::headless::teams::TeamsPayload::decode(
        r#"{"boot_id":"boot-1","revision":1,"teams":[{"workspace_id":"ws_b","purpose":"ship it","members":[]}]}"#,
    )
    .expect("teams payload");
    assert!(state.receive_teams(&ClientEndpointId::Local, teams));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let row = row_of(&state.hits.sidebar_pins_rows, "t_done");
    let done = row_text(&frame, row);
    assert!(done.contains("\u{25C6} ship it"), "{done:?}");
    let mark_x = (row.x..row.right())
        .find(|x| cell(&frame, *x, row.y).symbol == "\u{25C6}")
        .expect("the team mark");
    assert_eq!(
        cell(&frame, mark_x, row.y).fg,
        crate::protocol::color_to_u32(state.config.palette.accent)
    );
    // The current row names the focused tab's plain group.
    let current = row_text(&frame, state.hits.sidebar_current);
    assert!(current.contains("Alpha"), "{current:?}");
}

#[test]
fn scheduled_section_orders_by_interval_and_shows_countdown_daily_time_and_fired() {
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    state.sidebar_clock = Some(t0 + Duration::from_secs(90));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        ids(&state.hits.sidebar_scheduled_rows),
        ["t_home", "t_work", "t_done"],
        "5m, 10m, daily"
    );
    let text = |state: &ClientShellState, frame: &FrameData, id: &str| {
        row_text(frame, row_of(&state.hits.sidebar_scheduled_rows, id))
    };
    let home = text(&state, &frame, "t_home");
    assert!(
        home.contains("\u{25F7}") && home.contains("5m") && home.contains("in 4m"),
        "{home:?}"
    );
    let work = text(&state, &frame, "t_work");
    assert!(work.contains("10m") && work.contains("in 9m"), "{work:?}");
    let minutes = state.config.daily_reminder_minutes;
    let daily = format!("{:02}:{:02}", minutes / 60, minutes % 60);
    let done = text(&state, &frame, "t_done");
    assert!(
        done.contains("\u{263C}") && done.contains("daily") && done.contains(&daily),
        "{done:?}"
    );
    let header = row_text(&frame, state.hits.sidebar_scheduled_header);
    assert!(header.contains("Scheduled"), "{header:?}");
    assert!(
        state.hits.sidebar_scheduled_header.y > state.hits.agent_body.bottom(),
        "under the list"
    );

    // The 5m reminder fires: its row says so, the marker is lit.
    state.tick_notifications(t0 + 5 * MINUTE);
    state.sidebar_clock = Some(t0 + 5 * MINUTE + Duration::from_secs(5));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let home_row = row_of(&state.hits.sidebar_scheduled_rows, "t_home");
    let home = row_text(&frame, home_row);
    assert!(
        home.contains(super::super::tab_sidebar_scheduled::FIRED),
        "{home:?}"
    );
    let lit =
        super::super::idle_reminders::ClientReminderLit::Scheduled.color(&state.config.palette);
    assert_eq!(
        cell(&frame, home_row.x + 3, home_row.y).fg,
        crate::protocol::color_to_u32(lit)
    );
}

#[test]
fn fired_is_not_shown_for_the_focused_tab() {
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    state.tick_notifications(t0 + 5 * MINUTE);
    // Focus moves to the lit tab; the reminder's own tick has not run yet.
    let mut focused = scheduled_snapshot();
    for tab in &mut focused.tabs {
        tab.focused = tab.tab_id == "t_home";
    }
    focused.focused_tab_id = Some("t_home".into());
    focused.focused_workspace_id = Some("ws_home".into());
    state.set_snapshot(Box::new(focused));
    state.sidebar_clock = Some(t0 + 5 * MINUTE + Duration::from_secs(1));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let home = row_text(&frame, row_of(&state.hits.sidebar_scheduled_rows, "t_home"));
    assert!(
        !home.contains(super::super::tab_sidebar_scheduled::FIRED),
        "{home:?}"
    );
}

#[test]
fn countdown_sets_the_clock_deadline_at_the_next_text_change() {
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    state.sidebar_clock = Some(t0 + Duration::from_secs(90));
    state.compose(COLS, ROWS).expect("composed frame");
    // 5m: 3m30s left shows "in 4m" until 3m00s are left, at t0 + 2m; 10m:
    // 8m30s left shows "in 9m" until t0 + 2m as well.
    assert_eq!(
        state.hits.sidebar_clock_deadline,
        Some(t0 + 2 * MINUTE),
        "the earliest text change"
    );
}

#[test]
fn countdowns_round_up_to_the_minute() {
    use super::super::sidebar_model::{format_countdown, next_countdown_tick};
    let left = |secs: u64| format_countdown(Duration::from_secs(secs));
    assert_eq!(left(240).as_str(), "in 4m");
    assert_eq!(left(241).as_str(), "in 5m");
    assert_eq!(left(1).as_str(), "in 1m");
    assert_eq!(left(0).as_str(), "in 1m");
    assert_eq!(left(3600 + 5 * 60).as_str(), "in 1h05");
    let now = Instant::now();
    let fire = now + Duration::from_secs(240);
    assert_eq!(
        next_countdown_tick(fire, now),
        Some(fire - 3 * MINUTE),
        "4m00s left: the text changes when 3m00s are left"
    );
    assert_eq!(
        next_countdown_tick(now + Duration::from_secs(30), now),
        Some(now + Duration::from_secs(30)),
        "the last minute ticks at the firing"
    );
    assert_eq!(next_countdown_tick(now, now), None, "due: no deadline");
}

#[test]
fn sections_hide_when_empty_and_when_disabled() {
    let mut state = active_state();
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(state.hits.sidebar_pins_header.is_empty(), "nothing pinned");
    assert!(
        state.hits.sidebar_scheduled_header.is_empty(),
        "nothing scheduled"
    );

    let mut config = tabs_config();
    config.ui.sidebar_pinned_agents = false;
    config.ui.sidebar_scheduled_agents = false;
    let t0 = Instant::now();
    let mut state = scheduled_state_with(ClientShellConfig::from_config(&config), t0);
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(state.hits.sidebar_pins_header.is_empty(), "the opt-out");
    assert!(
        state.hits.sidebar_scheduled_header.is_empty(),
        "the opt-out"
    );
    assert!(
        !state.hits.sidebar_active_header.is_empty(),
        "Active keeps its own toggle"
    );
}

#[test]
fn section_headers_fold_and_the_fold_is_persisted() {
    let path = std::env::temp_dir().join(format!(
        "herdr-sidebar-sections-preferences-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let t0 = Instant::now();
    let mut state = scheduled_state_with(
        ClientShellConfig::from_config(&tabs_config()).with_preferences_path(path.clone()),
        t0,
    );
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
    state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_pins_header;
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        header.x + 4,
        header.y,
    );
    assert!(outcome.repaint);
    assert_eq!(state.pinned_agents_folded, Some(true));
    state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_scheduled_header;
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        header.x + 4,
        header.y,
    );
    assert_eq!(state.scheduled_agents_folded, Some(true));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(state.hits.sidebar_pins_rows.is_empty(), "folded");
    assert!(state.hits.sidebar_scheduled_rows.is_empty(), "folded");
    let header = state.hits.sidebar_scheduled_header;
    assert_eq!(cell(&frame, header.x + 1, header.y).symbol, "\u{25B8}");
    // Folded, Scheduled shows one marker per interval kind present.
    let text = row_text(&frame, header);
    assert!(
        text.contains("\u{25F7}") && text.contains("\u{263C}"),
        "{text:?}"
    );
    let saved = preferences::load(&path).expect("saved");
    assert_eq!(saved.pinned_agents_folded, Some(true));
    assert_eq!(saved.scheduled_agents_folded, Some(true));
    let restored = ClientShellState::new(
        ClientShellConfig::from_config(&tabs_config()).with_preferences_path(path.clone()),
    );
    assert_eq!(restored.pinned_agents_folded, Some(true));
    assert_eq!(restored.scheduled_agents_folded, Some(true));
    std::fs::remove_file(path).expect("remove preferences");
}

#[test]
fn a_click_on_a_section_entry_focuses_the_tab_and_reveals_it() {
    let mut state = pinned_state();
    state.collapsed_groups.insert(group_key("ws_b"));
    state.sidebar_model.mark_dirty();
    state.compose(COLS, ROWS).expect("composed frame");
    let row = row_of(&state.hits.sidebar_pins_rows, "t_done");
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        row.x + 6,
        row.y,
    );
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabFocus(target)] if target.tab_id == "t_done"
    ));
    assert!(!state.collapsed_groups.contains(&group_key("ws_b")));
    assert_eq!(state.sidebar_reveal_tab.as_deref(), Some("t_done"));
    assert!(state.tab_press.is_none(), "an entry never starts a drag");

    // A scheduled entry likewise.
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    state.compose(COLS, ROWS).expect("composed frame");
    let row = row_of(&state.hits.sidebar_scheduled_rows, "t_work");
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        row.x + 6,
        row.y,
    );
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabFocus(target)] if target.tab_id == "t_work"
    ));
    // A right press opens the tab's menu.
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 6,
        row.y,
    );
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab { ref tab_id, .. },
            ..
        })) if tab_id == "t_work"
    ));
}

fn open_menu_on(state: &mut ClientShellState, tab_id: &str) -> Vec<ClientContextMenuItem> {
    state.overlay = None;
    state.compose(COLS, ROWS).expect("composed frame");
    let row = row_of(&state.hits.sidebar_tabs, tab_id);
    mouse(
        state,
        MouseEventKind::Down(MouseButton::Right),
        row.x + 8,
        row.y,
    );
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu.items(),
        _ => panic!("tab context menu"),
    }
}

#[test]
fn pin_menu_item_toggles_through_tab_set_pinned_and_hides_without_server_support() {
    let mut state = active_state();
    let items = open_menu_on(&mut state, "t_work");
    let index = items
        .iter()
        .position(|item| item.label == "Pin")
        .expect("a Pin item");
    let important = items
        .iter()
        .position(|item| item.action == ClientContextMenuAction::Important)
        .expect("Important");
    assert_eq!(index, important + 1, "right after Important");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetPinned(params)] if params.tab_id == "t_work" && params.pinned
    ));
    assert!(
        !endpoint_methods(&outcome)
            .iter()
            .any(|method| matches!(method, Method::TabFocus(_))),
        "pinning focuses nothing"
    );

    // Once the push lists it, the item unpins.
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_work"])));
    let items = open_menu_on(&mut state, "t_work");
    let index = items
        .iter()
        .position(|item| item.label == "Unpin")
        .expect("an Unpin item");
    let mut outcome = ClientShellInput::default();
    state.activate_context_menu_item(index, &mut outcome);
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [Method::TabSetPinned(params)] if params.tab_id == "t_work" && !params.pinned
    ));

    // A server without `tab.set_pinned`: no item.
    state.set_endpoint_methods(Some(vec!["tab.focus".into(), "tab.set_reminder".into()]));
    let items = open_menu_on(&mut state, "t_work");
    assert!(
        !items
            .iter()
            .any(|item| item.action == ClientContextMenuAction::Pin),
        "{:?}",
        items.iter().map(|item| item.label).collect::<Vec<_>>()
    );
}

#[test]
fn tab_pins_state_is_dropped_when_the_boot_changes() {
    let mut state = active_state();
    let mut other = pins_payload(5, &["t_done"]);
    other.boot_id = "boot-other".into();
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, other));
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(
        state.hits.sidebar_pins_rows.is_empty(),
        "another server's pins never show"
    );
    // The active server's push replaces it, older revisions do not.
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
    assert!(!state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &[])));
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(ids(&state.hits.sidebar_pins_rows), ["t_done"]);
}

#[test]
fn sections_rebuild_the_model_on_data_change_only() {
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
    state.sidebar_clock = Some(t0 + Duration::from_secs(90));
    for _ in 0..5 {
        state.compose(COLS, ROWS).expect("composed frame");
    }
    assert!(!state.hits.sidebar_pins_rows.is_empty());
    assert!(!state.hits.sidebar_scheduled_rows.is_empty());
    let builds = state.sidebar_model.builds;
    assert_eq!(builds, 1, "static frames reuse the model");
    assert!(state.receive_tab_pins(
        &ClientEndpointId::Local,
        pins_payload(2, &["t_done", "t_work"])
    ));
    state.compose(COLS, ROWS).expect("composed frame");
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        state.sidebar_model.builds,
        builds + 1,
        "a pins push rebuilds once"
    );
    // The countdown moves (and a reminder fires) without a rebuild.
    state.tick_notifications(t0 + 5 * MINUTE);
    state.sidebar_clock = Some(t0 + 6 * MINUTE);
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        state.sidebar_model.builds,
        builds + 1,
        "a clock tick never rebuilds"
    );
}

#[test]
fn current_row_follows_focus_and_the_group_row_stays_put() {
    let mut state = active_state();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let current = state.hits.sidebar_current;
    assert_eq!(current.y, 1, "right under the toolbar");
    assert_eq!(state.hits.agent_body.y, 2, "the list starts under it");
    let text = row_text(&frame, current);
    assert!(
        text.contains("\u{203A}") && text.contains("focus work") && text.contains("Alpha"),
        "{text:?}"
    );
    let rows_before = state.hits.sidebar_tabs.clone();
    assert!(
        rows_before.iter().any(|(_, id)| id == "t_focus"),
        "the focused tab keeps its group row"
    );

    let mut moved = active_snapshot();
    for tab in &mut moved.tabs {
        tab.focused = tab.tab_id == "t_work";
    }
    moved.focused_tab_id = Some("t_work".into());
    moved.focused_workspace_id = Some("ws_b".into());
    state.set_snapshot(Box::new(moved));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = row_text(&frame, state.hits.sidebar_current);
    assert!(
        text.contains("old work") && text.contains("Beta"),
        "{text:?}"
    );
    assert_eq!(
        state.hits.sidebar_tabs, rows_before,
        "no list row moved: only the current row follows focus"
    );

    // A click reveals the focused tab, without a focus request.
    let current = state.hits.sidebar_current;
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        current.x + 6,
        current.y,
    );
    assert!(endpoint_methods(&outcome).is_empty(), "already focused");
    assert_eq!(state.sidebar_reveal_tab.as_deref(), Some("t_work"));
    // Hovering it selects the focused tab for the detail strip.
    mouse(&mut state, MouseEventKind::Moved, current.x + 6, current.y);
    assert_eq!(state.sidebar_hover, Some(SidebarHover::Current));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(detail_text(&state, &frame)[0].contains("old work"));
}

#[test]
fn current_row_hides_without_a_focused_list_tab() {
    let snapshot = active_snapshot();
    let groups = HashSet::new();
    // The focused tab is a fixed row's (News or coordinator): no current row.
    let model = SidebarModel::built(ModelInputs {
        fixed_ids: (Some("t_focus"), None),
        ..ModelInputs::new(&snapshot, &groups)
    });
    assert_eq!(model.focused_tab, None);
    let mut state = active_state();
    let mut unfocused = active_snapshot();
    for tab in &mut unfocused.tabs {
        tab.focused = false;
    }
    unfocused.focused_tab_id = None;
    state.set_snapshot(Box::new(unfocused));
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(state.hits.sidebar_current.is_empty());
    assert_eq!(
        state.hits.agent_body.y, 1,
        "the list starts under the toolbar"
    );
}

#[test]
fn active_block_sits_under_the_list_with_its_rule_on_top() {
    let mut state = active_state();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_active_header;
    assert_eq!(header.y, state.hits.agent_body.bottom() + 1);
    assert_eq!(cell(&frame, header.x + 2, header.y - 1).symbol, "\u{2500}");
    assert_eq!(
        state.hits.sidebar_active_rows[0].0.y,
        header.y + 1,
        "entries under the header"
    );
}

#[test]
fn section_headers_describe_themselves_in_the_detail_strip() {
    let t0 = Instant::now();
    let mut state = scheduled_state(t0);
    assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
    state.sidebar_clock = Some(t0 + Duration::from_secs(90));
    state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_pins_header;
    mouse(&mut state, MouseEventKind::Moved, header.x + 4, header.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame).join(" ");
    assert!(text.contains("1 pinned"), "{text:?}");
    let header = state.hits.sidebar_scheduled_header;
    mouse(&mut state, MouseEventKind::Moved, header.x + 4, header.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame).join(" ");
    assert!(text.contains("3 scheduled"), "{text:?}");
    assert!(text.contains("next scratch in 4m"), "{text:?}");
    // A pinned tab's own facts say so.
    let entry = row_of(&state.hits.sidebar_pins_rows, "t_done");
    mouse(&mut state, MouseEventKind::Moved, entry.x + 6, entry.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame).join(" ");
    assert!(text.contains("pinned"), "{text:?}");
}

/// Fork smoke tests: FORK.md section 10 lists them by name and the sync gate
/// runs them with `-E 'test(fork_smoke)'`.
mod fork_smoke {
    use super::*;

    /// The current row, the Active block under the list and the Pinned and
    /// Scheduled blocks under it all reach the composed frame.
    #[test]
    fn current_row_and_bottom_blocks_reach_the_renderer() {
        let t0 = Instant::now();
        let mut state = scheduled_state(t0);
        assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
        let frame = state.compose(COLS, ROWS).expect("composed frame");
        let hits = &state.hits;
        assert_eq!(hits.sidebar_current.y, 1);
        let list = hits.agent_body;
        assert!(hits.sidebar_active_header.y > list.bottom());
        assert!(hits.sidebar_pins_header.y > hits.sidebar_active_header.y);
        assert!(hits.sidebar_scheduled_header.y > hits.sidebar_pins_header.y);
        assert!(row_text(&frame, hits.sidebar_current).contains("focus work"));
    }

    #[test]
    fn pinned_and_scheduled_sections_reach_the_renderer() {
        let t0 = Instant::now();
        let mut state = scheduled_state(t0);
        assert!(state.receive_tab_pins(&ClientEndpointId::Local, pins_payload(1, &["t_done"])));
        state.sidebar_clock = Some(t0 + Duration::from_secs(90));
        let frame = state.compose(COLS, ROWS).expect("composed frame");
        assert_eq!(ids(&state.hits.sidebar_pins_rows), ["t_done"]);
        assert_eq!(
            ids(&state.hits.sidebar_scheduled_rows),
            ["t_home", "t_work", "t_done"]
        );
        assert!(row_text(&frame, state.hits.sidebar_pins_header).contains("Pinned"));
        assert!(row_text(&frame, state.hits.sidebar_scheduled_header).contains("Scheduled"));
        let home = row_text(&frame, row_of(&state.hits.sidebar_scheduled_rows, "t_home"));
        assert!(home.contains("in 4m"), "{home:?}");
    }
}
