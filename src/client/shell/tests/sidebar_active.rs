//! The tabs sidebar's Active block, detail strip and derived model
//! (sidebar v2).
//!
//! The fixture has a blocked tab, a tab in live voice mode, two working tabs
//! (the older state change first), a finished tab and a parked one, spread
//! over the ungrouped bucket and two groups. Assertions read the block's
//! own hit rects (`sidebar_active_*`, `sidebar_detail`) so they hold however
//! the layout plan places the block.

use std::time::{Duration, Instant};

use super::super::sidebar_model::{SidebarHover, SidebarModel};
use super::*;
use crate::api::schema::AgentVoiceMode;
use crate::config::{Config, SidebarLayoutConfig};
use crate::protocol::ClientShellWorkspace;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 106;
const ROWS: u16 = 45;

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_width = 31;
    config
}

fn tabs_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&tabs_config()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn workspace(id: &str, label: &str, status: AgentStatus) -> ClientShellWorkspace {
    ClientShellWorkspace {
        workspace_id: id.into(),
        active_tab_id: String::new(),
        new_workspace_cwd: "/repo".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        branch: None,
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused: false,
        agent_status: status,
    }
}

fn tab(id: &str, workspace_id: &str, label: &str, status: AgentStatus) -> ClientShellTab {
    ClientShellTab {
        tab_id: id.into(),
        workspace_id: workspace_id.into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        zoomed: false,
        focused: false,
        agent_status: status,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn pane(pane_id: &str, workspace_id: &str, tab_id: &str) -> ClientShellPane {
    ClientShellPane {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        label: None,
        cwd: Some("/repo".into()),
        foreground_cwd: Some("/repo".into()),
        focused: false,
        right_click_passthrough: false,
    }
}

fn agent(
    pane_id: &str,
    workspace_id: &str,
    tab_id: &str,
    status: AgentStatus,
    seq: u64,
) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: seq,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// (tab, space, label, status, seq): one pane and agent per tab.
const FIXTURE: [(&str, &str, &str, AgentStatus, u64); 7] = [
    ("t_home", "ws_home", "scratch", AgentStatus::Idle, 1),
    ("t_focus", "ws_a", "focus work", AgentStatus::Working, 5),
    ("t_block", "ws_a", "needs input", AgentStatus::Blocked, 7),
    ("t_voice", "ws_b", "talking", AgentStatus::Idle, 2),
    ("t_work", "ws_b", "old work", AgentStatus::Working, 3),
    ("t_done", "ws_b", "finished", AgentStatus::Done, 9),
    ("t_park", "ws_b", "parked", AgentStatus::Suspended, 4),
];

fn pane_of(tab_id: &str) -> String {
    tab_id.replacen("t_", "p_", 1)
}

fn active_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.focused_workspace_id = Some("ws_a".into());
    snapshot.focused_tab_id = Some("t_focus".into());
    snapshot.focused_pane_id = Some("p_focus".into());
    snapshot.workspaces = vec![
        workspace("ws_home", "home", AgentStatus::Idle),
        workspace("ws_a", "Alpha", AgentStatus::Blocked),
        workspace("ws_b", "Beta", AgentStatus::Working),
    ];
    snapshot.workspaces[1].focused = true;
    snapshot.tabs = FIXTURE
        .iter()
        .map(|(id, ws, label, status, _)| tab(id, ws, label, *status))
        .collect();
    snapshot.tabs[1].focused = true;
    snapshot.panes = FIXTURE
        .iter()
        .map(|(id, ws, _, _, _)| pane(&pane_of(id), ws, id))
        .collect();
    snapshot.agents = FIXTURE
        .iter()
        .map(|(id, ws, _, status, seq)| agent(&pane_of(id), ws, id, *status, *seq))
        .collect();
    snapshot
}

fn voice_payload(revision: u64, live: bool) -> crate::server::headless::voice::VoicePayload {
    let panes = if live {
        serde_json::json!([{ "pane_id": "p_voice", "voice": "live" }])
    } else {
        serde_json::json!([])
    };
    serde_json::from_value(serde_json::json!({
        "boot_id": "boot-1",
        "revision": revision,
        "panes": panes,
    }))
    .expect("voice payload")
}

/// The fixture installed so the client projects `t_done` as Done (it saw
/// the turn working), with `p_voice` in live voice mode.
fn active_state_with(config: ClientShellConfig) -> ClientShellState {
    let mut state = ClientShellState::new(config);
    let mut working = active_snapshot();
    for agent in &mut working.agents {
        if agent.pane_id == "p_done" {
            agent.agent_status = AgentStatus::Working;
            agent.state_change_seq = 8;
        }
    }
    state.set_snapshot(Box::new(working));
    state.set_snapshot(Box::new(active_snapshot()));
    state.set_pane_surface(surface());
    assert!(state.receive_voice(&ClientEndpointId::Local, voice_payload(1, true)));
    state
}

fn active_state() -> ClientShellState {
    active_state_with(ClientShellConfig::from_config(&tabs_config()))
}

/// An agent times push measured at a server clock of 10^10 ms: (pane, seq,
/// age in ms).
fn times_payload(
    revision: u64,
    panes: &[(&str, u64, u64)],
) -> crate::server::headless::agent_times::AgentTimesPayload {
    crate::server::headless::agent_times::AgentTimesPayload {
        boot_id: "boot-1".into(),
        revision,
        server_now_unix_ms: 10_000_000_000,
        panes: panes
            .iter()
            .map(
                |(pane_id, seq, age_ms)| crate::app::agent_times::AgentTimePane {
                    pane_id: (*pane_id).into(),
                    state_change_seq: *seq,
                    since_unix_ms: 10_000_000_000 - age_ms,
                },
            )
            .collect(),
    }
}

fn active_ids(state: &ClientShellState) -> Vec<&str> {
    state
        .hits
        .sidebar_active_rows
        .iter()
        .map(|(_, id)| id.as_str())
        .collect()
}

fn row_text(frame: &FrameData, rect: Rect) -> String {
    (rect.x..rect.right())
        .map(|x| cell(frame, x, rect.y).symbol.as_str())
        .collect()
}

fn cell(frame: &FrameData, x: u16, y: u16) -> &crate::protocol::CellData {
    &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)]
}

