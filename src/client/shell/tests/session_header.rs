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
        group: None,
        name,
        name_color: palette.text,
        context: Some(("82%", palette.yellow)),
        subagents: 3,
        mail: Some(("✉1", palette.overlay1)),
        browser: Some(HeaderBrowser {
            viewing: false,
            docked: false,
            pages: 0,
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
            pages: 0,
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
            pages: 0,
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
    let grouped = SessionHeader {
        icon: "○",
        icon_color: palette.green,
        group: Some("Search It"),
        ..header("local clean up", &palette)
    };
    // whole, shortened, dropped
    for width in [80, 62, 59] {
        section("in a group", &grouped, width);
    }
    // the page count badge after `Browser`
    let with_pages = |pages: u32, viewing: bool| SessionHeader {
        browser: Some(HeaderBrowser {
            viewing,
            docked: false,
            pages,
        }),
        ..header("header-design-and-navigation", &palette)
    };
    section("7 pages", &with_pages(7, false), 110);
    section("12 pages", &with_pages(12, false), 110);
    section("150 pages", &with_pages(150, false), 110);
    section("7 pages, browser view", &with_pages(7, true), 110);
    section("7 pages, short form", &with_pages(7, false), 37);
    section("12 pages, toggle only", &with_pages(12, false), 26);
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
    assert!(wide.contains("Terminal  Browser ◨  ≡"), "{wide}");
    // the name gives way while it keeps MIN_NAME cells
    let mid = text(&header, 52);
    assert!(mid.contains("Terminal  Browser ◨  ≡"), "{mid}");
    assert!(mid.contains('…') && !mid.contains("navigation"), "{mid}");
    // then the short tabs
    let narrow = text(&header, 36);
    assert!(narrow.contains("T  B ◨  ≡"), "{narrow}");
    assert!(!narrow.contains("Terminal"), "{narrow}");
    // then the dock toggle alone; ≡ is always the last cell before the margin
    for width in [26u16, 20, 12, 6] {
        let row = text(&header, width);
        assert!(row.trim_end().ends_with('≡'), "{width}: {row:?}");
        if width >= 12 {
            assert!(row.contains('◨'), "{width}: {row:?}");
        }
    }
}

/// `header` with the tab's browser at `pages` (Browser view on or off).
fn paged<'a>(
    name: &'a str,
    palette: &crate::app::state::Palette,
    pages: u32,
    viewing: bool,
) -> SessionHeader<'a> {
    SessionHeader {
        browser: Some(HeaderBrowser {
            viewing,
            docked: false,
            pages,
        }),
        ..header(name, palette)
    }
}

/// `header` drawn on a fresh buffer: the buffer and the hits.
fn drawn(
    header: &SessionHeader<'_>,
    width: u16,
) -> (Buffer, super::super::session_header::SessionHeaderHits) {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    let hits = session_header::render(&mut buffer, area, header, &palette);
    (buffer, hits)
}

#[test]
fn the_browser_button_shows_the_page_count_in_a_badge() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let name = "header-design-and-navigation";
    // none at 0
    let row = text(&paged(name, &palette, 0, false), 110);
    assert!(row.contains("Terminal  Browser ◨  ≡"), "{row}");
    // ` 7 ` after a blank; 10+ one cell wider; 99+ from 100
    for (pages, label) in [
        (1, "Browser  1  ◨  ≡"),
        (7, "Browser  7  ◨  ≡"),
        (12, "Browser  12  ◨  ≡"),
        (99, "Browser  99  ◨  ≡"),
        (100, "Browser  99+  ◨  ≡"),
        (4321, "Browser  99+  ◨  ≡"),
    ] {
        let row = text(&paged(name, &palette, pages, false), 110);
        assert!(row.contains(label), "{pages}: {row}");
    }
    // the short form: the bare count right after `B`
    let row = text(&paged(name, &palette, 7, false), 37);
    assert!(row.contains("T  B7 ◨  ≡"), "{row}");
    let row = text(&paged(name, &palette, 12, false), 38);
    assert!(row.contains("T  B12 ◨  ≡"), "{row}");
    // only `◨` left: no count
    let row = text(&paged(name, &palette, 12, false), 26);
    assert!(row.trim_end().ends_with("◨  ≡"), "{row}");
    assert!(!row.contains("12"), "{row}");
}

