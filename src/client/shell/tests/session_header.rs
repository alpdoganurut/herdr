//! The session header (fork, header design): the tabs layout's one row on
//! top of the session area. Goldens of the row at several widths
//! (`golden/session_header.txt`, rewritten by `HERDR_UPDATE_GOLDEN=1`), its
//! facts' rules, its controls' hits and actions, and the one dock slot the
//! notes and browser docks share.

use std::collections::{HashMap, HashSet};

use super::super::session_header::{self, HeaderBrowser, HeaderTarget, SessionHeader};
use super::*;
use crate::api::schema::{BrowserCompanionAppInfo, BrowserGetInfo, BrowserSessionInfo};
use crate::config::Config;
use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 160;
const ROWS: u16 = 40;

fn header<'a>(name: &'a str, palette: &crate::app::state::Palette) -> SessionHeader<'a> {
    SessionHeader {
        icon: "●",
        icon_color: palette.yellow,
        name,
        name_color: palette.text,
        context: Some(("82%", palette.yellow)),
        subagents: 3,
        mail: Some(("✉1", palette.overlay1)),
        browser: Some(HeaderBrowser {
            viewing: false,
            docked: false,
        }),
        notes_docked: false,
        hover: None,
    }
}

/// The header drawn alone at `width` columns: its text and style runs.
fn dump(header: &SessionHeader<'_>, width: u16) -> String {
    let palette = super::tab_sidebar_golden::golden_palette(
        &ClientShellConfig::from_config(&Config::default()).palette,
    );
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    session_header::render(&mut buffer, area, header, &palette);
    let frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
    super::tab_sidebar_golden::sidebar_dump(&frame, &palette, width, 0, 1)
}

fn text(header: &SessionHeader<'_>, width: u16) -> String {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    session_header::render(&mut buffer, area, header, &palette);
    (0..width)
        .map(|x| buffer[(x, 0)].symbol().to_owned())
        .collect::<String>()
}

#[test]
fn golden_session_header_at_several_widths() {
    let palette = super::tab_sidebar_golden::golden_palette(
        &ClientShellConfig::from_config(&Config::default()).palette,
    );
    let mut out = String::new();
    let mut section = |title: &str, header: &SessionHeader<'_>, width: u16| {
        out.push_str(&format!("== {title} @ {width}\n"));
        out.push_str(&dump(header, width));
    };
    let working = header("header-design-and-navigation", &palette);
    for width in [110, 60, 44, 36, 26, 12] {
        section("terminal view, working, facts", &working, width);
    }
    let browser_view = SessionHeader {
        icon: "○",
        icon_color: palette.green,
        context: None,
        subagents: 0,
        mail: None,
        browser: Some(HeaderBrowser {
            viewing: true,
            docked: false,
        }),
        notes_docked: true,
        ..header("header-design", &palette)
    };
    section("browser view, notes docked", &browser_view, 80);
    let docked = SessionHeader {
        icon: "◉",
        icon_color: palette.red,
        subagents: 0,
        browser: Some(HeaderBrowser {
            viewing: false,
            docked: true,
        }),
        hover: Some(HeaderTarget::NotesDock),
        ..header("endpoint-compatibility-review", &palette)
    };
    section("blocked, browser docked, notes hovered", &docked, 60);
    let shell = SessionHeader {
        icon: "·",
        icon_color: palette.overlay0,
        context: None,
        subagents: 0,
        mail: None,
        browser: None,
        hover: Some(HeaderTarget::ViewBrowser),
        ..header("zsh", &palette)
    };
    section("shell, no companion", &shell, 60);
    super::tab_sidebar_golden::assert_golden("session_header", &out);
}