fn mouse(state: &mut ClientShellState, kind: MouseEventKind, x: u16, y: u16) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn endpoint_methods(outcome: &ClientShellInput) -> Vec<&crate::api::schema::Method> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => Some(&request.method),
            _ => None,
        })
        .collect()
}

/// The detail strip's text rows (its rule rows left out).
fn detail_text(state: &ClientShellState, frame: &FrameData) -> Vec<String> {
    let strip = state.hits.sidebar_detail;
    assert!(strip.height >= 2, "a detail strip: {strip:?}");
    (strip.y + 1..strip.bottom())
        .map(|y| row_text(frame, Rect::new(strip.x, y, strip.width, 1)))
        .filter(|text| !text.contains("\u{2500}\u{2500}"))
        .collect()
}

fn list_row(state: &ClientShellState, rows: &[(Rect, String)], id: &str) -> Rect {
    let _ = state;
    rows.iter()
        .find(|(_, row_id)| row_id == id)
        .map(|(rect, _)| *rect)
        .unwrap_or_else(|| panic!("a row for {id}"))
}

#[test]
fn sidebar_model_builds_once_until_the_snapshot_changes() {
    let mut state = tabs_state();
    for _ in 0..3 {
        state.compose(106, 30).expect("composed frame");
    }
    assert_eq!(
        state.sidebar_model.builds, 1,
        "static frames reuse the model"
    );
    state.set_snapshot(Box::new(snapshot()));
    state.compose(106, 30).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 2, "a new snapshot rebuilds it");
}

