//! Characterization goldens of the `tabs` sidebar (sidebar v2, S0a).
//!
//! One fixture covers every row kind the tabs layout draws: the ungrouped
//! bucket, a focused group (a working tab with subagents, an important blocked
//! tab whose reminder is lit, an idle tab with a scheduled reminder and a
//! colour tag), a team group with a purpose (a finished codex tab, a tab in
//! live voice mode, a suspended tab), a folded group, a browser-marked tab,
//! the pinned Browser row and a three-line status footer.
//!
//! Each golden is the sidebar region as text plus the style runs of every row
//! (fg, bg and bold, colours named by palette token). The expectations live in
//! `golden/*.txt` next to this file; `HERDR_UPDATE_GOLDEN=1 cargo nextest run
//! tab_sidebar_golden` rewrites them, so a rendering change is a reviewable
//! diff of those files.

use std::fmt::Write as _;

use super::*;
use crate::api::schema::browser::{BrowserGetInfo, BrowserPaneCursor, BrowserProfileInfo};
use crate::api::schema::team::{TeamActor, TeamInfo, TeamMemberInfo};
use crate::api::schema::{TabColor, TabRemindInterval};
use crate::config::{Config, SidebarLayoutConfig};
use crate::protocol::ClientShellTabStatusSegment;

/// Golden sidebar width (`ui.sidebar_width`); the frame is 106 columns.
pub(super) const GOLDEN_SIDEBAR_WIDTH: u16 = 31;
pub(super) const GOLDEN_COLS: u16 = 106;

pub(super) fn golden_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = SidebarLayoutConfig::Tabs;
    config.ui.sidebar_width = GOLDEN_SIDEBAR_WIDTH;
    config
}

fn tab(
    tab_id: &str,
    workspace_id: &str,
    number: usize,
    label: &str,
    status: AgentStatus,
) -> ClientShellTab {
    ClientShellTab {
        tab_id: tab_id.into(),
        workspace_id: workspace_id.into(),
        number,
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
    kind: &str,
    status: AgentStatus,
    seq: u64,
) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        name: None,
        display_agent: Some(kind.into()),
        agent: Some(kind.into()),
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

fn workspace(
    template: &ClientShellWorkspace,
    id: &str,
    number: usize,
    label: &str,
    active_tab: &str,
    status: AgentStatus,
) -> ClientShellWorkspace {
    let mut workspace = template.clone();
    workspace.workspace_id = id.into();
    workspace.number = number;
    workspace.label = label.into();
    workspace.active_tab_id = active_tab.into();
    workspace.focused = false;
    workspace.agent_status = status;
    workspace
}

/// The golden fixture (see the module docs). `t_plan` (Planning) is focused.
pub(super) fn golden_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let template = snapshot.workspaces[0].clone();
    snapshot.workspaces = vec![
        workspace(&template, "ws_1", 1, "home", "t_home", AgentStatus::Idle),
        workspace(
            &template,
            "ws_plan",
            2,
            "Planning",
            "t_plan",
            AgentStatus::Blocked,
        ),
        workspace(
            &template,
            "ws_team",
            3,
            "search-it",
            "t_done",
            AgentStatus::Done,
        ),
        workspace(
            &template,
            "ws_bird",
            4,
            "Bird sort",
            "t_bird1",
            AgentStatus::Idle,
        ),
    ];
    snapshot.workspaces[1].focused = true;
    snapshot.focused_workspace_id = Some("ws_plan".into());
    snapshot.focused_tab_id = Some("t_plan".into());
    snapshot.focused_pane_id = Some("p_plan".into());

    let mut plan = tab(
        "t_plan",
        "ws_plan",
        1,
        "sidebar redesign",
        AgentStatus::Working,
    );
    plan.focused = true;
    let mut review = tab("t_review", "ws_plan", 2, "review api", AgentStatus::Blocked);
    review.important = true;
    let mut notes = tab("t_notes", "ws_plan", 3, "notes", AgentStatus::Idle);
    notes.remind_every = Some(TabRemindInterval::H1);
    notes.color = Some(TabColor::Purple);
    snapshot.tabs = vec![
        tab("t_home", "ws_1", 1, "scratch", AgentStatus::Idle),
        plan,
        review,
        notes,
        tab("t_done", "ws_team", 1, "fixer", AgentStatus::Done),
        tab("t_voice", "ws_team", 2, "talker", AgentStatus::Idle),
        tab("t_park", "ws_team", 3, "parked", AgentStatus::Suspended),
        tab("t_bird1", "ws_bird", 1, "levels", AgentStatus::Idle),
        tab("t_bird2", "ws_bird", 2, "art", AgentStatus::Working),
    ];
    snapshot.panes = vec![
        pane("p_home", "ws_1", "t_home"),
        pane("p_plan", "ws_plan", "t_plan"),
        pane("p_review", "ws_plan", "t_review"),
        pane("p_notes", "ws_plan", "t_notes"),
        pane("p_done", "ws_team", "t_done"),
        pane("p_voice", "ws_team", "t_voice"),
        pane("p_park", "ws_team", "t_park"),
        pane("p_bird1", "ws_bird", "t_bird1"),
        pane("p_bird2", "ws_bird", "t_bird2"),
    ];
    snapshot.panes[1].focused = true;
    let mut working = agent(
        "p_plan",
        "ws_plan",
        "t_plan",
        "claude",
        AgentStatus::Working,
        5,
    );
    working.subagents = 2;
    snapshot.agents = vec![
        working,
        agent(
            "p_review",
            "ws_plan",
            "t_review",
            "claude",
            AgentStatus::Blocked,
            3,
        ),
        agent(
            "p_notes",
            "ws_plan",
            "t_notes",
            "claude",
            AgentStatus::Idle,
            2,
        ),
        agent("p_done", "ws_team", "t_done", "codex", AgentStatus::Done, 4),
        agent(
            "p_voice",
            "ws_team",
            "t_voice",
            "codex",
            AgentStatus::Idle,
            1,
        ),
        agent(
            "p_park",
            "ws_team",
            "t_park",
            "claude",
            AgentStatus::Suspended,
            0,
        ),
        agent(
            "p_bird2",
            "ws_bird",
            "t_bird2",
            "claude",
            AgentStatus::Working,
            6,
        ),
    ];
    snapshot.tab_bar_right = vec![ClientShellTabStatusSegment {
        text: "cpu 12%\n\u{1b}[31mmem 80%\u{1b}[0m\nnet ok".into(),
        accent: false,
    }];
    snapshot.tab_bar_right_separator = " ".into();
    snapshot
}