#[test]
fn the_badge_is_grey_and_accent_while_browser_view_is_on() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let chrome = palette.sidebar_chrome();
    let name = "header-design-and-navigation";
    for (viewing, fg, bg) in [
        (false, palette.text, palette.surface1),
        (true, chrome, palette.accent),
    ] {
        let (buffer, hits) = drawn(&paged(name, &palette, 12, viewing), 110);
        // `Browser` then a blank on the chrome, then ` 12 ` as one block
        let label_end = hits.browser.x + 7;
        assert_eq!(buffer[(label_end, 0)].bg, chrome, "the blank");
        let block: String = (label_end + 1..label_end + 5)
            .map(|x| buffer[(x, 0)].symbol().to_owned())
            .collect();
        assert_eq!(block, " 12 ");
        for x in label_end + 1..label_end + 5 {
            let cell = &buffer[(x, 0)];
            assert_eq!((cell.fg, cell.bg), (fg, bg), "viewing {viewing}, x {x}");
        }
        assert_eq!(buffer[(label_end + 5, 0)].bg, chrome, "after the block");
        // hover leaves the badge alone
        let hovered = SessionHeader {
            hover: Some(HeaderTarget::ViewBrowser),
            ..paged(name, &palette, 12, viewing)
        };
        let (buffer, _) = drawn(&hovered, 110);
        let cell = &buffer[(label_end + 2, 0)];
        assert_eq!((cell.fg, cell.bg), (fg, bg), "hovered, viewing {viewing}");
    }
    // the short form's count is a block too
    let (buffer, hits) = drawn(&paged(name, &palette, 7, false), 37);
    let cell = &buffer[(hits.browser.x + 1, 0)];
    assert_eq!(cell.symbol(), "7");
    assert_eq!((cell.fg, cell.bg), (palette.text, palette.surface1));
}

#[test]
fn the_badge_is_part_of_the_browser_button_and_the_dock_toggle_moves_right() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let name = "header-design-and-navigation";
    let (_, plain) = drawn(&paged(name, &palette, 0, false), 110);
    assert_eq!(plain.browser.width, 7);
    for (pages, badge, width) in [(7, 4, 110), (12, 5, 110), (150, 6, 110), (7, 1, 37)] {
        let (_, hits) = drawn(&paged(name, &palette, pages, false), width);
        let label = if width == 110 { 7 } else { 1 };
        assert_eq!(hits.browser.width, label + badge, "{pages} @ {width}");
        assert_eq!(
            hits.browser_dock.x,
            hits.browser.right() + 1,
            "{pages} @ {width}"
        );
        assert_eq!(hits.browser_dock.width, 1);
        assert_eq!(hits.notes_dock.x, hits.browser_dock.x + 3);
        assert!(hits.terminal.right() + 2 == hits.browser.x);
        // every badge cell is the Browser button
        for x in hits.browser.x..hits.browser.right() {
            assert_eq!(
                hits.resolve((x, 0)),
                Some(HeaderTarget::ViewBrowser),
                "x {x}"
            );
        }
        assert_eq!(hits.resolve((hits.browser.right(), 0)), None, "the gap");
        assert_eq!(
            hits.resolve((hits.browser_dock.x, 0)),
            Some(HeaderTarget::BrowserDock)
        );
        // the banner slot ends before the view tabs
        assert!(hits.banner.is_empty() || hits.banner.right() < hits.terminal.x);
    }
}