#[test]
fn model_rebuilds_on_data_change_not_per_compose() {
    let mut state = active_state();
    for _ in 0..5 {
        state.compose(COLS, ROWS).expect("composed frame");
    }
    assert_eq!(state.sidebar_model.builds, 1);
    state.set_snapshot(Box::new(active_snapshot()));
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 2, "a snapshot rebuilds once");
    assert!(state.receive_voice(&ClientEndpointId::Local, voice_payload(2, true)));
    state.compose(COLS, ROWS).expect("composed frame");
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 3, "a voice push rebuilds once");
    assert!(state.receive_agent_times(&ClientEndpointId::Local, times_payload(1, &[])));
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 4, "a times push rebuilds once");
    assert!(
        !state.receive_agent_times(&ClientEndpointId::Local, times_payload(1, &[])),
        "an equal revision is ignored"
    );
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 4);
    // Hovering repaints but never rebuilds.
    let (rect, _) = state.hits.sidebar_active_rows[0];
    mouse(&mut state, MouseEventKind::Moved, rect.x + 4, rect.y);
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_model.builds, 4);
}

#[test]
fn active_block_orders_blocked_voice_working_done_by_seq() {
    let mut state = active_state();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        active_ids(&state),
        ["t_block", "t_voice", "t_work", "t_focus", "t_done"],
        "blocked, live voice, working (older change first), done; idle and parked tabs are not listed"
    );
    let header = state.hits.sidebar_active_header;
    assert_eq!(header.y, 1, "right under the toolbar");
    let text = row_text(&frame, header);
    assert!(
        text.contains("Active") && !text.contains("agents"),
        "titled Active: {text:?}"
    );
    assert_eq!(
        text.trim_end().chars().last(),
        Some('5'),
        "the entry count: {text:?}"
    );
    // The first entry: the status icon at x = 3 in red, the name bold, the
    // group's label after it.
    let (row, _) = state.hits.sidebar_active_rows[0];
    let text = row_text(&frame, row);
    assert!(text.contains("needs input Alpha"), "{text:?}");
    let palette = &state.config.palette;
    assert_eq!(
        cell(&frame, row.x + 3, row.y).fg,
        crate::protocol::color_to_u32(palette.red)
    );
    let name = cell(&frame, row.x + 5, row.y);
    assert_ne!(name.modifier & ratatui::style::Modifier::BOLD.bits(), 0);
    // The voice entry shows the mic at x = 5 and the name from x = 7.
    let (row, _) = state.hits.sidebar_active_rows[1];
    let (mark, color) = super::super::voice::voice_mark(AgentVoiceMode::Live, &state.config);
    let mic = cell(&frame, row.x + 5, row.y);
    assert_eq!(mic.symbol, mark);
    assert_eq!(mic.fg, crate::protocol::color_to_u32(color));
    assert_eq!(cell(&frame, row.x + 7, row.y).symbol, "t");
    // The hairline rule closes the block; the list starts under it.
    let rule = Rect::new(header.x, header.y + 6, header.width, 1);
    assert!(row_text(&frame, rule).contains("\u{2500}\u{2500}\u{2500}"));
    assert_eq!(
        cell(&frame, rule.x + 1, rule.y).fg,
        crate::protocol::color_to_u32(palette.surface1)
    );
    assert!(state.hits.agent_body.y > rule.y);
}

/// `t_home` idle with two subagents running (since eb0979d4 background
/// work alone reads as idle).
fn subagents_snapshot() -> ClientShellSnapshot {
    let mut snapshot = active_snapshot();
    for agent in &mut snapshot.agents {
        if agent.pane_id == "p_home" {
            agent.subagents = 2;
        }
    }
    snapshot
}

