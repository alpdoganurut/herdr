//! Fork: agents' context use in the tabs sidebar (`agent_context.rs`): the
//! `NN%` row mark from 75 %, the detail strip chip, the model rebuild on a
//! push and the `ui.sidebar_context_usage` toggle.

use super::*;
use crate::app::agent_context::AgentContextPane;
use crate::config::{Config, SidebarLayoutConfig};
use crate::server::headless::agent_context::AgentContextPayload;

const COLS: u16 = 106;
const ROWS: u16 = 40;

fn tabs_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_active_agents = false;
    config.ui.sidebar_pinned_agents = false;
    config.ui.sidebar_scheduled_agents = false;
    config
}

fn agent(pane_id: &str, tab_id: &str) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some("claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// Three tabs, one Claude pane each; `tab_1` is focused.
fn context_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let first = snapshot.tabs[0].clone();
    let pane = snapshot.panes[0].clone();
    snapshot.tabs = (1..=3)
        .map(|index| ClientShellTab {
            tab_id: format!("tab_{index}"),
            number: index,
            label: format!("agent{index}"),
            custom_label: true,
            focused: index == 1,
            ..first.clone()
        })
        .collect();
    snapshot.panes = (1..=3)
        .map(|index| ClientShellPane {
            pane_id: format!("pane_{index}"),
            tab_id: format!("tab_{index}"),
            focused: index == 1,
            ..pane.clone()
        })
        .collect();
    snapshot.agents = (1..=3)
        .map(|index| agent(&format!("pane_{index}"), &format!("tab_{index}")))
        .collect();
    snapshot
}

fn payload(revision: u64, panes: &[(&str, u64, u64)]) -> AgentContextPayload {
    AgentContextPayload {
        boot_id: "boot-1".into(),
        revision,
        panes: panes
            .iter()
            .map(|(pane_id, used, window)| AgentContextPane {
                pane_id: (*pane_id).into(),
                used: *used,
                window: *window,
            })
            .collect(),
    }
}

fn state_with(config: &Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(context_snapshot()));
    state.set_pane_surface(surface());
    state
}

fn row_rect(state: &ClientShellState, tab_id: &str) -> ratatui::layout::Rect {
    state
        .hits
        .sidebar_tabs
        .iter()
        .find(|(_, id)| id == tab_id)
        .map(|(rect, _)| *rect)
        .expect("tab row")
}

fn text_of(frame: &FrameData, rect: ratatui::layout::Rect) -> String {
    (rect.x..rect.right())
        .map(|x| {
            frame.cells[(rect.y * frame.width + x) as usize]
                .symbol
                .as_str()
        })
        .collect()
}

/// The fg of the first cell of `needle` in the row.
fn fg_of(frame: &FrameData, rect: ratatui::layout::Rect, needle: char) -> Option<u32> {
    (rect.x..rect.right())
        .find(|x| frame.cells[(rect.y * frame.width + x) as usize].symbol == needle.to_string())
        .map(|x| frame.cells[(rect.y * frame.width + x) as usize].fg)
}