fn golden_team() -> TeamInfo {
    TeamInfo {
        workspace_id: "ws_team".into(),
        workspace_label: "search-it".into(),
        purpose: Some("Search It".into()),
        purpose_by: Some(TeamActor::User),
        created_unix: 1,
        revision: 1,
        members: ["t_done", "t_voice", "t_park"]
            .iter()
            .enumerate()
            .map(|(index, tab_id)| TeamMemberInfo {
                pane_id: tab_id.replace("t_", "p_"),
                tab_id: Some((*tab_id).into()),
                name: Some((*tab_id).into()),
                agent: Some("codex".into()),
                role: (index == 0).then(|| "fixer".into()),
                joined_unix: 1,
                ..TeamMemberInfo::default()
            })
            .collect(),
        excluded: Vec::new(),
    }
}

/// The golden palette: every token a distinct colour, so a cell's colour
/// names exactly one token in the dumps (the default theme aliases several,
/// e.g. `sidebar_bg` is `Reset`).
pub(super) fn golden_palette(base: &crate::app::state::Palette) -> crate::app::state::Palette {
    use ratatui::style::Color::Rgb;
    let mut palette = base.clone();
    palette.accent = Rgb(0x10, 0x00, 0x01);
    palette.panel_bg = Rgb(0x10, 0x00, 0x02);
    palette.sidebar_bg = Rgb(0x10, 0x00, 0x03);
    palette.active_row_bg = Rgb(0x10, 0x00, 0x04);
    palette.selection_bg = Rgb(0x10, 0x00, 0x05);
    palette.surface0 = Rgb(0x10, 0x00, 0x06);
    palette.surface1 = Rgb(0x10, 0x00, 0x07);
    palette.surface_dim = Rgb(0x10, 0x00, 0x08);
    palette.overlay0 = Rgb(0x10, 0x00, 0x09);
    palette.overlay1 = Rgb(0x10, 0x00, 0x0a);
    palette.text = Rgb(0x10, 0x00, 0x0b);
    palette.subtext0 = Rgb(0x10, 0x00, 0x0c);
    palette.mauve = Rgb(0x10, 0x00, 0x0d);
    palette.green = Rgb(0x10, 0x00, 0x0e);
    palette.yellow = Rgb(0x10, 0x00, 0x0f);
    palette.red = Rgb(0x10, 0x00, 0x10);
    palette.blue = Rgb(0x10, 0x00, 0x11);
    palette.teal = Rgb(0x10, 0x00, 0x12);
    palette.peach = Rgb(0x10, 0x00, 0x13);
    palette
}