#[test]
fn active_block_lists_idle_tabs_running_subagents_after_working() {
    let mut state = active_state();
    state.set_snapshot(Box::new(subagents_snapshot()));
    let received = Instant::now() + Duration::from_secs(86_400);
    state.sidebar_clock = Some(received);
    // Idle for 12m (the idle agent's own seq), working for 42m.
    assert!(state.receive_agent_times_at(
        &ClientEndpointId::Local,
        times_payload(
            1,
            &[("p_home", 1, 12 * 60 * 1000), ("p_work", 3, 42 * 60 * 1000)]
        ),
        received,
    ));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        active_ids(&state),
        ["t_block", "t_voice", "t_work", "t_focus", "t_home", "t_done"],
        "after working, before done"
    );
    let header = state.hits.sidebar_active_header;
    assert_eq!(
        row_text(&frame, header).trim_end().chars().last(),
        Some('6'),
        "counted in the header"
    );
    // The entry: the subagent icon in the idle colour, `⚭2`, its idle time.
    let row = list_row(&state, &state.hits.sidebar_active_rows, "t_home");
    let icon = cell(&frame, row.x + 3, row.y);
    assert_eq!(icon.symbol, super::super::tab_sidebar::TAB_SUBAGENTS_ICON);
    assert_eq!(
        icon.fg,
        crate::protocol::color_to_u32(status_color(AgentStatus::Idle, &state.config.palette))
    );
    let text = row_text(&frame, row);
    assert!(text.contains("scratch"), "{text:?}");
    assert!(text.contains("\u{26AD}2"), "{text:?}");
    assert!(
        text.trim_end().ends_with("12m"),
        "time since idle: {text:?}"
    );
    // The voice entry (idle, no subagents) still shows no time.
    let row = list_row(&state, &state.hits.sidebar_active_rows, "t_voice");
    let text = row_text(&frame, row);
    assert!(!text.trim_end().ends_with('m'), "{text:?}");

    // Hovering the header counts the class.
    mouse(&mut state, MouseEventKind::Moved, header.x + 4, header.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let joined = detail_text(&state, &frame).join(" ");
    assert!(joined.contains("1 subagents"), "{joined:?}");

    // Folded: the subagent mark sits between working and done.
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        header.x + 4,
        header.y,
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_active_header;
    let last = header.right() - 2;
    let done = status_icon(AgentStatus::Done, state.config.status_indicators);
    let working = status_icon(AgentStatus::Working, state.config.status_indicators);
    assert_eq!(cell(&frame, last, header.y).symbol, done);
    assert_eq!(
        cell(&frame, last - 2, header.y).symbol,
        super::super::tab_sidebar::TAB_SUBAGENTS_ICON
    );
    assert_eq!(cell(&frame, last - 4, header.y).symbol, working);
}

#[test]
fn idle_tab_running_subagents_without_times_lists_without_a_duration() {
    let mut state = active_state();
    state.set_snapshot(Box::new(subagents_snapshot()));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(active_ids(&state).contains(&"t_home"));
    let row = list_row(&state, &state.hits.sidebar_active_rows, "t_home");
    let text = row_text(&frame, row);
    assert!(text.contains("\u{26AD}2"), "{text:?}");
    assert!(!text.trim_end().ends_with('m'), "no time known: {text:?}");
    assert_eq!(state.hits.sidebar_clock_deadline, None);

    // Its subagents finish: it leaves the block.
    state.set_snapshot(Box::new(active_snapshot()));
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(!active_ids(&state).contains(&"t_home"));
}

#[test]
fn pinned_tabs_never_listed() {
    let mut state = active_state();
    state.compose(COLS, ROWS).expect("composed frame");
    let snapshot = state.snapshot.as_deref().expect("snapshot");
    let model = SidebarModel::built(
        snapshot,
        &HashSet::new(),
        None,
        (Some("t_block"), Some("t_work")),
        true,
    );
    let listed: Vec<&str> = model
        .active
        .iter()
        .map(|entry| snapshot.tabs[entry.tab as usize].tab_id.as_str())
        .collect();
    assert_eq!(listed, ["t_focus", "t_done"]);
}

#[test]
fn active_block_is_hidden_when_nothing_is_active() {
    let mut state = tabs_state();
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.hits.sidebar_active_header, Rect::default());
    assert!(state.hits.sidebar_active_rows.is_empty());
    assert_eq!(
        state.hits.sidebar_tabs[0].0.y, 1,
        "the list starts right under the toolbar"
    );
}