#[test]
fn the_header_counts_the_tabs_pages_and_hides_them_while_the_companion_is_stopped() {
    let mut state = browser_state();
    let set = |state: &mut ClientShellState, companion: &str, pages: u32| {
        let info = state.browser.info.as_mut().expect("info");
        info.companion_app.as_mut().expect("companion").state = companion.into();
        info.sessions[0].pages = pages;
        state.compose(COLS, ROWS);
    };
    let pages = |state: &ClientShellState| {
        state
            .session_header_view()
            .and_then(|header| header.browser)
            .map(|browser| browser.pages)
    };
    set(&mut state, "running", 7);
    assert_eq!(pages(&state), Some(7));
    // a click on the badge opens Browser view
    let hits = state.hits.session_header;
    assert_eq!(hits.browser.width, 7 + 4);
    click(
        &mut state,
        Rect::new(hits.browser.right() - 2, hits.browser.y, 1, 1),
    );
    assert!(state.browser_view_open_for("tab_1"));
    for stopped in ["stopped", "not_installed", "failed"] {
        set(&mut state, stopped, 3);
        assert_eq!(pages(&state), Some(0), "{stopped}");
        assert_eq!(state.hits.session_header.browser.width, 7, "{stopped}");
    }
    set(&mut state, "starting", 3);
    assert_eq!(pages(&state), Some(3));
    // another tab's pages are not this tab's
    let info = state.browser.info.as_mut().expect("info");
    info.sessions[0].tab_id = "tab_9".into();
    assert_eq!(pages(&state), Some(0));
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
            // the compact bar's companion (its bar has expand and close)
            version: Some("0.2.4".into()),
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
    assert_eq!(hits.notes_dock.right(), COLS, "≡ at the far right");
    assert!(hits.browser_dock.x < hits.notes_dock.x);
    // Browser, then Terminal
    click(&mut state, hits.browser);
    assert!(state.browser_view_open_for("tab_1"));
    press(&mut state, |hits| hits.terminal);
    assert!(!state.browser_view_open_for("tab_1"));
    // ◨ docks the browser: the band takes the dock slot
    let outcome = press(&mut state, |hits| hits.browser_dock);
    assert!(outcome.resize);
    assert!(state.browser_dock_open_for("tab_1"));
    assert!(!state.layout(COLS, ROWS).browser_band.is_empty());
    // ≡ swaps it for the notes dock
    press(&mut state, |hits| hits.notes_dock);
    assert!(state.info_dock_open_for_focused_tab());
    assert!(!state.browser_dock_open_for("tab_1"), "one dock slot");
    let layout = state.layout(COLS, ROWS);
    assert!(layout.browser_band.is_empty() && !layout.info_dock.is_empty());
    // ◨ again swaps back
    press(&mut state, |hits| hits.browser_dock);
    assert!(state.browser_dock_open_for("tab_1"));
    assert!(!state.info_dock_open_for_focused_tab(), "one dock slot");
    // from Browser view, ◨ brings the terminal back with the browser docked
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
    // Fork (compact bar): the title bar is the fallback of a band the
    // companion does not cover (here it cannot dock to this terminal)
    report_placement(&mut state, "unsupported");
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let band = state.layout(COLS, ROWS).browser_band;
    let title: String = (band.x..band.right())
        .map(|x| {
            frame.cells[usize::from(band.y) * usize::from(COLS) + usize::from(x)]
                .symbol
                .clone()
        })
        .collect();
    assert!(title.contains("◨ herdr+ preview"), "{title}");
    assert!(title.trim_end().ends_with("⤢  ✕"), "{title}");
    // the placement is the whole band right of the divider, title or not
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
    assert_eq!((spec.area.y, spec.area.height), (band.y, band.height));
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

/// Fork (compact bar): the tab bar's dock toggles, the view strip's terminal
/// mark and the menus' Open mark are single-cell glyphs the JetBrains Mono
/// face draws itself (the old `▤` / `▣` came from a wider fallback face).
/// `≡` is East Asian Width "ambiguous": herdr counts it as one cell.
#[test]
fn the_dock_and_view_glyphs_are_one_cell_native_glyphs() {
    let glyphs = [
        (session_header::BROWSER_DOCK_GLYPH, '\u{25E8}'),
        (session_header::NOTES_DOCK_GLYPH, '\u{2261}'),
        (
            super::super::browser_dock_render::TERMINAL_GLYPH,
            '\u{25EB}',
        ),
        (super::super::browser_dock_render::DOCK_GLYPH, '\u{25E8}'),
        (super::super::context_menu::GLYPH_OPEN, '\u{229F}'),
    ];
    for (glyph, expected) in glyphs {
        assert_eq!(glyph.chars().collect::<Vec<_>>(), vec![expected], "{glyph}");
        assert_eq!(super::super::render::display_width(glyph), 1, "{glyph}");
    }
}

/// The host's report on this client's placement, as `browser.get` carries it.
fn report_placement(state: &mut ClientShellState, report: &str) {
    let id = state.browser_dock.placement_id.clone();
    if let Some(info) = state.browser.info.as_mut() {
        info.placements = vec![crate::api::schema::BrowserPlacementStatus {
            id,
            state: report.into(),
            reason: None,
        }];
    }
}

/// Fork (compact bar): over a band the companion covers, herdr draws no
/// title bar (the companion's bar has expand and close), and the placement
/// keeps the same area either way, so the host's report never moves it.
#[test]
fn fork_smoke_a_covered_band_has_no_title_bar_and_the_same_placement() {
    use super::super::browser_dock::BrowserDockTarget;
    let mut state = browser_state();
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    let placement = |state: &mut ClientShellState| {
        state.compose(COLS, ROWS);
        state.browser_placement_spec().expect("a placement")
    };
    report_placement(&mut state, "unbound");
    let uncovered = placement(&mut state);
    report_placement(&mut state, "covering");
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let band = state.layout(COLS, ROWS).browser_band;
    let row: String = (band.x + 1..band.right())
        .map(|x| {
            frame.cells[usize::from(band.y) * usize::from(COLS) + usize::from(x)]
                .symbol
                .clone()
        })
        .collect();
    assert!(
        !row.contains("herdr+ preview") && !row.contains('✕'),
        "{row:?}"
    );
    assert!(!state
        .hits
        .browser_dock
        .targets
        .iter()
        .any(|(_, target)| matches!(
            target,
            BrowserDockTarget::Promote | BrowserDockTarget::Close
        )));
    let covered = placement(&mut state);
    assert_eq!(covered.area, uncovered.area);
    assert_eq!((covered.area.y, covered.area.height), (band.y, band.height));
}

/// Fork (compact bar): the companion's expand, restore and close, relayed
/// by the server (`endpoint.placement-action.v1`), act like the band's
/// own buttons, once per relay, and only on this client's placement.
#[test]
fn fork_smoke_a_placement_action_promotes_restores_and_closes_the_dock() {
    use crate::api::schema::BrowserPlacementAction as Action;
    use crate::server::headless::placement_action::PlacementActionPayload;
    let mut state = browser_state();
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    let id = state.browser_dock.placement_id.clone();
    let endpoint = crate::client::endpoint::ClientEndpointId::Local;
    let send = |state: &mut ClientShellState, revision, id: &str, action| {
        state.receive_browser_placement_action(
            &endpoint,
            PlacementActionPayload {
                boot_id: "boot".into(),
                revision,
                id: id.into(),
                tab_id: "tab_1".into(),
                action,
            },
        )
    };
    let docks = |state: &ClientShellState| {
        (
            state.browser_view_open_for("tab_1"),
            state.browser_dock_open_for("tab_1"),
        )
    };
    // expand: Browser view, the dock slot freed
    assert!(send(&mut state, 1, &id, Action::Promote).repaint);
    assert_eq!(docks(&state), (true, false));
    // the same relay twice acts once
    send(&mut state, 1, &id, Action::Restore);
    assert_eq!(docks(&state), (true, false));
    // restore: the terminal comes back with the browser docked
    send(&mut state, 2, &id, Action::Restore);
    assert_eq!(docks(&state), (false, true));
    // another client's placement: nothing
    send(&mut state, 3, "pl-someone-else", Action::Close);
    assert_eq!(docks(&state), (false, true));
    // close: the dock goes
    send(&mut state, 4, &id, Action::Close);
    assert_eq!(docks(&state), (false, false));
    // close in Browser view: back to the terminal
    state.set_browser_view("tab_1".into(), true, &mut ClientShellInput::default());
    send(&mut state, 5, &id, Action::Close);
    assert_eq!(docks(&state), (false, false));
    // the relay as received from any endpoint, boot and tab
    let relay = |state: &mut ClientShellState,
                 endpoint: &crate::client::endpoint::ClientEndpointId,
                 boot: &str,
                 revision,
                 tab: &str,
                 action| {
        state.receive_browser_placement_action(
            endpoint,
            PlacementActionPayload {
                boot_id: boot.into(),
                revision,
                id: id.clone(),
                tab_id: tab.into(),
                action,
            },
        )
    };
    // a tab that is no longer the focused one: nothing
    relay(&mut state, &endpoint, "boot", 6, "tab_2", Action::Promote);
    assert_eq!(docks(&state), (false, false));
    assert!(!state.browser_view_open_for("tab_2"));
    // another endpoint (an SSH server's relay): nothing
    let remote = crate::client::endpoint::ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
            .expect("profile id"),
    );
    relay(&mut state, &remote, "boot", 7, "tab_1", Action::Promote);
    assert_eq!(docks(&state), (false, false));
    // a new server boot (a live handoff) counts from revision 1 again: it acts
    state.set_browser_view("tab_1".into(), true, &mut ClientShellInput::default());
    relay(&mut state, &endpoint, "boot-2", 1, "tab_1", Action::Restore);
    assert_eq!(docks(&state), (false, true));
}