/// The golden state: the fixture, the team and voice pushes, a lit reminder
/// on `t_review`, `t_home` marked by the browser, `Bird sort` folded.
/// `t_done` is first seen working, so the client projects its `Done`
/// (a finished turn it watched) instead of `Idle`.
pub(super) fn golden_state(config: &Config) -> ClientShellState {
    let mut shell_config = ClientShellConfig::from_config(config);
    shell_config.palette = golden_palette(&shell_config.palette);
    let mut state = ClientShellState::new(shell_config);
    let mut working = golden_snapshot();
    for agent in &mut working.agents {
        if agent.pane_id == "p_done" {
            agent.agent_status = AgentStatus::Working;
            agent.state_change_seq = 3;
        }
    }
    state.set_snapshot(Box::new(working));
    state.set_snapshot(Box::new(golden_snapshot()));
    state.set_pane_surface(surface());
    let teams: crate::server::headless::teams::TeamsPayload =
        serde_json::from_value(serde_json::json!({
            "boot_id": "boot-1",
            "revision": 1,
            "teams": [golden_team()],
        }))
        .expect("teams payload");
    assert!(state.receive_teams(&ClientEndpointId::Local, teams));
    let voice: crate::server::headless::voice::VoicePayload =
        serde_json::from_value(serde_json::json!({
            "boot_id": "boot-1",
            "revision": 1,
            "panes": [{ "pane_id": "p_voice", "voice": "live" }],
        }))
        .expect("voice payload");
    assert!(state.receive_voice(&ClientEndpointId::Local, voice));
    state.collapsed_groups.insert(group_key("ws_bird"));
    // The important blocked tab's reminder fires: its `★` lights.
    let t0 = std::time::Instant::now();
    state.tick_notifications(t0);
    state.tick_notifications(t0 + std::time::Duration::from_secs(11 * 60));
    // `t_home`'s pane used the browser just now.
    state.browser.info = Some(BrowserGetInfo {
        seq: 1,
        enabled: true,
        profiles: vec![BrowserProfileInfo {
            name: "main".into(),
            state: "stopped".into(),
            ..Default::default()
        }],
        recent_panes: vec![BrowserPaneCursor {
            pane_id: "p_home".into(),
            tab_id: Some("t_home".into()),
            current: "main:t1".into(),
            last_at: crate::app::news::unix_now(),
        }],
        ..Default::default()
    });
    state
}

/// Palette token names, in lookup order: the first token with a cell's
/// colour names it (`Reset` is `-`).
fn token_name(palette: &crate::app::state::Palette, value: u32) -> String {
    use crate::protocol::color_to_u32;
    let tokens = [
        ("text", palette.text),
        ("subtext0", palette.subtext0),
        ("overlay1", palette.overlay1),
        ("overlay0", palette.overlay0),
        ("accent", palette.accent),
        ("sidebar_bg", palette.sidebar_bg),
        ("active_row_bg", palette.active_row_bg),
        ("selection_bg", palette.selection_bg),
        ("surface0", palette.surface0),
        ("surface1", palette.surface1),
        ("surface_dim", palette.surface_dim),
        ("panel_bg", palette.panel_bg),
        ("red", palette.red),
        ("yellow", palette.yellow),
        ("green", palette.green),
        ("teal", palette.teal),
        ("blue", palette.blue),
        ("mauve", palette.mauve),
        ("peach", palette.peach),
    ];
    if value == color_to_u32(ratatui::style::Color::Reset) {
        return "-".into();
    }
    tokens
        .iter()
        .find(|(_, color)| color_to_u32(*color) == value)
        .map_or_else(|| format!("#{value:08x}"), |(name, _)| (*name).to_string())
}

/// The sidebar rows `y0..y1` (columns `0..width`): each row's text, then
/// its style runs `x0-x1 fg/bg[/b]` (adjacent cells with one style merge).
pub(super) fn sidebar_dump(
    frame: &FrameData,
    palette: &crate::app::state::Palette,
    width: u16,
    y0: u16,
    y1: u16,
) -> String {
    let bold = ratatui::style::Modifier::BOLD.bits();
    let mut out = String::new();
    for y in y0..y1.min(frame.height) {
        let cell =
            |x: u16| &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)];
        let text: String = (0..width).map(|x| cell(x).symbol.as_str()).collect();
        let _ = writeln!(out, "{y:02}|{text}|");
        let style = |x: u16| {
            let cell = cell(x);
            (
                token_name(palette, cell.fg),
                token_name(palette, cell.bg),
                cell.modifier & bold != 0,
            )
        };
        let mut start = 0;
        while start < width {
            let current = style(start);
            let mut end = start + 1;
            while end < width && style(end) == current {
                end += 1;
            }
            let (fg, bg, is_bold) = current;
            let _ = writeln!(
                out,
                "   {start:02}-{:02} {fg}/{bg}{}",
                end - 1,
                if is_bold { "/b" } else { "" }
            );
            start = end;
        }
    }
    out
}