#[test]
fn active_block_respects_the_opt_out() {
    let mut config = tabs_config();
    config.ui.sidebar_active_agents = false;
    let mut state = active_state_with(ClientShellConfig::from_config(&config));
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.hits.sidebar_active_header, Rect::default());
    assert!(state.hits.sidebar_active_rows.is_empty());
    assert!(state.sidebar_model.active.is_empty());
    assert_eq!(state.hits.agent_body.y, 1);
}

#[test]
fn active_block_caps_with_more_and_expands() {
    let mut state = active_state();
    // 25 rows: a cap of 4 lines, so 3 entries and `+2 more`.
    let frame = state.compose(COLS, 25).expect("composed frame");
    assert_eq!(active_ids(&state), ["t_block", "t_voice", "t_work"]);
    let more = state.hits.sidebar_active_more;
    assert!(row_text(&frame, more).contains("+2 more"));
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        more.x + 6,
        more.y,
    );
    assert!(outcome.repaint);
    assert!(endpoint_methods(&outcome).is_empty(), "no focus, no drag");
    assert!(state.active_agents_expanded);
    let frame = state.compose(COLS, 25).expect("composed frame");
    assert_eq!(active_ids(&state).len(), 5, "expanded shows every entry");
    let more = state.hits.sidebar_active_more;
    assert!(row_text(&frame, more).contains("show fewer"));
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        more.x + 6,
        more.y,
    );
    assert!(!state.active_agents_expanded);
    state.compose(COLS, 25).expect("composed frame");
    assert_eq!(active_ids(&state).len(), 3);
}

#[test]
fn active_header_click_folds_and_persists() {
    let path = std::env::temp_dir().join(format!(
        "herdr-sidebar-active-preferences-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut state = active_state_with(
        ClientShellConfig::from_config(&tabs_config()).with_preferences_path(path.clone()),
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let header = state.hits.sidebar_active_header;
    assert_eq!(cell(&frame, header.x + 1, header.y).symbol, "\u{25BE}");
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        header.x + 4,
        header.y,
    );
    assert!(outcome.repaint);
    assert_eq!(state.active_agents_folded, Some(true));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(
        state.hits.sidebar_active_rows.is_empty(),
        "folded to its header"
    );
    let header = state.hits.sidebar_active_header;
    assert_eq!(cell(&frame, header.x + 1, header.y).symbol, "\u{25B8}");
    // The folded header shows one mark per class present, the last at
    // cw - 2: blocked, the live mic, working, done.
    let (mic, _) = super::super::voice::voice_mark(AgentVoiceMode::Live, &state.config);
    let done = status_icon(AgentStatus::Done, state.config.status_indicators);
    let blocked = status_icon(AgentStatus::Blocked, state.config.status_indicators);
    let last = header.right() - 2;
    assert_eq!(cell(&frame, last, header.y).symbol, done);
    assert_eq!(
        cell(&frame, last, header.y).fg,
        crate::protocol::color_to_u32(state.config.palette.teal)
    );
    assert_eq!(cell(&frame, last - 4, header.y).symbol, mic);
    assert_eq!(cell(&frame, last - 6, header.y).symbol, blocked);
    assert_eq!(
        state.hits.agent_body.y,
        header.y + 2,
        "header and rule, then the list"
    );
    let saved = preferences::load(&path).expect("saved");
    assert_eq!(saved.active_agents_folded, Some(true));
    let restored = ClientShellState::new(
        ClientShellConfig::from_config(&tabs_config()).with_preferences_path(path.clone()),
    );
    assert_eq!(restored.active_agents_folded, Some(true));
    std::fs::remove_file(path).expect("remove preferences");
}

#[test]
fn active_entry_click_focuses_unfolds_and_reveals() {
    let mut state = active_state();
    state.collapsed_groups.insert(group_key("ws_b"));
    state.sidebar_model.mark_dirty();
    state.compose(COLS, ROWS).expect("composed frame");
    assert!(
        !state.hits.sidebar_tabs.iter().any(|(_, id)| id == "t_done"),
        "the folded group hides its tabs"
    );
    let row = list_row(&state, &state.hits.sidebar_active_rows, "t_done");
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        row.x + 6,
        row.y,
    );
    assert!(matches!(
        endpoint_methods(&outcome)[..],
        [crate::api::schema::Method::TabFocus(target)] if target.tab_id == "t_done"
    ));
    assert!(!state.collapsed_groups.contains(&group_key("ws_b")));
    assert_eq!(state.sidebar_reveal_tab.as_deref(), Some("t_done"));
    assert!(state.tab_press.is_none(), "an entry never starts a drag");
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(state.sidebar_reveal_tab, None, "taken by the list");
    assert!(state.hits.sidebar_tabs.iter().any(|(_, id)| id == "t_done"));

    // A right press opens the tab's menu.
    let (row, _) = state.hits.sidebar_active_rows[0];
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
        })) if tab_id == "t_block"
    ));
}