/// Fork (compact bar): a running companion that predates the compact bar
/// (no expand or close in its bar) keeps the band's title bar over a band it
/// covers, and its area starts below that row; 0.2.4 and newer do not.
#[test]
fn fork_smoke_an_older_companion_keeps_the_band_title_bar_over_a_covered_band() {
    use super::super::browser_dock::BrowserDockTarget;
    let mut state = browser_state();
    state.toggle_browser_dock("tab_1".into(), &mut ClientShellInput::default());
    report_placement(&mut state, "covering");
    let set_version = |state: &mut ClientShellState, version: Option<&str>| {
        if let Some(companion) = state
            .browser
            .info
            .as_mut()
            .and_then(|info| info.companion_app.as_mut())
        {
            companion.version = version.map(str::to_owned);
        }
    };
    let drawn = |state: &mut ClientShellState| {
        state.compose(COLS, ROWS);
        let promote = state
            .hits
            .browser_dock
            .targets
            .iter()
            .any(|(_, target)| *target == BrowserDockTarget::Promote);
        let spec = state.browser_placement_spec().expect("a placement");
        (promote, spec.area.y, spec.area.height)
    };
    let band = state.layout(COLS, ROWS).browser_band;
    for (version, title) in [
        (Some("0.2.3"), true),
        (None, true),
        (Some("0.2.4"), false),
        (Some("0.3.0-dev"), false),
    ] {
        set_version(&mut state, version);
        let (promote, y, height) = drawn(&mut state);
        assert_eq!(promote, title, "{version:?}");
        let top = u16::from(title);
        assert_eq!(
            (y, height),
            (band.y + top, band.height - top),
            "{version:?}"
        );
    }
}