/// The cell text, fg, bg and bold of one cell.
pub(super) fn cell_style(frame: &FrameData, x: u16, y: u16) -> (String, u32, u32, bool) {
    let cell = &frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)];
    (
        cell.symbol.clone(),
        cell.fg,
        cell.bg,
        cell.modifier & ratatui::style::Modifier::BOLD.bits() != 0,
    )
}

/// The text of the sidebar rows `y0..y1`.
pub(super) fn sidebar_lines(frame: &FrameData, width: u16, y0: u16, y1: u16) -> Vec<String> {
    (y0..y1.min(frame.height))
        .map(|y| {
            (0..width)
                .map(|x| {
                    frame.cells[usize::from(y) * usize::from(frame.width) + usize::from(x)]
                        .symbol
                        .as_str()
                })
                .collect()
        })
        .collect()
}

/// Compare `actual` with `golden/<name>.txt`, or rewrite it when
/// `HERDR_UPDATE_GOLDEN` is set.
fn assert_golden(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/client/shell/tests/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("HERDR_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden dir")).expect("golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}; run with HERDR_UPDATE_GOLDEN=1", path.display()));
    if expected != actual {
        let first = expected
            .lines()
            .zip(actual.lines())
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        let context = |text: &str| {
            text.lines()
                .skip(first.saturating_sub(3))
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "golden {name} differs at line {}\n--- expected\n{}\n--- actual\n{}",
            first + 1,
            context(&expected),
            context(actual)
        );
    }
}

fn compose_golden(rows: u16) -> (ClientShellState, FrameData) {
    let mut state = golden_state(&golden_config());
    let frame = state.compose(GOLDEN_COLS, rows).expect("composed frame");
    (state, frame)
}

#[test]
fn golden_list_rows_at_31_columns() {
    let (state, frame) = compose_golden(60);
    assert_eq!(state.hits.sidebar_divider.x, GOLDEN_SIDEBAR_WIDTH - 1);
    let body = state.hits.agent_body;
    let dump = sidebar_dump(
        &frame,
        &state.config.palette,
        GOLDEN_SIDEBAR_WIDTH,
        body.y,
        body.y + 13,
    );
    assert_golden("tab_sidebar_list_rows", &dump);
    let first = sidebar_lines(&frame, GOLDEN_SIDEBAR_WIDTH, body.y, body.y + 1);
    assert!(first[0].contains("scratch"), "{first:?}");
    // Group headers sit on a surface0 band today.
    let (header, _) = state.hits.sidebar_groups[0];
    let (symbol, _, bg, _) = cell_style(&frame, header.x + 1, header.y);
    assert_eq!(symbol, "▾");
    assert_eq!(
        bg,
        crate::protocol::color_to_u32(state.config.palette.surface0)
    );
    // Every listed tab, in order; the folded group's tabs are hidden.
    let ids: Vec<&str> = state
        .hits
        .sidebar_tabs
        .iter()
        .map(|(_, id)| id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["t_home", "t_plan", "t_review", "t_notes", "t_done", "t_voice", "t_park"]
    );
    let groups: Vec<&str> = state
        .hits
        .sidebar_groups
        .iter()
        .map(|(_, id)| id.as_str())
        .collect();
    assert_eq!(groups, ["ws_plan", "ws_team", "ws_bird"]);
}

#[test]
fn golden_short_height_keeps_the_last_tab() {
    let (mut state, frame) = compose_golden(14);
    let dump = sidebar_dump(&frame, &state.config.palette, GOLDEN_SIDEBAR_WIDTH, 0, 14);
    assert_golden("tab_sidebar_short", &dump);
    assert!(state.hits.agent_body.height >= 1);
    // Scrolled to the bottom, the last list row is drawn.
    state.agent_scroll = usize::MAX;
    let frame = state.compose(GOLDEN_COLS, 14).expect("composed frame");
    let dump = sidebar_dump(&frame, &state.config.palette, GOLDEN_SIDEBAR_WIDTH, 0, 14);
    assert_golden("tab_sidebar_short_scrolled", &dump);
    assert!(
        state
            .hits
            .sidebar_groups
            .iter()
            .any(|(_, id)| id == "ws_bird"),
        "the last row (the folded group) shows when scrolled down"
    );
}

#[test]
fn golden_footer_and_pinned_rows() {
    let (state, frame) = compose_golden(40);
    let body = state.hits.agent_body;
    assert_ne!(
        state.hits.browser_row,
        Rect::default(),
        "browser row pinned"
    );
    let dump = sidebar_dump(
        &frame,
        &state.config.palette,
        GOLDEN_SIDEBAR_WIDTH,
        body.bottom(),
        40,
    );
    assert_golden("tab_sidebar_footer", &dump);
    let toolbar = sidebar_dump(&frame, &state.config.palette, GOLDEN_SIDEBAR_WIDTH, 0, 1);
    assert_golden("tab_sidebar_toolbar", &toolbar);
}