#[test]
fn durations_render_and_schedule_the_next_minute() {
    let mut state = active_state();
    let received = Instant::now() + Duration::from_secs(86_400);
    state.sidebar_clock = Some(received);
    // Blocked 3h05m10s, working 42m: keyed by the agents' seqs.
    let blocked_ms = (3 * 3600 + 5 * 60 + 10) * 1000;
    assert!(state.receive_agent_times_at(
        &ClientEndpointId::Local,
        times_payload(
            1,
            &[("p_block", 7, blocked_ms), ("p_work", 3, 42 * 60 * 1000)]
        ),
        received,
    ));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let (row, _) = state.hits.sidebar_active_rows[0];
    let text = row_text(&frame, row);
    assert!(text.trim_end().ends_with("3h05"), "{text:?}");
    assert_eq!(
        cell(&frame, row.right() - 2, row.y).fg,
        crate::protocol::color_to_u32(state.config.palette.red),
        "a blocked time is red"
    );
    let (row, _) = state.hits.sidebar_active_rows[2];
    let text = row_text(&frame, row);
    assert!(text.trim_end().ends_with("42m"), "{text:?}");
    let (row, _) = state.hits.sidebar_active_rows[3];
    let text = row_text(&frame, row);
    assert!(
        !text.trim_end().ends_with('m'),
        "no time for t_focus: {text:?}"
    );
    // The next repaint is when 3h05 turns 3h06: 50 s from now.
    assert_eq!(
        state.hits.sidebar_clock_deadline,
        Some(received + Duration::from_secs(50))
    );
    assert!(!state.tick_sidebar_clock(received + Duration::from_secs(49)));
    assert!(state.tick_sidebar_clock(received + Duration::from_secs(50)));
    state.sidebar_clock = Some(received + Duration::from_secs(50));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let (row, _) = state.hits.sidebar_active_rows[0];
    assert!(row_text(&frame, row).trim_end().ends_with("3h06"));
}

#[test]
fn stale_times_push_shows_no_duration() {
    let mut state = active_state();
    let received = Instant::now() + Duration::from_secs(86_400);
    state.sidebar_clock = Some(received);
    // The push lags the snapshot: its seqs are older.
    assert!(state.receive_agent_times_at(
        &ClientEndpointId::Local,
        times_payload(1, &[("p_block", 6, 600_000), ("p_work", 2, 600_000)]),
        received,
    ));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    for (row, id) in &state.hits.sidebar_active_rows {
        let text = row_text(&frame, *row);
        assert!(!text.contains("10m"), "{id}: {text:?}");
    }
    assert_eq!(state.hits.sidebar_clock_deadline, None, "nothing to tick");
}