#[test]
fn the_name_truncates_first_then_the_view_tabs_shrink_and_the_toggles_stay() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let header = header("header-design-and-navigation", &palette);
    let wide = text(&header, 110);
    assert!(
        wide.contains("● header-design-and-navigation  82% ⚭3 ✉1"),
        "{wide}"
    );
    assert!(wide.contains("Terminal  Browser ◧  ▤"), "{wide}");
    // the name gives way while it keeps MIN_NAME cells
    let mid = text(&header, 52);
    assert!(mid.contains("Terminal  Browser ◧  ▤"), "{mid}");
    assert!(mid.contains('…') && !mid.contains("navigation"), "{mid}");
    // then the short tabs
    let narrow = text(&header, 36);
    assert!(narrow.contains("T  B ◧  ▤"), "{narrow}");
    assert!(!narrow.contains("Terminal"), "{narrow}");
    // then the dock toggle alone; ▤ is always the last cell before the margin
    for width in [26u16, 20, 12, 6] {
        let row = text(&header, width);
        assert!(row.trim_end().ends_with('▤'), "{width}: {row:?}");
        if width >= 12 {
            assert!(row.contains('◧'), "{width}: {row:?}");
        }
    }
}

#[test]
fn the_status_icon_follows_the_tab_state_and_a_resume() {
    use crate::config::StatusIndicatorStyle::{Dots, Symbols};
    let glyph = session_header::status_glyph;
    assert_eq!(glyph(AgentStatus::Working, false, Dots), "●");
    assert_eq!(glyph(AgentStatus::Idle, false, Dots), "○");
    assert_eq!(glyph(AgentStatus::Blocked, false, Dots), "◉");
    assert_eq!(glyph(AgentStatus::Suspended, false, Dots), "◌");
    assert_eq!(glyph(AgentStatus::Suspended, true, Dots), "↻");
    assert_eq!(glyph(AgentStatus::Blocked, false, Symbols), "×");
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let color = |status, resuming| session_header::status_glyph_color(status, resuming, &palette);
    assert_eq!(color(AgentStatus::Working, false), palette.yellow);
    assert_eq!(color(AgentStatus::Idle, false), palette.green);
    assert_eq!(color(AgentStatus::Blocked, false), palette.red);
    assert_eq!(color(AgentStatus::Suspended, false), palette.overlay0);
    assert_eq!(color(AgentStatus::Suspended, true), palette.yellow);
}

fn agent(pane_id: &str, status: AgentStatus, subagents: u32) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: None,
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
        subagents,
    }
}

fn header_state(config: &Config) -> ClientShellState {
    let mut snapshot = snapshot();
    snapshot.tabs[0].label = "restart screen".into();
    snapshot.tabs[0].agent_status = AgentStatus::Working;
    snapshot.agents = vec![agent("pane_1", AgentStatus::Working, 0)];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface());
    state.compose(COLS, ROWS);
    state
}

fn set_context(state: &mut ClientShellState, used: u64) {
    state.agent_context.insert(
        ClientEndpointId::Local,
        super::super::agent_context::ClientAgentContextState {
            boot_id: "boot-1".into(),
            revision: used,
            panes: HashMap::from([(
                "pane_1".to_owned(),
                crate::agent_context::ContextUsage {
                    used,
                    window: 200_000,
                },
            )]),
            handoffs: HashMap::new(),
        },
    );
}

#[test]
fn context_shows_from_75_percent_and_the_counts_only_when_not_zero() {
    let mut state = header_state(&Config::default());
    let view = state.session_header_view().expect("a header");
    assert_eq!(view.name, "restart screen");
    assert_eq!(view.icon, "●");
    assert_eq!((view.context, view.subagents, view.mail), (None, 0, None));
    // 74 %: nothing; 75 %: the tab row's text and colour
    set_context(&mut state, 148_000);
    assert_eq!(state.session_header_view().expect("header").context, None);
    set_context(&mut state, 150_000);
    let palette = state.config.palette.clone();
    assert_eq!(
        state.session_header_view().expect("header").context,
        Some(("75%", palette.yellow))
    );
    set_context(&mut state, 190_000);
    assert_eq!(
        state.session_header_view().expect("header").context,
        Some(("95%", palette.red))
    );
    // `ui.sidebar_context_usage = false` hides it, as on the tab row
    let mut config = Config::default();
    config.ui.sidebar_context_usage = false;
    let mut off = header_state(&config);
    set_context(&mut off, 190_000);
    assert_eq!(off.session_header_view().expect("header").context, None);
    // subagents and quiet mail
    let mut snapshot = (*state.snapshot.clone().expect("snapshot")).clone();
    snapshot.agents = vec![agent("pane_1", AgentStatus::Working, 2)];
    state.set_snapshot(Box::new(snapshot));
    state.quiet.insert(
        ClientEndpointId::Local,
        super::super::quiet::ClientQuietState {
            boot_id: "boot-1".into(),
            revision: 1,
            dnd_tab_ids: HashSet::new(),
            tab_counts: HashMap::from([("tab_1".to_owned(), 3)]),
        },
    );
    let view = state.session_header_view().expect("header");
    assert_eq!(view.subagents, 2);
    assert_eq!(view.mail.map(|(text, _)| text), Some("✉3"));
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let row = super::tab_sidebar_golden::sidebar_lines(&frame, COLS, 0, 1).remove(0);
    assert!(row.contains("● restart screen  95% ⚭2 ✉3"), "{row}");
    // no agent: the shell's dot, no facts
    let mut snapshot = (*state.snapshot.clone().expect("snapshot")).clone();
    snapshot.agents.clear();
    snapshot.tabs[0].agent_status = AgentStatus::Unknown;
    state.set_snapshot(Box::new(snapshot));
    state.quiet.clear();
    let view = state.session_header_view().expect("header");
    assert_eq!(
        (view.icon, view.context, view.subagents, view.mail),
        ("·", None, 0, None)
    );
}

