//! The `tabs` sidebar layout's breathing agent glyph (`breathe.rs`).

use super::super::breathe::{dimness, glyph_color, FRAME, PERIOD};
use super::*;
use crate::config::{Config, SidebarLayoutConfig};
use crate::protocol::color_to_u32;
use ratatui::style::Color;
use std::time::{Duration, Instant};

fn tab(tab_id: &str, number: usize, focused: bool, status: AgentStatus) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: "ws_1".into(),
        number,
        label: format!("t{number}"),
        custom_label: true,
        zoomed: false,
        focused,
        agent_status: status,
        color: None,
        important: false,
        remind_every: None,
    }
}

fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
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
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
        subagents: 0,
    }
}

/// tab_1 (focused) and tab_2 working, tab_3 idle, all claude.
fn working_snapshot(statuses: [AgentStatus; 3]) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = statuses
        .iter()
        .enumerate()
        .map(|(index, status)| {
            tab(
                &format!("tab_{}", index + 1),
                index + 1,
                index == 0,
                *status,
            )
        })
        .collect();
    snapshot.agents = statuses
        .iter()
        .enumerate()
        .map(|(index, status)| {
            agent(
                &format!("pane_{}", index + 1),
                &format!("tab_{}", index + 1),
                *status,
            )
        })
        .collect();
    snapshot
}

fn tabs_state(statuses: [AgentStatus; 3]) -> ClientShellState {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    // The host reported its background, so the default one interpolates.
    state.host_background = Some(crate::terminal_theme::RgbColor {
        r: 0x1e,
        g: 0x1e,
        b: 0x2e,
    });
    state.set_snapshot(Box::new(working_snapshot(statuses)));
    state.set_pane_surface(surface());
    state
}

/// The glyph cell's fg on each tab row, composed at `phase` of the breath.
fn glyph_fgs(state: &mut ClientShellState, phase: f32) -> Vec<u32> {
    state.breathe_clock =
        Some(state.breathe_epoch + Duration::from_secs_f32(PERIOD.as_secs_f32() * phase));
    let frame = state.compose(106, 20).expect("composed frame");
    state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(rect, _)| frame.cells[(rect.y * frame.width + rect.right() - 2) as usize].fg)
        .collect()
}

const WORKING: [AgentStatus; 3] = [
    AgentStatus::Working,
    AgentStatus::Working,
    AgentStatus::Idle,
];

#[test]
fn the_breath_is_an_eased_cycle() {
    assert_eq!(dimness(0.0), 0.0);
    assert!((dimness(0.5) - 1.0).abs() < 1e-6);
    assert!(dimness(1.0) < 1e-6);
    assert!(
        (dimness(0.25) - 0.5).abs() < 1e-6,
        "cosine: half dim at a quarter"
    );
    // RGB interpolation toward the background; named colors map to RGB.
    let normal = Color::Rgb(200, 100, 50);
    let bg = Color::Rgb(0, 0, 0);
    assert_eq!(glyph_color(normal, bg, None, 0.0), normal);
    assert_eq!(glyph_color(normal, bg, None, 0.5), Color::Rgb(70, 35, 18));
    assert_eq!(
        glyph_color(Color::White, bg, None, 0.5),
        Color::Rgb(89, 89, 89)
    );
    // The terminal default background resolves through the host's color;
    // unknown, the glyph toggles instead.
    assert_eq!(
        glyph_color(normal, Color::Reset, Some((0, 0, 0)), 0.5),
        Color::Rgb(70, 35, 18)
    );
    assert_eq!(
        glyph_color(normal, Color::Reset, None, 0.5),
        Color::DarkGray
    );
    assert_eq!(glyph_color(normal, Color::Reset, None, 0.2), normal);
}