#[test]
fn a_tab_in_a_group_names_it_dim_before_the_name() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let grouped = SessionHeader {
        group: Some("Search It"),
        ..header("local clean up", &palette)
    };
    let wide = text(&grouped, 110);
    assert!(
        wide.contains("● Search It · local clean up  82% ⚭3 ✉1"),
        "{wide}"
    );
    assert!(wide.contains("Terminal  Browser ◨  ≡"), "{wide}");
    // the group is overlay1 and not bold; the name keeps its colour and bold
    let area = Rect::new(0, 0, 110, 1);
    let mut buffer = Buffer::empty(area);
    session_header::render(&mut buffer, area, &grouped, &palette);
    assert_eq!(buffer[(3, 0)].fg, palette.overlay1);
    assert!(!buffer[(3, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(13, 0)].fg, palette.overlay1);
    assert_eq!(buffer[(15, 0)].fg, palette.text);
    assert!(buffer[(15, 0)].modifier.contains(Modifier::BOLD));
    // no group: the row is the same as before
    let plain = header("local clean up", &palette);
    assert!(text(&plain, 110).contains("● local clean up  82%"));
}

#[test]
fn a_narrow_row_shortens_the_group_then_drops_it_and_keeps_the_name() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let grouped = SessionHeader {
        group: Some("Search It"),
        ..header("local clean up", &palette)
    };
    let plain = header("local clean up", &palette);
    for width in [110u16, 72, 65, 64, 62, 60, 59, 52, 44, 36, 26, 12] {
        let row = text(&grouped, width);
        let without = text(&plain, width);
        // the controls and the facts sit where they sit without a group
        assert_eq!(
            row.find("◨").map(|at| row[at..].to_owned()),
            without.find("◨").map(|at| without[at..].to_owned()),
            "{width}: {row:?}"
        );
        assert_eq!(row.contains("82%"), without.contains("82%"), "{width}");
        // the name is never shorter than without the group
        let name_of = |row: &str| {
            row.split(" · ")
                .last()
                .and_then(|rest| rest.trim_start_matches(['●', ' ']).split("  ").next())
                .map(str::to_owned)
                .unwrap_or_default()
        };
        assert_eq!(name_of(&row), name_of(&without), "{width}: {row:?}");
    }
    // whole, then shortened with an ellipsis, then gone
    assert!(text(&grouped, 72).contains("● Search It · local clean up"));
    let short = text(&grouped, 62);
    assert!(short.contains("● Searc… · local clean up"), "{short:?}");
    let gone = text(&grouped, 59);
    assert!(gone.contains("● local clean up  82%"), "{gone:?}");
    assert!(!gone.contains('·') && !gone.contains("Sea"), "{gone:?}");
}