#[test]
fn a_restarting_agent_shows_the_resume_icon() {
    let mut state = header_state(&Config::default());
    let mut snapshot = (*state.snapshot.clone().expect("snapshot")).clone();
    snapshot.tabs[0].agent_status = AgentStatus::Suspended;
    // the relaunched agent is not detected yet: only its pane is listed
    snapshot.agents.clear();
    state.set_snapshot(Box::new(snapshot));
    state.agent_times.insert(
        ClientEndpointId::Local,
        super::super::agent_times::ClientAgentTimesState {
            boot_id: "boot-1".into(),
            resume_marks: HashMap::from([(
                "pane_1".to_owned(),
                super::super::agent_times::ClientResumeMark::Restarting,
            )]),
            ..Default::default()
        },
    );
    let view = state.session_header_view().expect("header");
    assert_eq!(view.icon, "↻");
    let yellow = state.config.palette.yellow;
    assert_eq!(view.icon_color, yellow);
}

fn companion_info() -> BrowserGetInfo {
    BrowserGetInfo {
        seq: 1,
        enabled: true,
        companion_app: Some(BrowserCompanionAppInfo {
            state: "running".into(),
            ..Default::default()
        }),
        window_off: true,
        sessions: vec![BrowserSessionInfo {
            tab_id: "tab_1".into(),
            title: "herdr+ preview".into(),
            url: "http://localhost:3000".into(),
            pages: 1,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn browser_state() -> ClientShellState {
    let mut state = header_state(&Config::default());
    state.browser.info = Some(companion_info());
    state.browser_dock.extent_for_test = Some((COLS, ROWS, 1600, 800));
    state.compose(COLS, ROWS);
    state
}

fn click(state: &mut ClientShellState, rect: Rect) -> ClientShellInput {
    let at = |kind| {
        RawInputEvent::Mouse(MouseEvent {
            kind,
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::empty(),
        })
    };
    let outcome = state.handle_raw_events(vec![
        at(MouseEventKind::Down(MouseButton::Left)),
        at(MouseEventKind::Up(MouseButton::Left)),
    ]);
    state.compose(COLS, ROWS);
    outcome
}

/// A click on the header control `pick` names.
fn press(
    state: &mut ClientShellState,
    pick: fn(&super::super::session_header::SessionHeaderHits) -> Rect,
) -> ClientShellInput {
    let rect = pick(&state.hits.session_header);
    click(state, rect)
}

#[test]
fn fork_smoke_the_header_sits_on_top_and_its_controls_switch_views_and_docks() {
    let mut state = browser_state();
    let layout = state.layout(COLS, ROWS);
    assert_eq!(layout.session_header.y, 0);
    assert_eq!(layout.session_header.x, layout.sidebar.right());
    assert_eq!(layout.pane_surface.y, 1);
    let hits = state.hits.session_header;
    assert!(!hits.terminal.is_empty() && !hits.browser.is_empty());
    assert!(!hits.browser_dock.is_empty() && !hits.notes_dock.is_empty());
    assert_eq!(hits.notes_dock.right(), COLS, "▤ at the far right");
    assert!(hits.browser_dock.x < hits.notes_dock.x);
    // Browser, then Terminal
    click(&mut state, hits.browser);
    assert!(state.browser_view_open_for("tab_1"));
    press(&mut state, |hits| hits.terminal);
    assert!(!state.browser_view_open_for("tab_1"));
    // ◧ docks the browser: the band takes the dock slot
    let outcome = press(&mut state, |hits| hits.browser_dock);
    assert!(outcome.resize);
    assert!(state.browser_dock_open_for("tab_1"));
    assert!(!state.layout(COLS, ROWS).browser_band.is_empty());
    // ▤ swaps it for the notes dock
    press(&mut state, |hits| hits.notes_dock);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.browser_dock_open_for("tab_1"), "one dock slot");
    let layout = state.layout(COLS, ROWS);
    assert!(layout.browser_band.is_empty() && !layout.info_dock.is_empty());
    // ◧ again swaps back
    press(&mut state, |hits| hits.browser_dock);
    assert!(state.browser_dock_open_for("tab_1"));
    assert!(!state.info_dock_open_for_focused_tab(), "one dock slot");
    // from Browser view, ◧ brings the terminal back with the browser docked
    press(&mut state, |hits| hits.browser_dock);
    assert!(!state.browser_dock_open_for("tab_1"));
    press(&mut state, |hits| hits.browser);
    press(&mut state, |hits| hits.browser_dock);
    assert!(!state.browser_view_open_for("tab_1"));
    assert!(state.browser_dock_open_for("tab_1"));
    // a press on the header never reaches a pane
    let x = state.hits.session_header.area.x + 1;
    let outcome = click(&mut state, Rect::new(x, 0, 1, 1));
    assert!(outcome.requests.is_empty(), "{:?}", outcome.requests);
}

#[test]
fn the_lit_toggle_shows_the_open_dock_and_hover_is_text_colour_only() {
    let mut state = browser_state();
    let palette = state.config.palette.clone();
    let style_at = |state: &mut ClientShellState, rect: Rect| {
        let frame = state.compose(COLS, ROWS).expect("frame").frame;
        let (_, fg, bg, _) = super::tab_sidebar_golden::cell_style(&frame, rect.x, rect.y);
        (fg, bg)
    };
    let fg = |color| crate::protocol::color_to_u32(color);
    let chrome = fg(palette.sidebar_chrome());
    let hits = state.hits.session_header;
    assert_eq!(
        style_at(&mut state, hits.terminal),
        (fg(palette.accent), chrome)
    );
    assert_eq!(
        style_at(&mut state, hits.browser),
        (fg(palette.overlay0), chrome)
    );
    assert_eq!(
        style_at(&mut state, hits.notes_dock),
        (fg(palette.overlay0), chrome)
    );
    state.toggle_info_pane_for_focused_tab(&mut ClientShellInput::default());
    assert_eq!(
        style_at(&mut state, hits.notes_dock),
        (fg(palette.accent), chrome)
    );
    assert_eq!(
        style_at(&mut state, hits.browser_dock),
        (fg(palette.overlay0), chrome)
    );
    // a hovered control turns `text`, the background stays
    let outcome = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: hits.browser.x,
        row: hits.browser.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(outcome.repaint);
    assert_eq!(state.header_hover, Some(HeaderTarget::ViewBrowser));
    assert_eq!(
        style_at(&mut state, hits.browser),
        (fg(palette.text), chrome)
    );
}

#[test]
fn each_dock_type_keeps_its_own_width_across_a_swap() {
    let mut state = browser_state();
    let mut outcome = ClientShellInput::default();
    state.info_dock_width = 50;
    state.browser_dock.percent = 60;
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.compose(COLS, ROWS);
    assert_eq!(state.layout(COLS, ROWS).info_dock.width, 50);
    state.toggle_browser_dock("tab_1".into(), &mut outcome);
    state.compose(COLS, ROWS);
    let layout = state.layout(COLS, ROWS);
    assert!(layout.info_dock.is_empty());
    assert_eq!(
        layout.browser_band.width,
        (COLS - layout.sidebar.width) * 60 / 100
    );
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.compose(COLS, ROWS);
    let layout = state.layout(COLS, ROWS);
    assert!(layout.browser_band.is_empty());
    assert_eq!(layout.info_dock.width, 50);
    assert_eq!(
        (state.info_dock_width, state.browser_dock.percent),
        (50, 60)
    );
}

#[test]
fn the_browser_dock_title_bar_promotes_and_closes() {
    let mut state = browser_state();
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let band = state.layout(COLS, ROWS).browser_band;
    let title: String = (band.x..band.right())
        .map(|x| {
            frame.cells[usize::from(band.y) * usize::from(COLS) + usize::from(x)]
                .symbol
                .clone()
        })
        .collect();
    assert!(title.contains("◧ herdr+ preview"), "{title}");
    assert!(title.trim_end().ends_with("⤢  ✕"), "{title}");
    // the companion covers the band below its title bar
    let spec = state
        .take_browser_placement_request(std::time::Instant::now())
        .and_then(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } => {
                serde_json::from_str::<crate::api::schema::Request>(&request).ok()
            }
            _ => None,
        })
        .and_then(|request| match request.method {
            crate::api::schema::Method::BrowserPlacementSet(params) => params.placement,
            _ => None,
        })
        .expect("a placement");
    assert_eq!(
        (spec.area.y, spec.area.height),
        (band.y + 1, band.height - 1)
    );
    let target = |state: &ClientShellState, wanted| {
        state
            .hits
            .browser_dock
            .targets
            .iter()
            .find(|(_, target)| *target == wanted)
            .map(|(rect, _)| *rect)
            .expect("drawn")
    };
    use super::super::browser_dock::BrowserDockTarget;
    // ⤢: Browser view, the dock slot freed
    let promote = target(&state, BrowserDockTarget::Promote);
    click(&mut state, promote);
    assert!(state.browser_view_open_for("tab_1"));
    assert!(!state.browser_dock_open_for("tab_1"));
    // ✕ closes
    press(&mut state, |hits| hits.terminal);
    press(&mut state, |hits| hits.browser_dock);
    assert!(state.browser_dock_open_for("tab_1"));
    let close = target(&state, BrowserDockTarget::Close);
    click(&mut state, close);
    assert!(!state.browser_dock_open_for("tab_1"));
}

#[test]
fn browser_view_keeps_the_notes_dock_beside_the_page() {
    let mut state = browser_state();
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.set_browser_view("tab_1".into(), true, &mut outcome);
    state.compose(COLS, ROWS);
    let layout = state.layout(COLS, ROWS);
    let full = super::super::browser_dock::full_area(&layout);
    assert!(!layout.info_dock.is_empty());
    assert!(
        !full.intersects(layout.info_dock),
        "{full:?} {:?}",
        layout.info_dock
    );
    assert_eq!(full.right(), layout.info_dock.x);
    assert!(
        !state.hits.info_dock.area.is_empty(),
        "the notes dock stays clickable"
    );
}

#[test]
fn the_spaces_layout_keeps_its_view_strip_and_has_no_header() {
    let mut state = header_state(&spaces_config());
    state.browser.info = Some(companion_info());
    state.compose(COLS, ROWS);
    let layout = state.layout(COLS, ROWS);
    assert!(layout.session_header.is_empty());
    assert!(
        !layout.view_strip.is_empty(),
        "the tab has pages: the strip shows"
    );
    assert_eq!(state.hits.session_header, Default::default());
    // both docks still fit side by side there
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.toggle_browser_dock("tab_1".into(), &mut outcome);
    state.compose(COLS, ROWS);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(state.browser_dock_open_for("tab_1"));
    let layout = state.layout(COLS, ROWS);
    assert!(!layout.info_dock.is_empty() && !layout.browser_band.is_empty());
}

/// Audit: a live switch from the spaces layout (both docks side by side) to
/// the tabs layout leaves one dock per tab: the browser stays, the notes
/// close.
#[test]
fn a_switch_to_the_tabs_layout_keeps_the_browser_and_closes_the_notes() {
    let mut state = header_state(&spaces_config());
    state.browser.info = Some(companion_info());
    state.browser_dock.extent_for_test = Some((COLS, ROWS, 1600, 800));
    state.compose(COLS, ROWS);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.toggle_browser_dock("tab_1".into(), &mut outcome);
    assert!(state.info_dock_open_for_focused_tab() && state.browser_dock_open_for("tab_1"));
    state.config.sidebar_layout = crate::config::SidebarLayoutConfig::Tabs;
    state.reconcile_dock_slot();
    assert!(
        state.browser_dock_open_for("tab_1"),
        "the browser keeps the slot"
    );
    assert!(!state.info_dock_open_for_focused_tab(), "the notes close");
    state.compose(COLS, ROWS);
    let layout = state.layout(COLS, ROWS);
    assert!(!layout.browser_band.is_empty() && layout.info_dock.is_empty());
}

fn notice_key(state: &ClientShellState) -> Option<String> {
    state
        .visible_endpoint_notice
        .as_ref()
        .map(|notice| format!("{:?}", notice.key))
}

/// Audit: a swap gives the slot away only to a dock that fits the window;
/// otherwise the open dock stays and the "too narrow" notice says why.
#[test]
fn a_swap_keeps_the_open_dock_when_the_other_does_not_fit() {
    // 26-column sidebar, a 65-column pane area: the notes (28 + 30) fit,
    // the band (40 + 30) does not.
    let cols = 91;
    let mut state = header_state(&Config::default());
    state.browser.info = Some(companion_info());
    state.browser_dock.extent_for_test = Some((cols, ROWS, 900, 800));
    state.compose(cols, ROWS);
    let mut outcome = ClientShellInput::default();
    state.toggle_info_pane("tab_1".into(), &mut outcome);
    state.compose(cols, ROWS);
    assert!(!state.layout(cols, ROWS).info_dock.is_empty());
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    assert!(state.info_dock_open_for_focused_tab(), "the notes stay");
    assert!(!state.browser_dock_open_for("tab_1"));
    assert!(
        notice_key(&state).is_some_and(|key| key.contains("browser_dock:narrow")),
        "{:?}",
        notice_key(&state)
    );

    // The other way: a 50-column pane area keeps a docked browser listed
    // (its band waits for room) and the notes (58) do not fit either.
    let cols = 76;
    let mut state = header_state(&Config::default());
    state.browser.info = Some(companion_info());
    state.browser_dock.extent_for_test = Some((cols, ROWS, 760, 800));
    state.compose(cols, ROWS);
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    state.compose(cols, ROWS);
    assert!(state.browser_dock_open_for("tab_1"));
    state.visible_endpoint_notice = None;
    state.toggle_info_pane("tab_1".into(), &mut ClientShellInput::default());
    assert!(state.browser_dock_open_for("tab_1"), "the browser stays");
    assert!(!state.info_dock_open_for_focused_tab());
    assert!(
        notice_key(&state).is_some_and(|key| key.contains("info_pane:narrow")),
        "{:?}",
        notice_key(&state)
    );
}

/// An agent's notes reveal never evicts a browser the user docked on that
/// tab: the reveal is skipped and the notes stay closed; without the dock
/// it opens the notes as before.
#[test]
fn a_notes_reveal_leaves_a_docked_browser_alone() {
    use crate::server::headless::notes_reveal::NotesRevealPayload;
    let reveal = |revision| NotesRevealPayload {
        boot_id: "boot-1".into(),
        revision,
        tab_id: "tab_1".into(),
    };
    let mut state = browser_state();
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    state.compose(COLS, ROWS);
    state.receive_notes_reveal(&ClientEndpointId::Local, reveal(1));
    assert!(
        state.browser_dock_open_for("tab_1"),
        "the user's dock stays"
    );
    assert!(
        !state.info_dock_open_for_focused_tab(),
        "the notes stay closed"
    );
    // the dock closed: the next reveal opens the notes
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    state.receive_notes_reveal(&ClientEndpointId::Local, reveal(2));
    assert!(state.info_dock_open_for_focused_tab());
}