#[test]
fn working_glyphs_breathe_through_a_cycle_and_others_stay() {
    let mut state = tabs_state(WORKING);
    let rest = glyph_fgs(&mut state, 0.0);
    let half = glyph_fgs(&mut state, 0.5);
    let full = glyph_fgs(&mut state, 1.0);
    let quarter = glyph_fgs(&mut state, 0.25);
    assert_eq!(full, rest, "a whole breath comes back");
    for row in [0, 1] {
        assert_ne!(half[row], rest[row], "working row {row} dims half way");
        assert_ne!(quarter[row], rest[row]);
        assert_ne!(quarter[row], half[row], "a smooth fade, not a toggle");
    }
    assert_eq!(half[2], rest[2], "an idle tab does not breathe");
    // The focused row's brand color dims toward its highlight.
    let palette = state.config.palette.clone();
    let expected = glyph_color(
        Color::Rgb(0xD9, 0x77, 0x57),
        palette.active_row_bg,
        state.breathe_reset_rgb(),
        0.5,
    );
    assert_eq!(half[0], color_to_u32(expected));
}

#[test]
fn frames_are_scheduled_only_while_something_breathes() {
    let mut state = tabs_state(WORKING);
    state.compose(106, 20).expect("composed frame");
    let composed = state.last_composed_at.expect("composed");
    assert_eq!(state.next_breathe_deadline(), Some(composed + FRAME));
    assert!(!state.tick_breathing(composed));
    assert!(state.tick_breathing(composed + FRAME));
    assert!(state.timer_delay(composed) <= FRAME);

    // Nothing working: no frame at rest.
    let mut state = tabs_state([AgentStatus::Idle, AgentStatus::Done, AgentStatus::Blocked]);
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.next_breathe_deadline(), None);
    assert!(!state.tick_breathing(Instant::now() + Duration::from_secs(5)));

    // The spaces layout draws no agent glyphs, so nothing breathes there.
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(working_snapshot(WORKING)));
    state.set_pane_surface(surface());
    state.compose(106, 20).expect("composed frame");
    assert_eq!(state.next_breathe_deadline(), None);
}

#[test]
fn a_pane_fast_path_patch_leaves_the_breathing_glyph_alone() {
    let mut state = tabs_state(WORKING);
    let half = glyph_fgs(&mut state, 0.5);
    let (rect, _) = state.hits.sidebar_tabs[0];
    let glyph = (rect.right() - 2, rect.y);
    let mut pane = surface().panes[0].clone();
    pane.content_revision = 1;
    let patch = crate::protocol::PaneSurfacePatch {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        base_surface_revision: 1,
        surface_revision: 2,
        rows: vec![crate::protocol::PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![
                crate::protocol::CellData {
                    symbol: "$".into(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                };
                2
            ],
        }],
        panes: vec![pane],
        cursor: None,
    };
    let ClientPaneSurfacePatchOutcome::Applied(Some(composed)) =
        state.apply_pane_surface_patch(patch)
    else {
        panic!("the fast path applies");
    };
    assert!(
        composed.rows.iter().all(|row| row.y != glyph.1
            || glyph.0 < row.x
            || glyph.0 >= row.x + row.cells.len() as u16),
        "the patch never touches the sidebar glyph"
    );
    // The next animation frame still draws the glyph at its phase.
    assert_eq!(glyph_fgs(&mut state, 0.5), half);
}

/// Fork smoke tests: FORK.md section 10 lists them by name and the sync gate
/// runs them with `-E 'test(fork_smoke)'`.
mod fork_smoke {
    use super::*;

    #[test]
    fn a_working_tab_glyph_breathes_and_schedules_frames() {
        let mut state = tabs_state(WORKING);
        let rest = glyph_fgs(&mut state, 0.0);
        let half = glyph_fgs(&mut state, 0.5);
        assert_ne!(half[1], rest[1], "the working glyph dims half way");
        assert_eq!(half[2], rest[2], "the idle one does not");
        assert!(state.next_breathe_deadline().is_some());
    }
}