#[test]
fn the_header_names_the_focused_tabs_group_and_its_team_purpose() {
    let config = Config::default();
    let mut state = header_state(&config);
    // the first space is no group
    assert_eq!(
        state.session_header_view().map(|view| view.group),
        Some(None)
    );
    let mut snapshot = state.snapshot.as_deref().cloned().expect("snapshot");
    let mut group = snapshot.workspaces[0].clone();
    group.workspace_id = "ws_2".into();
    group.label = "Marla".into();
    group.focused = false;
    snapshot.workspaces.push(group);
    snapshot.tabs[0].workspace_id = "ws_2".into();
    let boot_id = snapshot.boot_id.clone();
    state.set_snapshot(Box::new(snapshot));
    let view = state.session_header_view().expect("header");
    assert_eq!((view.group, view.name), (Some("Marla"), "restart screen"));
    // a team names the group by its purpose, as the sidebar header does
    let team = crate::api::schema::team::TeamInfo {
        workspace_id: "ws_2".into(),
        workspace_label: "Marla".into(),
        purpose: Some("Search It".into()),
        purpose_by: None,
        created_unix: 0,
        revision: 1,
        members: Vec::new(),
        excluded: Vec::new(),
    };
    state.teams.insert(
        ClientEndpointId::Local,
        super::super::teams::ClientTeamsState::from_payload(
            crate::server::headless::teams::TeamsPayload {
                boot_id,
                revision: 1,
                teams: vec![team],
            },
        ),
    );
    let view = state.session_header_view().expect("header");
    assert_eq!(view.group, Some("Search It"));
    let frame = state.compose(COLS, ROWS).expect("frame").frame;
    let row = super::tab_sidebar_golden::sidebar_lines(&frame, COLS, 0, 1).remove(0);
    assert!(row.contains("Search It · restart screen"), "{row}");
}