#[test]
fn detail_strip_describes_hover_then_focused_tab() {
    let mut state = active_state();
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let lines = detail_text(&state, &frame);
    assert!(
        lines[0].contains("focus work"),
        "the focused tab: {lines:?}"
    );
    assert!(lines[0].contains("Alpha"), "{lines:?}");

    // Hovering an Active entry describes it.
    let (row, _) = state.hits.sidebar_active_rows[0];
    let outcome = mouse(&mut state, MouseEventKind::Moved, row.x + 6, row.y);
    assert!(outcome.repaint);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let lines = detail_text(&state, &frame);
    assert!(lines[0].contains("needs input"), "{lines:?}");
    assert_eq!(
        cell(&frame, row.x, row.y).symbol,
        "\u{258E}",
        "the hovered entry wears the bar"
    );
    assert_eq!(
        cell(&frame, row.x + 10, row.y).bg,
        crate::protocol::color_to_u32(state.config.palette.sidebar_hover_bg())
    );

    // Hovering the header describes the block.
    let header = state.hits.sidebar_active_header;
    mouse(&mut state, MouseEventKind::Moved, header.x + 4, header.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let lines = detail_text(&state, &frame);
    assert!(
        lines[0].contains("Active") && !lines[0].contains("agents"),
        "{lines:?}"
    );
    assert!(lines.join(" ").contains("1 blocked"), "{lines:?}");

    // A stale hover (the tab is gone) falls back to the focused tab.
    state.sidebar_hover = Some(SidebarHover::Tab("t_gone".into()));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(detail_text(&state, &frame)[0].contains("focus work"));

    // Leaving the sidebar clears the hover.
    let outcome = mouse(&mut state, MouseEventKind::Moved, COLS - 2, ROWS - 2);
    assert!(outcome.repaint);
    assert_eq!(state.sidebar_hover, None);
}

#[test]
fn detail_strip_lists_only_applicable_facts() {
    let mut state = active_state();
    state.compose(COLS, ROWS).expect("composed frame");
    // The blocked tab, hovered in the list.
    let row = list_row(&state, &state.hits.sidebar_tabs, "t_block");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let joined = detail_text(&state, &frame).join(" ");
    assert!(joined.contains("blocked"), "{joined:?}");
    assert!(joined.contains("claude"), "{joined:?}");
    for absent in ["panes", "live", "muted", "(suspended)", "\u{26AD}"] {
        assert!(!joined.contains(absent), "{absent}: {joined:?}");
    }
    // The voice tab says so.
    let row = list_row(&state, &state.hits.sidebar_tabs, "t_voice");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let joined = detail_text(&state, &frame).join(" ");
    assert!(joined.contains("live"), "{joined:?}");
    // The group header names the group and counts its tabs by status.
    let row = list_row(&state, &state.hits.sidebar_groups, "ws_b");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let lines = detail_text(&state, &frame);
    assert!(lines[0].contains("Beta"), "{lines:?}");
    assert!(lines.join(" ").contains("1 working"), "{lines:?}");
}

#[test]
fn detail_strip_names_a_team_members_role() {
    let mut state = active_state();
    let mut team = super::teams::team(Some("ship it"));
    team.workspace_id = "ws_b".into();
    team.members[0].pane_id = pane_of("t_work");
    team.members[0].tab_id = Some("t_work".into());
    team.members[1].tab_id = Some("t_gone".into());
    assert!(state.receive_teams(
        &ClientEndpointId::Local,
        super::teams::payload("boot-1", 1, vec![team])
    ));
    state.compose(COLS, ROWS).expect("composed frame");
    let row = list_row(&state, &state.hits.sidebar_tabs, "t_work");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let joined = detail_text(&state, &frame).join(" ");
    assert!(joined.contains("\u{25C6} fixer"), "{joined:?}");
    // A tab of the same group that holds no member gets no chip.
    let row = list_row(&state, &state.hits.sidebar_tabs, "t_done");
    mouse(&mut state, MouseEventKind::Moved, row.x + 8, row.y);
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let joined = detail_text(&state, &frame).join(" ");
    assert!(!joined.contains('\u{25C6}'), "{joined:?}");
}

#[test]
fn hover_repaints_only_on_target_change() {
    let mut state = active_state();
    state.compose(COLS, ROWS).expect("composed frame");
    let (row, _) = state.hits.sidebar_active_rows[1];
    assert!(mouse(&mut state, MouseEventKind::Moved, row.x + 4, row.y).repaint);
    assert!(
        !mouse(&mut state, MouseEventKind::Moved, row.x + 9, row.y).repaint,
        "the same row again: no repaint"
    );
    assert_eq!(
        state.sidebar_hover,
        Some(SidebarHover::ActiveEntry("t_voice".into()))
    );
    let (tab_row, _) = state.hits.sidebar_tabs[0];
    assert!(mouse(&mut state, MouseEventKind::Moved, tab_row.x + 4, tab_row.y).repaint);
    assert!(matches!(state.sidebar_hover, Some(SidebarHover::Tab(_))));
    // The toolbar is no row: the hover clears, once.
    assert!(mouse(&mut state, MouseEventKind::Moved, 2, 0).repaint);
    assert_eq!(state.sidebar_hover, None);
    assert!(!mouse(&mut state, MouseEventKind::Moved, 3, 0).repaint);
}

#[test]
fn wheel_scrolling_the_list_drops_the_stale_hover() {
    let mut state = active_state();
    state.compose(COLS, 16).expect("composed frame");
    assert!(
        state.hits.agent_max_scroll > 0,
        "the list scrolls at 16 rows"
    );
    let (row, _) = state.hits.sidebar_tabs[0];
    mouse(&mut state, MouseEventKind::Moved, row.x + 4, row.y);
    assert!(matches!(state.sidebar_hover, Some(SidebarHover::Tab(_))));
    // The pointer stays put while the rows move under it.
    assert!(mouse(&mut state, MouseEventKind::ScrollDown, row.x + 4, row.y).repaint);
    assert_eq!(state.sidebar_hover, None);
}

#[test]
fn settings_indicators_toggle_writes_sidebar_active_agents() {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "herdr-sidebar-active-settings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\nsidebar_layout = \"tabs\"\n").unwrap();
    std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);

    let mut state = active_state();
    state.open_settings_overlay();
    let index = ClientSettingsSection::ALL
        .iter()
        .position(|section| *section == ClientSettingsSection::Indicators)
        .expect("indicators section");
    for _ in 0..index {
        state.handle_input_bytes(b"\t");
    }
    let frame = state.compose(108, 30).expect("settings frame");
    let choices = state.hits.settings_choices.clone();
    assert_eq!(
        choices.iter().map(|(_, index)| *index).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        choices[1].0.y,
        choices[0].0.y + 1,
        "the radio rows stay adjacent"
    );
    assert_eq!(choices[2].0.y, choices[0].0.y + 3);
    assert!(row_text(&frame, choices[2].0).contains("active agents block: on"));
    let (rect, _) = choices[2];
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 4,
        rect.y,
    );

    let written = std::fs::read_to_string(&path).unwrap();
    std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
    let _ = std::fs::remove_dir_all(&dir);
    let parsed: Config = toml::from_str(&written).unwrap();
    assert!(!parsed.ui.sidebar_active_agents, "{written}");
    assert!(
        !written.contains("status_indicators"),
        "row 2 never writes an indicator style: {written}"
    );
    assert!(!state.config.sidebar_active_agents, "live reload");
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            selected: 2,
            ..
        }))
    ));
}

pub(super) mod fork_smoke {
    use super::*;

    #[test]
    fn active_block_lists_attention_first_and_hides_when_quiet() {
        let mut state = active_state();
        state.compose(COLS, ROWS).expect("composed frame");
        assert_eq!(
            active_ids(&state),
            ["t_block", "t_voice", "t_work", "t_focus", "t_done"]
        );
        assert!(state.hits.agent_body.y > 1);
        // Every agent exits: the block and its rule leave.
        let mut quiet = active_snapshot();
        quiet.agents.clear();
        for tab in &mut quiet.tabs {
            tab.agent_status = AgentStatus::Idle;
        }
        state.set_snapshot(Box::new(quiet));
        assert!(state.receive_voice(&ClientEndpointId::Local, voice_payload(2, false)));
        state.compose(COLS, ROWS).expect("composed frame");
        assert!(state.hits.sidebar_active_rows.is_empty());
        assert_eq!(state.hits.sidebar_active_header, Rect::default());
        assert_eq!(
            state.hits.agent_body.y, 1,
            "the list starts under the toolbar"
        );
    }
}