fn detail_text(state: &ClientShellState, frame: &FrameData) -> String {
    let detail = state.hits.sidebar_detail;
    (detail.y..detail.bottom())
        .map(|y| {
            text_of(
                frame,
                ratatui::layout::Rect::new(detail.x, y, detail.width, 1),
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn rows_show_the_use_from_75_percent_yellow_then_red() {
    let mut state = state_with(&tabs_config());
    state.compose(COLS, ROWS).expect("composed frame");
    let builds = state.sidebar_model.builds;
    assert!(state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(
            1,
            &[
                ("pane_1", 120_000, 200_000),
                ("pane_2", 164_000, 200_000),
                ("pane_3", 186_000, 200_000),
            ],
        ),
    ));
    // An older revision of the same server is ignored.
    assert!(!state.receive_agent_context(&ClientEndpointId::Local, payload(1, &[])));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        state.sidebar_model.builds,
        builds + 1,
        "a push rebuilds the model once"
    );
    state.compose(COLS, ROWS).expect("composed frame");
    assert_eq!(
        state.sidebar_model.builds,
        builds + 1,
        "no rebuild per frame"
    );

    let palette = state.config.palette.clone();
    let rgb = crate::protocol::color_to_u32;
    let first = text_of(&frame, row_rect(&state, "tab_1"));
    assert!(!first.contains('%'), "60%: nothing: {first:?}");
    let second_rect = row_rect(&state, "tab_2");
    let second = text_of(&frame, second_rect);
    assert!(second.trim_end().ends_with("82%"), "{second:?}");
    assert_eq!(fg_of(&frame, second_rect, '8'), Some(rgb(palette.yellow)));
    let third_rect = row_rect(&state, "tab_3");
    let third = text_of(&frame, third_rect);
    assert!(third.trim_end().ends_with("93%"), "{third:?}");
    assert_eq!(fg_of(&frame, third_rect, '9'), Some(rgb(palette.red)));
    // The label keeps its place.
    assert!(second.contains("agent2"), "{second:?}");
}

#[test]
fn the_use_sits_left_of_the_marks_and_the_label_truncates_first() {
    let mut snapshot = context_snapshot();
    snapshot.tabs[1].important = true;
    snapshot.tabs[1].label = "a-very-long-tab-label-that-truncates".into();
    let mut state = state_with(&tabs_config());
    state.set_snapshot(Box::new(snapshot));
    state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(1, &[("pane_2", 164_000, 200_000)]),
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let row = text_of(&frame, row_rect(&state, "tab_2"));
    let row = row.trim_end();
    assert!(row.ends_with("82% \u{2605}"), "{row:?}");
    assert!(row.contains("a-very"), "the label truncates first: {row:?}");
}

#[test]
fn the_narrowest_row_packs_the_use_and_the_marks_and_truncates_the_label() {
    let mut snapshot = context_snapshot();
    snapshot.tabs[1].important = true;
    snapshot.tabs[1].label = "a-long-label".into();
    let mut state = state_with(&tabs_config());
    state.set_snapshot(Box::new(snapshot));
    state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(1, &[("pane_2", 186_000, 200_000)]),
    );
    // Narrower than the minimum: clamped to it.
    state.sidebar_width = 1;
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let rect = row_rect(&state, "tab_2");
    let row = text_of(&frame, rect);
    let row = row.trim_end();
    // Packed without gaps, the margin kept, the label cut.
    assert!(row.ends_with("93%\u{2605}"), "{row:?}");
    assert!(!row.contains("a-long-label"), "{row:?}");
    assert!(row.contains("a-l\u{2026}"), "{row:?}");
}

#[test]
fn the_detail_strip_names_the_use_at_any_level_and_the_toggle_hides_it() {
    let mut state = state_with(&tabs_config());
    state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(1, &[("pane_1", 120_000, 200_000)]),
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    let text = detail_text(&state, &frame);
    assert!(text.contains("ctx 60% \u{b7} 120k/200k"), "{text:?}");

    let mut config = tabs_config();
    config.ui.sidebar_context_usage = false;
    let mut state = state_with(&config);
    state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(1, &[("pane_1", 190_000, 200_000)]),
    );
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(!detail_text(&state, &frame).contains("ctx"));
    assert!(!text_of(&frame, row_rect(&state, "tab_1")).contains('%'));
}

#[test]
fn a_push_from_another_server_boot_is_not_used() {
    let mut state = state_with(&tabs_config());
    let mut other = payload(5, &[("pane_2", 190_000, 200_000)]);
    other.boot_id = "boot-0".into();
    assert!(state.receive_agent_context(&ClientEndpointId::Local, other));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(!text_of(&frame, row_rect(&state, "tab_2")).contains('%'));
    // The current boot replaces it whatever the revision.
    assert!(state.receive_agent_context(
        &ClientEndpointId::Local,
        payload(1, &[("pane_2", 190_000, 200_000)]),
    ));
    let frame = state.compose(COLS, ROWS).expect("composed frame");
    assert!(text_of(&frame, row_rect(&state, "tab_2"))
        .trim_end()
        .ends_with("95%"));
}

#[test]
fn context_chip_text_formats_tokens() {
    use crate::agent_context::ContextUsage;
    let text = |used, window| {
        super::super::tab_sidebar_detail::context_chip_text(ContextUsage { used, window })
            .as_str()
            .to_owned()
    };
    assert_eq!(text(164_000, 200_000), "ctx 82% \u{b7} 164k/200k");
    assert_eq!(text(820_271, 1_000_000), "ctx 82% \u{b7} 820k/1M");
    assert_eq!(text(1_250_000, 2_000_000), "ctx 62% \u{b7} 1.2M/2M");
    assert_eq!(text(950, 258_400), "ctx 0% \u{b7} 950/258k");
}