#[test]
fn in_browser_view_the_group_leaves_the_notice_line_where_it_is_without_one() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let viewing = |group| SessionHeader {
        group,
        browser: Some(HeaderBrowser {
            viewing: true,
            docked: false,
            pages: 0,
        }),
        ..header("local clean up", &palette)
    };
    let hits = |header: &SessionHeader<'_>, width: u16| {
        let area = Rect::new(0, 0, width, 1);
        let mut buffer = Buffer::empty(area);
        let hits = session_header::render(&mut buffer, area, header, &palette);
        let row = (0..width)
            .map(|x| buffer[(x, 0)].symbol().to_owned())
            .collect::<String>();
        (hits, row)
    };
    let (grouped, plain) = (viewing(Some("Search It")), viewing(None));
    for width in 40u16..=110 {
        let (with, row) = hits(&grouped, width);
        let (without, _) = hits(&plain, width);
        if row.contains("Sea") {
            // the group only moves the notice line's start: it stays after
            // the facts (never over the group or the name) where it was
            // after them without a group, with at least BANNER_MIN cells
            // and the same right edge
            assert!(without.banner.x > 3, "{width}: {row:?}");
            assert!(with.banner.x > 3, "{width}: {row:?}");
            assert!(with.banner.width >= 16, "{width}: {row:?}");
            assert_eq!(with.banner.right(), without.banner.right(), "{width}");
        } else {
            // dropped: the notice line is the one without a group
            assert_eq!(with.banner, without.banner, "{width}: {row:?}");
        }
    }
    // a wide row still names the group, with the notice line after the facts
    let (wide, row) = hits(&grouped, 110);
    assert!(row.contains("Search It · local clean up"), "{row:?}");
    assert!(wide.banner.width >= 16, "{wide:?}");
}

#[test]
fn a_wide_character_cut_leaves_no_blank_before_the_separator() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let grouped = SessionHeader {
        group: Some("検索検索検索"),
        context: None,
        subagents: 0,
        mail: None,
        browser: None,
        ..header("name", &palette)
    };
    // the name's room is width-7 and the group gets width-7-4-3 cells: 6 at
    // width 20, where `検索` and `…` fill only 5 (the third 検 does not fit
    // the 5 cells before the `…`); the separator follows the `…` directly
    // (`text` shows a wide character's second cell as a blank)
    let row = text(&grouped, 20);
    assert!(row.contains("● 検 索 … · name"), "{row:?}");
    assert!(!row.contains("…  ·"), "{row:?}");
    // 7 cells at width 21 cut cleanly
    let row = text(&grouped, 21);
    assert!(row.contains("● 検 索 検 … · name"), "{row:?}");
}

#[test]
fn the_group_and_the_page_badge_share_the_header() {
    let palette = ClientShellConfig::from_config(&Config::default()).palette;
    let make = |group, pages, viewing| SessionHeader {
        group,
        browser: Some(HeaderBrowser {
            viewing,
            docked: false,
            pages,
        }),
        ..header("local clean up", &palette)
    };
    let row_of = |buffer: &Buffer, width: u16| {
        (0..width)
            .map(|x| buffer[(x, 0)].symbol().to_owned())
            .collect::<String>()
    };
    let name_of = |row: &str| {
        row.split(" · ")
            .last()
            .and_then(|rest| rest.trim_start_matches(['●', ' ']).split("  ").next())
            .map(str::to_owned)
            .unwrap_or_default()
    };
    for viewing in [false, true] {
        let grouped = make(Some("Search It"), 12, viewing);
        let plain = make(None, 12, viewing);
        for width in 12u16..=120 {
            let (buffer, with) = drawn(&grouped, width);
            let (plain_buffer, without) = drawn(&plain, width);
            let (row, plain_row) = (row_of(&buffer, width), row_of(&plain_buffer, width));
            // the badge, the view tabs and the toggles sit where they sit
            // without a group, and so do their hits
            assert_eq!(
                row.find("◨").map(|at| row[at..].to_owned()),
                plain_row.find("◨").map(|at| plain_row[at..].to_owned()),
                "{viewing} {width}: {row:?}"
            );
            assert_eq!(
                (
                    with.terminal,
                    with.browser,
                    with.browser_dock,
                    with.notes_dock
                ),
                (
                    without.terminal,
                    without.browser,
                    without.browser_dock,
                    without.notes_dock
                ),
                "{viewing} {width}"
            );
            // the facts and the name are the ones without a group
            assert_eq!(row.contains("82%"), plain_row.contains("82%"), "{width}");
            assert_eq!(name_of(&row), name_of(&plain_row), "{width}: {row:?}");
            // a group never reaches the view tabs or the badge
            if row.contains("Sea") && !with.terminal.is_empty() {
                let group_end = row.find(" · ").map(|at| row[..at].chars().count());
                assert!(
                    group_end.is_some_and(|end| (end as u16) < with.terminal.x),
                    "{width}: {row:?}"
                );
            }
            // the notice line stays left of the view tabs and the badge
            let first_control = if with.terminal.is_empty() {
                with.browser_dock.x
            } else {
                with.terminal.x
            };
            assert!(
                with.banner.is_empty() || with.banner.right() < first_control,
                "{viewing} {width}: {with:?}"
            );
            // (the notice line is used in Browser view only)
            if !viewing {
                continue;
            }
            if row.contains("Sea") {
                if !with.banner.is_empty() {
                    assert!(with.banner.width >= 16, "{width}: {with:?}");
                    assert!(with.banner.x > 3, "{width}: {with:?}");
                    assert_eq!(with.banner.right(), without.banner.right(), "{width}");
                }
            } else {
                assert_eq!(with.banner, without.banner, "{width}: {row:?}");
            }
        }
    }
    // the badge's five cells take the group's room first: at a width where
    // the group shows whole without pages it shortens with them, and where
    // it shortens without pages it drops with them
    let state = |pages, width| {
        let row = text(&make(Some("Search It"), pages, false), width);
        if row.contains("Search It · ") {
            "whole"
        } else if row.contains("… · ") {
            "short"
        } else {
            assert!(!row.contains('·') && !row.contains("Sea"), "{row:?}");
            "gone"
        }
    };
    let widths = 40u16..=120;
    let shortened = widths
        .clone()
        .find(|&width| state(0, width) == "whole" && state(12, width) == "short");
    let dropped = widths
        .clone()
        .find(|&width| state(0, width) == "short" && state(12, width) == "gone");
    assert!(
        shortened.is_some() && dropped.is_some(),
        "{shortened:?} {dropped:?}"
    );
    let (shortened, dropped) = (shortened.unwrap_or(0), dropped.unwrap_or(0));
    let row = text(&make(Some("Search It"), 12, false), shortened);
    assert!(row.contains("Browser  12  ◨  ≡"), "{row:?}");
    let row = text(&make(Some("Search It"), 12, false), dropped);
    assert!(row.contains("● local clean up  82%"), "{row:?}");
    assert!(row.contains("Browser  12  ◨  ≡"), "{row:?}");
}
