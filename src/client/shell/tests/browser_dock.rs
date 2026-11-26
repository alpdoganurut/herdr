//! The browser dock and Browser view (fork, companion launcher): the band
//! on the right, its divider, the view strip, Browser view over the pane
//! area, the key guard, the placeholder and the placement this client
//! publishes (`browser.placement.set`).

use super::*;
use crate::api::schema::{
    BrowserCompanionAppInfo, BrowserGetInfo, BrowserPlacementKind, BrowserPlacementStatus,
    BrowserSessionInfo, Method, Request,
};
use crate::config::Config;
use crate::input::TerminalKey;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

const COLS: u16 = 160;
const ROWS: u16 = 45;

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

fn two_tab_snapshot(focused: &str) -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.tabs = vec![
        tab("tab_1", "planner", focused == "tab_1"),
        tab("tab_2", "scratch", focused == "tab_2"),
    ];
    snapshot.focused_tab_id = Some(focused.into());
    snapshot
}

fn companion(state: &str) -> BrowserGetInfo {
    BrowserGetInfo {
        seq: 1,
        enabled: true,
        companion_app: Some(BrowserCompanionAppInfo {
            state: state.into(),
            ..Default::default()
        }),
        window_off: true,
        ..Default::default()
    }
}

fn dock_state() -> ClientShellState {
    dock_state_with(&Config::default())
}

/// The spaces layout keeps the view strip (the tabs layout's header
/// carries its controls).
fn spaces_config() -> Config {
    let mut config = Config::default();
    config.ui.sidebar_layout = crate::config::SidebarLayoutConfig::Spaces;
    config
}

fn dock_state_with(config: &Config) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(config));
    state.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    state.set_pane_surface(surface());
    state.browser.info = Some(companion("running"));
    state.browser_dock.extent_for_test = Some((COLS, ROWS, 1600, 900));
    present(&mut state);
    state
}

/// Compose, and the frame reaches the terminal (a placement shows only
/// over a presented frame that makes room for it).
fn recompose(state: &mut ClientShellState) -> Option<FrameData> {
    if state.pane_surface.is_none() {
        state.set_pane_surface(surface());
    }
    let composed = state.compose(COLS, ROWS)?;
    state.note_presented(composed.drawn);
    Some(composed.frame)
}

fn toggle_dock(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.toggle_browser_dock_for_focused_tab(&mut outcome);
    recompose(state);
    outcome
}

fn toggle_view(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.toggle_browser_view_for_focused_tab(&mut outcome);
    recompose(state);
    outcome
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })
}

fn click(column: u16, row: u16) -> Vec<RawInputEvent> {
    vec![
        mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        mouse(MouseEventKind::Up(MouseButton::Left), column, row),
    ]
}

/// The placement message this client would send now, decoded.
fn sent_placement(
    state: &mut ClientShellState,
    at: std::time::Instant,
) -> Option<Option<crate::api::schema::BrowserPlacementSpec>> {
    let message = state.take_browser_placement_request(at)?;
    let crate::protocol::ClientMessage::ClientShellEndpointRequest { boot_id, request } = message
    else {
        panic!("a placement is an endpoint request");
    };
    assert_eq!(boot_id, "boot-1");
    let request: Request = serde_json::from_str(&request).expect("request json");
    assert!(request
        .id
        .starts_with(super::super::browser_dock::PLACEMENT_REQUEST_PREFIX));
    match request.method {
        Method::BrowserPlacementSet(params) => Some(params.placement),
        other => panic!("{other:?}"),
    }
}

fn row_text(frame: &FrameData, y: u16) -> String {
    (0..frame.width)
        .map(|x| frame.cells[(y * frame.width + x) as usize].symbol.as_str())
        .collect()
}

#[test]
fn fork_smoke_the_dock_takes_45_percent_on_the_right_and_reports_the_narrower_surface() {
    let mut state = dock_state();
    let before = state.surface_size(COLS, ROWS);
    let outcome = toggle_dock(&mut state);
    assert!(outcome.resize, "the surface narrows");
    assert!(outcome.repaint);
    let layout = state.layout(COLS, ROWS);
    let main_width = COLS - layout.sidebar.width;
    let band = layout.browser_band;
    assert_eq!(band.right(), COLS, "the band meets the window's right edge");
    assert_eq!(band.width, main_width * 45 / 100);
    assert_eq!(layout.pane_surface.right(), band.x);
    let after = state.surface_size(COLS, ROWS);
    assert_eq!(after.cols, before.cols - band.width);
    // the tabs layout's header carries the view controls: no strip row
    assert_eq!(layout.session_header.height, 1);
    assert!(layout.view_strip.is_empty());
    assert_eq!(after.rows, before.rows);
    // the divider is the band's first column
    assert_eq!(state.hits.browser_dock.divider.x, band.x);
    // the other tab is untouched: switching to it reflows that tab alone
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    recompose(&mut state);
    assert!(state.layout(COLS, ROWS).browser_band.is_empty());
    assert_eq!(state.surface_size(COLS, ROWS), before);
}

#[test]
fn the_divider_drag_is_remembered_and_a_double_click_resets_it() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let divider = state.hits.browser_dock.divider;
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Down(MouseButton::Left),
        divider.x,
        10,
    )]);
    assert!(state.browser_dock.dragging);
    assert!(
        matches!(state.chrome_drag, Some(ClientChromeDrag::BrowserDockWidth)),
        "the divider press starts a drag"
    );
    recompose(&mut state);
    // the dragged divider is drawn gold
    let frame = recompose(&mut state).expect("frame");
    assert_eq!(
        row_text(&frame, 10).chars().nth(usize::from(divider.x)),
        Some('┃')
    );
    let outcome = state.handle_raw_events(vec![mouse(
        MouseEventKind::Drag(MouseButton::Left),
        divider.x - 20,
        10,
    )]);
    assert!(outcome.resize);
    let layout = state.layout(COLS, ROWS);
    let main_width = COLS - layout.sidebar.width;
    assert!(state.browser_dock.percent > 45);
    assert!(state.browser_dock.percent_manual);
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        divider.x - 20,
        10,
    )]);
    assert!(!state.browser_dock.dragging);
    recompose(&mut state);
    let band = state.layout(COLS, ROWS).browser_band;
    assert_eq!(
        band.width,
        main_width * state.browser_dock.percent / 100,
        "the remembered percent sizes the band"
    );
    // a double-click resets to the config default
    let divider = state.hits.browser_dock.divider;
    state.handle_raw_events(click(divider.x, 10));
    state.handle_raw_events(click(divider.x, 10));
    assert_eq!(state.browser_dock.percent, 45);
    assert!(!state.browser_dock.percent_manual);
}

#[test]
fn the_dock_and_the_info_pane_share_the_right_edge_and_the_terminal_keeps_30_columns() {
    // Fork (session header): the tabs layout has one dock slot, the band holds it.
    let tabs = ClientShellConfig::from_config(&Config::default());
    let one = tabs.layout_with_browser(COLS, ROWS, false, 1, 26, Some(44), Some(45), false);
    assert_eq!(one.browser_band.right(), COLS);
    assert!(one.info_dock.is_empty(), "{one:?}");
    assert_eq!(one.pane_surface.right(), one.browser_band.x);
    // the spaces layout keeps both
    let config = ClientShellConfig::from_config(&spaces_config());
    // both docks: the band outermost, the info dock left of it
    let both = config.layout_with_browser(COLS, ROWS, false, 1, 26, Some(44), Some(45), true);
    assert_eq!(both.browser_band.right(), COLS);
    assert_eq!(both.info_dock.right(), both.browser_band.x);
    assert_eq!(both.pane_surface.right(), both.info_dock.x);
    assert!(both.pane_surface.width >= super::super::browser_dock::TERM_MIN);
    // too little room: the info dock drops first, the band shrinks to its minimum
    let narrow = config.layout_with_browser(110, ROWS, false, 1, 26, Some(44), Some(80), false);
    assert!(narrow.info_dock.is_empty(), "{narrow:?}");
    assert!(narrow.browser_band.width >= super::super::browser_dock::BAND_MIN);
    assert!(narrow.pane_surface.width >= super::super::browser_dock::TERM_MIN);
    // no room for the band and a 30-column terminal: no band
    let tiny = config.layout_with_browser(90, ROWS, false, 1, 26, None, Some(45), false);
    assert!(tiny.browser_band.is_empty());
}

#[test]
fn a_modal_over_the_band_unshows_the_placement() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let now = std::time::Instant::now();
    let spec = sent_placement(&mut state, now)
        .expect("a placement is sent")
        .expect("with a spec");
    assert!(spec.shown);
    assert_eq!(spec.kind, BrowserPlacementKind::Side);
    assert_eq!(spec.tab_id, "tab_1");
    let band = state.layout(COLS, ROWS).browser_band;
    // the area is the band without its divider
    assert_eq!(spec.area.x, band.x + 1);
    assert_eq!(spec.area.width, band.width - 1);
    assert_eq!(spec.grid.cols, COLS);
    assert_eq!(spec.extent_px.width, 1600);
    assert_eq!(spec.cell_px.width, 10);
    assert_eq!(spec.cell_px.height, 20);
    assert!(!spec.pids.is_empty());
    assert_eq!(spec.pids[0], std::process::id());
    // unchanged: nothing more is sent
    assert!(sent_placement(&mut state, now + std::time::Duration::from_secs(1)).is_none());
    // the Browser overlay (a modal) is over the band: reserved, not covered
    state.open_browser_overlay(&mut ClientShellInput::default());
    let later = now + std::time::Duration::from_secs(2);
    let spec = sent_placement(&mut state, later)
        .expect("sent again")
        .expect("spec");
    assert!(!spec.shown);
    // within 33 ms of the last one, a change waits for the gap
    state.overlay = None;
    assert!(sent_placement(&mut state, later + std::time::Duration::from_millis(10)).is_none());
    assert!(state.next_browser_dock_deadline(later).is_some());
    let spec = sent_placement(&mut state, later + std::time::Duration::from_millis(40))
        .expect("sent after the gap")
        .expect("spec");
    assert!(spec.shown);
    // undocking clears it, once
    toggle_dock(&mut state);
    let cleared = sent_placement(&mut state, later + std::time::Duration::from_secs(1));
    assert_eq!(cleared, Some(None));
    assert!(sent_placement(&mut state, later + std::time::Duration::from_secs(2)).is_none());
}

/// A placement that waits for its gap wakes the tick once per gap, even
/// when the tick cannot send it (the runtime takes it only while the
/// endpoint is online and no activation is pending), and needs no frame.
/// A snapshot that moves the focused tab changes the placement without any
/// input (the runtime sends it with the snapshot).
#[test]
fn a_waiting_placement_wakes_the_tick_once_per_gap_and_follows_a_tab_switch() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let t0 = std::time::Instant::now();
    let first = sent_placement(&mut state, t0).expect("sent").expect("spec");
    assert!(first.shown);
    // a change inside the gap waits for it
    state.browser_dock.nudge += 1;
    assert!(sent_placement(&mut state, t0 + std::time::Duration::from_millis(5)).is_none());
    let gap = std::time::Duration::from_millis(33);
    assert!(state.next_browser_dock_deadline(t0).expect("waiting") >= t0 + gap);
    // a tick that could not send it: the next wake is a gap later, no frame
    let t1 = t0 + std::time::Duration::from_millis(40);
    let mut outcome = ClientShellInput::default();
    state.tick_browser_dock(t1, &mut outcome);
    assert!(!outcome.repaint, "a waiting placement needs no frame");
    assert!(
        state.next_browser_dock_deadline(t1).expect("still waiting") >= t1 + gap,
        "never a busy loop"
    );
    // once the runtime can send it, it goes, and nothing waits any more
    let nudged = sent_placement(&mut state, t1).expect("sent").expect("spec");
    assert_eq!(nudged.nudge, first.nudge + 1);
    assert!(state.next_browser_dock_deadline(t1).is_none());
    // the server shows the other tab (no input): the placement says so
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    recompose(&mut state);
    let switched = sent_placement(&mut state, t1 + std::time::Duration::from_secs(1))
        .expect("sent with the snapshot")
        .expect("spec");
    assert_eq!(switched.tab_id, "tab_2");
    assert!(
        !switched.shown,
        "tab_2 has no dock: reserved elsewhere, not covered"
    );
    assert_eq!(switched.kind, BrowserPlacementKind::Side);
}

#[test]
fn fork_smoke_browser_view_covers_the_pane_area_without_resizing_the_panes() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let docked = state.surface_size(COLS, ROWS);
    let outcome = toggle_view(&mut state);
    assert!(!outcome.resize, "toggling never resizes the panes");
    assert_eq!(state.surface_size(COLS, ROWS), docked);
    let layout = state.layout(COLS, ROWS);
    let spec = sent_placement(&mut state, std::time::Instant::now())
        .expect("sent")
        .expect("spec");
    assert_eq!(spec.kind, BrowserPlacementKind::Full);
    // from the pane surface's left edge to the window's right edge, below the strip
    assert_eq!(spec.area.x, layout.pane_surface.x);
    assert_eq!(spec.area.x + spec.area.width, COLS);
    assert_eq!(spec.area.y, layout.pane_surface.y);
    assert_eq!(spec.area.height, layout.pane_surface.height);
    // the panes are not drawn and take no clicks; the strip says Browser
    let frame = recompose(&mut state).expect("frame");
    assert!(state.hits.panes.is_empty());
    assert!(frame.cursor.is_none());
    let strip = row_text(&frame, layout.view_strip.y);
    assert!(
        strip.contains("Terminal") && strip.contains("Browser"),
        "{strip}"
    );
    assert!(!row_text(&frame, layout.pane_surface.y).contains("LIVE"));
    // back to the terminal: the panes are there again
    toggle_view(&mut state);
    let frame = recompose(&mut state).expect("frame");
    assert!(!state.hits.panes.is_empty());
    assert!(frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>()
        .contains("LIVE"));
}

#[test]
fn keys_typed_in_browser_view_never_reach_the_hidden_panes() {
    let mut state = dock_state();
    toggle_view(&mut state);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('x'),
        KeyModifiers::empty(),
    ))]);
    assert!(
        !outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })),
        "no pane input: {:?}",
        outcome.requests
    );
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("the first swallowed key says how to type");
    assert!(notice.body.contains("Click the page"), "{}", notice.body);
    // the terminal again: keys reach the pane
    toggle_view(&mut state);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('x'),
        KeyModifiers::empty(),
    ))]);
    assert!(outcome
        .requests
        .iter()
        .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. })));
}

/// Fork (companion launcher, audit 3): nor does a paste (a shell would run
/// its line), an IME or compose commit, or an image paste; the first says
/// how to type into the page, like a key.
#[test]
fn fork_smoke_pastes_and_text_commits_in_browser_view_never_reach_the_hidden_panes() {
    let pane_input = |outcome: &ClientShellInput| {
        outcome
            .requests
            .iter()
            .any(|request| matches!(request, ClientMessage::ClientShellPaneInput { .. }))
    };
    let mut state = dock_state();
    assert!(state.clipboard_image_target().is_some());
    toggle_view(&mut state);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Paste("rm -rf build\n".into())]);
    assert!(!pane_input(&outcome), "{:?}", outcome.requests);
    let notice = state
        .visible_endpoint_notice
        .as_ref()
        .expect("the paste says how to type");
    assert!(notice.body.contains("Click the page"), "{}", notice.body);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("şğü"),
    )]);
    assert!(!pane_input(&outcome), "{:?}", outcome.requests);
    assert_eq!(
        state.clipboard_image_target(),
        None,
        "no image paste either"
    );
    // the terminal again: pastes and commits reach the pane
    toggle_view(&mut state);
    let outcome = state.handle_raw_events(vec![RawInputEvent::Paste("echo hi".into())]);
    assert!(pane_input(&outcome));
    let outcome = state.handle_raw_events(vec![RawInputEvent::Text(
        crate::input::TextCommit::new("ş"),
    )]);
    assert!(pane_input(&outcome));
    assert!(state.clipboard_image_target().is_some());
}

#[test]
fn the_view_strip_shows_only_for_tabs_with_pages_or_a_browser_view() {
    let mut state = dock_state_with(&spaces_config());
    assert!(!state.browser_view_strip_for_focused_tab());
    assert!(state.layout(COLS, ROWS).view_strip.is_empty());
    // an agent opened the tab's first page: the strip appears (one row)
    let mut info = companion("running");
    info.sessions = vec![BrowserSessionInfo {
        tab_id: "tab_1".into(),
        pages: 3,
        ..Default::default()
    }];
    state.browser.info = Some(info);
    let frame = recompose(&mut state).expect("frame");
    let strip = state.layout(COLS, ROWS).view_strip;
    assert_eq!(strip.height, 1);
    let text = row_text(&frame, strip.y);
    assert!(text.contains("Terminal"), "{text}");
    assert!(text.contains("Browser · 3"), "{text}");
    assert!(text.contains("◨"), "{text}");
    // the other tab has none
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    recompose(&mut state);
    assert!(state.layout(COLS, ROWS).view_strip.is_empty());
    // `ui.browser_view_strip = "off"`: never drawn
    let mut config = spaces_config();
    config.ui.browser_view_strip = "off".into();
    let mut off = ClientShellState::new(ClientShellConfig::from_config(&config));
    off.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    off.set_pane_surface(surface());
    let mut info = companion("running");
    info.sessions = vec![BrowserSessionInfo {
        tab_id: "tab_1".into(),
        pages: 3,
        ..Default::default()
    }];
    off.browser.info = Some(info);
    off.compose(COLS, ROWS);
    assert!(off.layout(COLS, ROWS).view_strip.is_empty());
    // a click on the strip's Browser segment opens Browser view
    let mut state = dock_state_with(&spaces_config());
    toggle_dock(&mut state);
    let (rect, _) = state
        .hits
        .browser_dock
        .targets
        .iter()
        .find(|(_, target)| *target == super::super::browser_dock::BrowserDockTarget::ViewBrowser)
        .copied()
        .expect("the Browser segment");
    state.handle_raw_events(click(rect.x + 1, rect.y));
    assert!(state.browser_view_open_for("tab_1"));
}

#[test]
fn the_placeholder_names_why_the_companion_is_not_covering() {
    let text = |state: &mut ClientShellState| {
        let frame = recompose(state).expect("frame");
        let band = state.layout(COLS, ROWS).browser_band;
        (band.y..band.bottom())
            .map(|y| {
                (band.x..band.right())
                    .map(|x| frame.cells[(y * frame.width + x) as usize].symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut state = dock_state();
    toggle_dock(&mut state);
    // running, nothing reported yet: calm (only the mark)
    let calm = text(&mut state);
    assert!(calm.contains('◎') && !calm.contains("companion"), "{calm}");
    // stopped: a Start button that starts it
    state.browser.info = Some(companion("stopped"));
    let stopped = text(&mut state);
    assert!(
        stopped.contains("The companion is not running"),
        "{stopped}"
    );
    assert!(stopped.contains("[ Start companion ]"), "{stopped}");
    let (rect, _) = *state
        .hits
        .browser_dock
        .targets
        .iter()
        .find(|(_, target)| *target == super::super::browser_dock::BrowserDockTarget::Start)
        .expect("the Start button");
    let outcome = state.handle_raw_events(click(rect.x + 2, rect.y));
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, Method::BrowserCompanion(ref params)
                if params.action == crate::api::schema::BrowserCompanionAction::Start)
    )));
    // not installed: the setup command
    state.browser.info = Some(companion("not_installed"));
    let missing = text(&mut state);
    assert!(
        missing.contains("herdr browser setup --companion"),
        "{missing}"
    );
    assert!(
        missing.lines().all(|line| line.trim() != "terminal"),
        "no word stranded on its own line: {missing}"
    );
    // failed: the launcher's give-up rule, in its own words
    state.browser.info = Some(companion("failed"));
    let failed = text(&mut state);
    assert!(
        failed.contains(&format!(
            "The companion stopped after {}",
            crate::browser::companion_app::GIVE_UP_TEXT
        )),
        "{failed}"
    );
    assert!(failed.contains("[ Restart ]"), "{failed}");
    // starting: the tab's label and the spinner
    state.browser.info = Some(companion("starting"));
    let starting = text(&mut state);
    assert!(starting.contains("Browser · planner"), "{starting}");
    assert!(starting.contains("Starting the companion"), "{starting}");
    // running but the companion cannot dock here: its reason
    let mut info = companion("running");
    info.placements = vec![BrowserPlacementStatus {
        id: state.browser_dock.placement_id.clone(),
        state: "unsupported".into(),
        reason: Some("Let herdr fill its Ghostty window (no Ghostty splits)".into()),
    }];
    state.browser.info = Some(info);
    let unsupported = text(&mut state);
    assert!(unsupported.contains("no Ghostty splits"), "{unsupported}");
    // unbound: a click anywhere on it binds (nudge)
    let mut info = companion("running");
    info.placements = vec![BrowserPlacementStatus {
        id: state.browser_dock.placement_id.clone(),
        state: "unbound".into(),
        reason: None,
    }];
    state.browser.info = Some(info);
    assert!(text(&mut state).contains("Click here once"));
    let band = state.layout(COLS, ROWS).browser_band;
    state.handle_raw_events(click(band.x + 5, band.y + 3));
    assert_eq!(state.browser_dock.nudge, 1);
}

#[test]
fn docking_a_remote_endpoint_or_on_an_old_server_says_why() {
    let mut state = dock_state();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == state.active_endpoint_id)
        .expect("the local endpoint")
        .methods = Some(vec!["browser.get".to_owned()].into_iter().collect());
    let outcome = toggle_dock(&mut state);
    assert!(!outcome.resize);
    assert!(state.layout(COLS, ROWS).browser_band.is_empty());
    let notice = state.visible_endpoint_notice.as_ref().expect("a notice");
    assert!(
        notice.body.contains("update and restart"),
        "{}",
        notice.body
    );
}

#[test]
fn a_companion_that_never_reports_on_the_dock_is_offered_a_restart() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let now = std::time::Instant::now();
    sent_placement(&mut state, now).expect("sent");
    // a moment later: still calm (the companion takes its time to bind)
    assert!(state.browser_placeholder(now).calm);
    // seconds later without a word from it: an old companion
    let later = now + std::time::Duration::from_secs(5);
    let placeholder = state.browser_placeholder(later);
    assert!(!placeholder.calm);
    assert_eq!(
        placeholder.button,
        Some((
            "Restart",
            super::super::browser_dock::BrowserDockTarget::Restart
        ))
    );
    assert!(placeholder.lines[0].0.contains("has not docked"));
    // its report arrives: calm again
    let mut info = companion("running");
    info.placements = vec![BrowserPlacementStatus {
        id: state.browser_dock.placement_id.clone(),
        state: "covering".into(),
        reason: None,
    }];
    state.browser.info = Some(info);
    assert!(state.browser_placeholder(later).calm);
}

/// Fork (companion launcher, audit 2 A8): the placement goes out after every
/// input event while a dock is in use, but the terminal is asked for its
/// pixel extent once per resize (one `/dev/tty` open), not per event; the
/// extent beyond whole cells comes from the terminal, never from the grid.
#[test]
fn fork_smoke_the_terminal_extent_is_read_once_per_resize_not_per_event() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let start = std::time::Instant::now();
    let first = sent_placement(&mut state, start)
        .expect("sent")
        .expect("a placement");
    assert_eq!(first.extent_px.width, 1600);
    let reads = state.browser_dock.extent_reads;
    assert_eq!(reads, 1);
    for step in 1..=20_u64 {
        state.handle_raw_events(vec![mouse(MouseEventKind::Moved, 10, 10)]);
        let _ = state
            .take_browser_placement_request(start + std::time::Duration::from_millis(50 * step));
    }
    assert_eq!(state.browser_dock.extent_reads, reads, "no read per event");
    // the window grows by less than a cell: the same grid, a wider extent
    state.browser_dock.extent_for_test = Some((COLS, ROWS, 1610, 900));
    state.note_terminal_resized();
    let later = start + std::time::Duration::from_secs(5);
    let grown = sent_placement(&mut state, later)
        .expect("sent again")
        .expect("a placement");
    assert_eq!(grown.extent_px.width, 1610);
    assert_eq!(grown.grid.cols, COLS);
    assert_eq!(state.browser_dock.extent_reads, reads + 1);
    // with no dock anywhere nothing is read or sent after the clear
    toggle_dock(&mut state);
    assert_eq!(
        sent_placement(&mut state, later + std::time::Duration::from_secs(1)),
        Some(None),
        "the clear"
    );
    state.note_terminal_resized();
    assert!(state
        .take_browser_placement_request(later + std::time::Duration::from_secs(2))
        .is_none());
    assert_eq!(state.browser_dock.extent_reads, reads + 1);
}

/// Fork (companion launcher, audit 2 A14): the layout's fast path (no dock,
/// no Browser view: no lookups) holds after the last dock or view closes or
/// its tab goes.
#[test]
fn closing_the_last_dock_or_view_leaves_the_dock_state_empty() {
    let mut state = dock_state();
    assert!(state.browser_dock.docked.is_empty() && state.browser_dock.viewing.is_empty());
    assert_eq!(state.browser_band_for_focused_tab(), None);
    toggle_dock(&mut state);
    toggle_view(&mut state);
    assert_eq!(state.browser_dock.docked.len(), 1);
    assert_eq!(state.browser_dock.viewing.len(), 1);
    assert!(state.browser_band_for_focused_tab().is_some());
    assert!(state.browser_view_strip_for_focused_tab());
    toggle_view(&mut state);
    toggle_dock(&mut state);
    assert!(
        state.browser_dock.docked.is_empty(),
        "no endpoint left listed"
    );
    assert!(state.browser_dock.viewing.is_empty());
    assert!(!state.browser_view_strip_for_focused_tab());
    // a dock on a tab that closes is pruned the same way
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    toggle_dock(&mut state);
    assert!(state.browser_dock_open_for("tab_2"));
    let mut snapshot = two_tab_snapshot("tab_1");
    snapshot.tabs.retain(|tab| tab.tab_id == "tab_1");
    state.set_snapshot(Box::new(snapshot));
    assert!(!state.browser_dock_open_for("tab_2"));
    assert!(state.browser_dock.docked.is_empty());
}

/// Fork (companion launcher, audit 2 A3): a key typed in Browser view while
/// no page covers the tab says what the area says to do, not "click the page".
#[test]
fn the_swallowed_key_toast_follows_what_covers_the_tab() {
    let body = |info: BrowserGetInfo, report: Option<&str>| {
        let mut state = dock_state();
        toggle_view(&mut state);
        let mut info = info;
        if let Some(report) = report {
            info.placements = vec![BrowserPlacementStatus {
                id: state.browser_dock.placement_id.clone(),
                state: report.into(),
                reason: Some("Let herdr fill its Ghostty window (no Ghostty splits)".into()),
            }];
        }
        state.browser.info = Some(info);
        state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        ))]);
        state
            .visible_endpoint_notice
            .as_ref()
            .expect("the toast")
            .body
            .clone()
    };
    let stopped = body(companion("stopped"), None);
    assert!(
        stopped.starts_with("The companion is not running: click Start companion;"),
        "{stopped}"
    );
    assert!(stopped.ends_with("shows the terminal again."), "{stopped}");
    let missing = body(companion("not_installed"), None);
    assert!(
        missing.contains("run herdr browser setup --companion"),
        "{missing}"
    );
    let failed = body(companion("failed"), None);
    assert!(
        failed.contains(crate::browser::companion_app::GIVE_UP_TEXT)
            && failed.contains("click Restart"),
        "{failed}"
    );
    assert!(body(companion("starting"), None).starts_with("The companion is starting;"));
    let unbound = body(companion("running"), Some("unbound"));
    assert!(
        unbound.starts_with("Click the browser's area once"),
        "{unbound}"
    );
    let split = body(companion("running"), Some("unsupported"));
    assert!(
        split.starts_with("Let herdr fill its Ghostty window"),
        "{split}"
    );
    let covering = body(companion("running"), Some("covering"));
    assert!(
        covering.starts_with("Click the page to type there;"),
        "{covering}"
    );
}

/// Fork (companion launcher, audit 2): a dock opened and closed again
/// inside the send gap after a clear leaves nothing waiting (no tick every
/// 33 ms until the next dock).
#[test]
fn a_dock_closed_inside_the_send_gap_leaves_nothing_pending() {
    let mut state = dock_state();
    let start = std::time::Instant::now();
    let at = |ms: u64| start + std::time::Duration::from_millis(ms);
    toggle_dock(&mut state);
    sent_placement(&mut state, at(0))
        .expect("sent")
        .expect("a placement");
    toggle_dock(&mut state);
    assert_eq!(sent_placement(&mut state, at(40)), Some(None), "the clear");
    // docked again inside the gap: it waits
    toggle_dock(&mut state);
    assert!(state.take_browser_placement_request(at(50)).is_none());
    assert!(state.next_browser_dock_deadline(at(50)).is_some());
    // and closed before the gap is over: nothing to send, nothing waiting
    toggle_dock(&mut state);
    assert!(state.take_browser_placement_request(at(60)).is_none());
    assert_eq!(state.next_browser_dock_deadline(at(60)), None);
}

/// Fork (companion launcher, audit 3): a server whose config keeps the
/// companion off (the browser disabled, `host = "window"` with the window
/// on) answers `browser.get` without it: no dock or strip is offered, and
/// opening one says why instead of carving an empty band; one already open
/// still closes.
#[test]
fn fork_smoke_without_the_companion_nothing_docks_and_the_notice_says_why() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    assert!(state.browser_dock_open_for("tab_1"));
    // host = "window": browser.get has no companion_app; the tab has pages
    let window_only = BrowserGetInfo {
        seq: 2,
        enabled: true,
        window_off: false,
        sessions: vec![BrowserSessionInfo {
            tab_id: "tab_2".into(),
            pages: 2,
            ..Default::default()
        }],
        ..Default::default()
    };
    state.browser.info = Some(window_only.clone());
    recompose(&mut state);
    assert!(!state.browser_dock_offered());
    // the open dock still closes
    toggle_dock(&mut state);
    assert!(!state.browser_dock_open_for("tab_1"));
    assert!(state.visible_endpoint_notice.is_none());
    // and does not open again
    let outcome = toggle_dock(&mut state);
    assert!(!state.browser_dock_open_for("tab_1"));
    assert!(!outcome.resize);
    let notice = state.visible_endpoint_notice.take().expect("says why");
    assert_eq!(notice.title, "Browser dock");
    assert!(notice.body.contains("host = \"window\""), "{}", notice.body);
    toggle_view(&mut state);
    assert!(!state.browser_view_open_for("tab_1"));
    let notice = state.visible_endpoint_notice.take().expect("says why");
    assert_eq!(notice.title, "Browser view");
    // pages alone show no view strip
    state.set_snapshot(Box::new(two_tab_snapshot("tab_2")));
    recompose(&mut state);
    assert!(!state.browser_view_strip_for_focused_tab());
    assert!(state.layout(COLS, ROWS).view_strip.is_empty());
    // the browser disabled
    state.browser.info = Some(BrowserGetInfo {
        enabled: false,
        ..window_only
    });
    toggle_dock(&mut state);
    assert!(!state.browser_dock_open_for("tab_2"));
    let notice = state.visible_endpoint_notice.take().expect("says why");
    assert!(notice.body.contains("enabled = false"), "{}", notice.body);
    // a `disabled` reply (`browser.companion status`) reads as no companion
    state.browser.info = Some(companion("stopped"));
    assert!(state.browser_dock_offered());
    state.handle_browser_companion_result(Ok(
        crate::api::schema::ResponseResult::BrowserCompanion {
            companion: BrowserCompanionAppInfo {
                state: crate::browser::companion_app::DISABLED_STATE.into(),
                ..Default::default()
            },
            note: Some("the companion browser is off".into()),
        },
    ));
    assert!(state.browser_companion().is_none());
    assert!(!state.browser_dock_offered());
    // before the first browser.get nothing is refused (the answer follows)
    state.browser.info = None;
    toggle_dock(&mut state);
    assert!(state.browser_dock_open_for("tab_2"));
}

/// Fork (companion launcher, audit 3): in Browser view the companion covers
/// the whole tab, so herdr's notice (the guard's "click the page"), a toast
/// (an agent finished) and the agent cards show as one line on the view
/// strip, between its switch and its `◨`: never inside the area the
/// placement publishes. With the strip off, the tab bar's right half (with
/// neither row they stay where they are).
#[test]
fn fork_smoke_browser_view_shows_notices_and_toasts_on_its_strip_not_under_the_page() {
    let full_area =
        |state: &ClientShellState| super::super::browser_dock::full_area(&state.layout(COLS, ROWS));
    let mut state = dock_state();
    toggle_view(&mut state);
    let now = std::time::Instant::now();
    let spec = sent_placement(&mut state, now)
        .expect("sent")
        .expect("a placement");
    let published = Rect::new(spec.area.x, spec.area.y, spec.area.width, spec.area.height);
    assert_eq!(published, full_area(&state));
    // the guard's notice
    state.handle_raw_events(vec![RawInputEvent::Key(TerminalKey::new(
        KeyCode::Char('x'),
        KeyModifiers::empty(),
    ))]);
    let frame = recompose(&mut state).expect("frame");
    // Fork (session header): the tabs layout's header carries the line.
    let strip = state.layout(COLS, ROWS).session_header;
    assert!(state.layout(COLS, ROWS).view_strip.is_empty());
    let toast = state.hits.notification_toast;
    assert!(!toast.is_empty(), "drawn");
    assert_eq!((toast.y, toast.height), (strip.y, 1), "on the strip");
    assert!(
        !toast.intersects(published),
        "{toast:?} under {published:?}"
    );
    let line = row_text(&frame, strip.y);
    assert!(line.contains("Click the page to type there"), "{line}");
    assert!(
        line.contains("Terminal") && line.contains("Browser") && line.contains("◨"),
        "the switch and the dock toggle stay: {line}"
    );
    // a toast (an agent finished): the strip's line too, clickable
    state.visible_endpoint_notice = None;
    state
        .visible_notifications
        .push_back(ClientVisibleNotification {
            endpoint_id: ClientEndpointId::Local,
            event: SemanticNotification {
                kind: SemanticNotificationKind::Finished,
                title: "claude finished".into(),
                body: Some("planner".into()),
                sound: None,
                agent: None,
                workspace_id: None,
                tab_id: None,
                pane_id: None,
                position: None,
            },
            deadline: now + std::time::Duration::from_secs(60),
            reminder: None,
        });
    let frame = recompose(&mut state).expect("frame");
    let toast = state.hits.notification_toast;
    assert_eq!(toast.y, strip.y);
    assert!(!toast.intersects(published));
    assert_eq!(state.hits.notification_toasts, vec![(toast, Some(0))]);
    assert!(row_text(&frame, strip.y).contains("claude done"));
    // the agent cards once the line is free
    state.visible_notifications.clear();
    let card = crate::api::schema::AgentNoticeInfo {
        id: "n1".into(),
        kind: crate::api::schema::AgentNoticeKind::Question,
        title: "which port?".into(),
        body: None,
        agent: Some("claude".into()),
        name: "builder".into(),
        pane_id: "pane_9".into(),
        tab_id: Some("tab_2".into()),
        tab_label: Some("scratch".into()),
        workspace_id: Some("ws_1".into()),
        workspace_label: None,
        unix: 1,
        team: None,
        role: None,
    };
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        crate::server::headless::agent_notices::AgentNoticesPayload {
            boot_id: "boot-1".into(),
            revision: 1,
            notices: vec![card],
            initial: false,
        },
    );
    let frame = recompose(&mut state).expect("frame");
    let (rect, _) = state.hits.agent_cards.first().expect("a card hit");
    assert_eq!(rect.y, strip.y);
    assert!(!rect.intersects(published));
    assert!(row_text(&frame, strip.y).contains("which port?"));
    // the terminal again: a notice is a card at the top right again
    toggle_view(&mut state);
    state.receive_agent_notices(
        &ClientEndpointId::Local,
        crate::server::headless::agent_notices::AgentNoticesPayload {
            boot_id: "boot-1".into(),
            revision: 2,
            notices: Vec::new(),
            initial: false,
        },
    );
    state.push_endpoint_notice(
        ClientEndpointNoticeKind::Rejected,
        "test",
        "Notice",
        "a body",
    );
    recompose(&mut state);
    let toast = state.hits.notification_toast;
    assert!(toast.height > 1, "a card: {toast:?}");
    assert!(toast.intersects(state.layout(COLS, ROWS).pane_surface));
    // the strip off (and a tab bar: the spaces layout): the tab bar's right
    // half, still outside the page
    let mut config = Config::default();
    config.ui.browser_view_strip = "off".into();
    config.ui.sidebar_layout = crate::config::SidebarLayoutConfig::Spaces;
    let mut off = ClientShellState::new(ClientShellConfig::from_config(&config));
    off.set_snapshot(Box::new(two_tab_snapshot("tab_1")));
    off.set_pane_surface(surface());
    off.browser.info = Some(companion("running"));
    off.browser_dock.extent_for_test = Some((COLS, ROWS, 1600, 900));
    off.compose(COLS, ROWS);
    toggle_view(&mut off);
    off.push_endpoint_notice(
        ClientEndpointNoticeKind::Rejected,
        "test",
        "Notice",
        "a body",
    );
    recompose(&mut off);
    let layout = off.layout(COLS, ROWS);
    assert!(layout.view_strip.is_empty());
    let toast = off.hits.notification_toast;
    assert_eq!(toast.y, layout.tab_bar.y, "{toast:?}");
    assert!(!toast.intersects(full_area(&off)));
}

/// Where `needle` is drawn on `frame` (one row, one cell per char).
fn find_text(frame: &FrameData, needle: &str) -> Option<Rect> {
    let chars: Vec<String> = needle.chars().map(String::from).collect();
    let width = u16::try_from(chars.len()).ok()?;
    (0..frame.height).find_map(|y| {
        (0..frame.width.saturating_sub(width)).find_map(|x| {
            chars
                .iter()
                .enumerate()
                .all(|(i, ch)| frame.cells[(y * frame.width + x + i as u16) as usize].symbol == *ch)
                .then(|| Rect::new(x, y, width, 1))
        })
    })
}

// Fork (toast below header): the config diagnostic and the endpoint status
// banner start below the header, so with the browser docked they keep left
// of its band (the companion's window covers it) and in Browser view they
// take the header's banner slot, beside the view tabs.
#[test]
fn fork_smoke_status_banners_keep_clear_of_the_docked_band_and_the_browser_view() {
    let mut state = dock_state();
    state.config_diagnostic = Some("bad config key".into());
    let active = state.active_endpoint_id.clone();
    let endpoint = state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == active)
        .expect("the active endpoint");
    endpoint.status = ClientEndpointStatus::Connecting;
    let lifecycle = format!("{} · connecting", endpoint.label);
    let needles = ["bad config key", lifecycle.as_str()];

    toggle_dock(&mut state);
    let frame = recompose(&mut state).expect("frame");
    let layout = state.layout(COLS, ROWS);
    let band = layout.browser_band;
    assert!(!band.is_empty(), "docked");
    for needle in needles {
        let rect = find_text(&frame, needle).unwrap_or_else(|| panic!("{needle:?} drawn"));
        assert!(!rect.intersects(band), "{needle:?} {rect:?} under {band:?}");
        assert!(
            !rect.intersects(layout.session_header),
            "{needle:?} {rect:?} on the header"
        );
    }

    toggle_view(&mut state);
    let frame = recompose(&mut state).expect("frame");
    let layout = state.layout(COLS, ROWS);
    let full = super::super::browser_dock::full_area(&layout);
    let header = layout.session_header;
    for needle in needles {
        let rect = find_text(&frame, needle).unwrap_or_else(|| panic!("{needle:?} drawn"));
        assert!(!rect.intersects(full), "{needle:?} {rect:?} under {full:?}");
        assert_eq!(rect.y, header.y, "{needle:?} on the header's slot");
    }
    let line = row_text(&frame, header.y);
    assert!(
        line.contains("Terminal") && line.contains("Browser") && line.contains("◨"),
        "the view tabs and the dock toggle stay: {line}"
    );
}

fn cycle(state: &mut ClientShellState) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.cycle_browser_for_focused_tab(&mut outcome);
    recompose(state);
    outcome
}

/// (docked, Browser view) of the focused tab.
fn browser_state(state: &ClientShellState) -> (bool, bool) {
    (
        state.browser_dock_open_for("tab_1"),
        state.browser_view_open_for("tab_1"),
    )
}

/// Fork (cycle_browser): none → docked → full → none, from each state.
#[test]
fn cycle_browser_goes_none_docked_full_none() {
    let mut state = dock_state();
    assert_eq!(browser_state(&state), (false, false));
    let outcome = cycle(&mut state);
    assert_eq!(browser_state(&state), (true, false), "none → docked");
    assert!(outcome.resize, "the band narrows the panes");
    cycle(&mut state);
    assert_eq!(browser_state(&state), (true, true), "docked → full");
    let outcome = cycle(&mut state);
    assert_eq!(
        browser_state(&state),
        (false, false),
        "full → none, no dock left"
    );
    assert!(outcome.resize, "the panes get the band's columns back");
    cycle(&mut state);
    assert_eq!(browser_state(&state), (true, false), "and round again");
}

/// Fork (cycle_browser): a Browser view opened without the dock (the
/// `toggle_browser_view` key) is full, and the cycle closes it to none.
#[test]
fn cycle_browser_closes_an_undocked_browser_view_to_none() {
    let mut state = dock_state();
    toggle_view(&mut state);
    assert_eq!(browser_state(&state), (false, true));
    cycle(&mut state);
    assert_eq!(browser_state(&state), (false, false));
}

/// Fork (cycle_browser): a refused step keeps the state (no companion).
#[test]
fn cycle_browser_with_the_companion_off_stays_none() {
    let mut state = dock_state();
    // host = "window": browser.get has no companion_app
    state.browser.info = Some(BrowserGetInfo {
        seq: 2,
        enabled: true,
        window_off: false,
        ..Default::default()
    });
    recompose(&mut state);
    cycle(&mut state);
    assert_eq!(browser_state(&state), (false, false));
    let notice = state.visible_endpoint_notice.take().expect("says why");
    assert_eq!(notice.title, "Browser dock");
}

/// Fork (cycle_browser): the default `ctrl+shift+b` is a direct binding that
/// fires on a real ctrl+shift+b report (kitty CSI u, with or without the
/// shifted alternate, or modifyOtherKeys), while plain ctrl+b (legacy 0x02
/// or CSI u) stays the prefix and never cycles.
#[test]
fn fork_smoke_ctrl_shift_b_cycles_the_browser_and_ctrl_b_stays_the_prefix() {
    for bytes in [
        &b"\x1b[98;6u"[..],
        &b"\x1b[98:66;6u"[..],
        &b"\x1b[27;6;98~"[..],
    ] {
        let mut state = dock_state();
        state.handle_input_bytes(bytes);
        assert_eq!(state.mode, ClientShellMode::Terminal, "{bytes:?}");
        assert_eq!(browser_state(&state), (true, false), "{bytes:?} docks");
        recompose(&mut state);
        state.handle_input_bytes(bytes);
        assert_eq!(browser_state(&state), (true, true), "{bytes:?} views");
        state.handle_input_bytes(bytes);
        assert_eq!(browser_state(&state), (false, false), "{bytes:?} closes");
    }
    for bytes in [&b"\x02"[..], &b"\x1b[98;5u"[..]] {
        let mut state = dock_state();
        state.handle_input_bytes(bytes);
        assert_eq!(
            state.mode,
            ClientShellMode::Prefix,
            "{bytes:?} is the prefix"
        );
        assert_eq!(
            browser_state(&state),
            (false, false),
            "{bytes:?} never cycles"
        );
    }
}

/// Fork (cycle_browser, audit): a held ctrl+shift+b steps once; the kitty
/// autorepeat (event type 2) never spins the cycle on.
#[test]
fn a_held_ctrl_shift_b_cycles_once_not_on_its_autorepeat() {
    let mut state = dock_state();
    state.set_host_reports_key_releases(true);
    state.handle_input_bytes(b"\x1b[98;6u");
    assert_eq!(browser_state(&state), (true, false));
    recompose(&mut state);
    // checked after each one: three steps would come round to docked again
    for repeat in 1..=3 {
        state.handle_input_bytes(b"\x1b[98;6:2u");
        assert_eq!(
            browser_state(&state),
            (true, false),
            "repeat {repeat} does nothing"
        );
    }
    state.handle_input_bytes(b"\x1b[98;6:3u");
    assert_eq!(browser_state(&state), (true, false), "nor does the release");
}

/// Fork (cycle_browser, audit): docked while the companion goes off, the
/// cycle closes the dock instead of asking for a view that is refused.
#[test]
fn cycle_browser_closes_the_dock_when_the_companion_is_off() {
    let mut state = dock_state();
    cycle(&mut state);
    assert_eq!(browser_state(&state), (true, false));
    // host = "window": browser.get has no companion_app
    state.browser.info = Some(BrowserGetInfo {
        seq: 2,
        enabled: true,
        window_off: false,
        ..Default::default()
    });
    recompose(&mut state);
    cycle(&mut state);
    assert_eq!(browser_state(&state), (false, false));
}

/// Fork (browser keys): the companion's cycle key, relayed by the server
/// (`endpoint.view-cycle.v1`), steps the focused tab like ctrl+shift+b in
/// this terminal (none → docked → Browser view → none), once per relay, and
/// only for the active endpoint and the tab the companion showed.
#[test]
fn fork_smoke_a_view_cycle_from_the_companion_steps_like_the_key() {
    use crate::server::headless::view_cycle::ViewCyclePayload;
    let mut state = dock_state();
    let local = crate::client::endpoint::ClientEndpointId::Local;
    let relay = |state: &mut ClientShellState,
                 endpoint: &crate::client::endpoint::ClientEndpointId,
                 boot: &str,
                 revision,
                 tab: Option<&str>| {
        let outcome = state.receive_browser_view_cycle(
            endpoint,
            ViewCyclePayload {
                boot_id: boot.into(),
                revision,
                tab_id: tab.map(str::to_owned),
            },
        );
        recompose(state);
        outcome
    };
    let outcome = relay(&mut state, &local, "boot", 1, Some("tab_1"));
    assert_eq!(browser_state(&state), (true, false), "none → docked");
    assert!(outcome.resize, "the band narrows the panes");
    // the same relay twice acts once
    relay(&mut state, &local, "boot", 1, Some("tab_1"));
    assert_eq!(browser_state(&state), (true, false));
    relay(&mut state, &local, "boot", 2, Some("tab_1"));
    assert_eq!(browser_state(&state), (true, true), "docked → full");
    // a tab other than the focused one: nothing
    relay(&mut state, &local, "boot", 3, Some("tab_2"));
    assert_eq!(browser_state(&state), (true, true));
    assert!(!state.browser_dock_open_for("tab_2") && !state.browser_view_open_for("tab_2"));
    // another endpoint (an SSH server's relay): nothing
    let remote = crate::client::endpoint::ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
            .expect("profile id"),
    );
    relay(&mut state, &remote, "boot", 4, Some("tab_1"));
    assert_eq!(browser_state(&state), (true, true));
    // no tab named: the focused one
    relay(&mut state, &local, "boot", 5, None);
    assert_eq!(browser_state(&state), (false, false), "full → none");
    // a new server boot (a live handoff) counts from revision 1 again
    relay(&mut state, &local, "boot-2", 1, Some("tab_1"));
    assert_eq!(browser_state(&state), (true, false));
}

/// Fork (browser keys audit): a client that cannot dock here (its server
/// lacks `browser.placement.set`) ignores the companion's cycle quietly: no
/// dock, no Browser view, no "update the server" notice.
#[test]
fn a_view_cycle_on_a_client_that_cannot_dock_is_ignored_quietly() {
    use crate::server::headless::view_cycle::ViewCyclePayload;
    let mut state = dock_state();
    state
        .endpoints
        .iter_mut()
        .find(|endpoint| endpoint.endpoint_id == state.active_endpoint_id)
        .expect("the local endpoint")
        .methods = Some(vec!["browser.get".to_owned()].into_iter().collect());
    assert!(!state.browser_dock_available());
    let outcome = state.receive_browser_view_cycle(
        &crate::client::endpoint::ClientEndpointId::Local,
        ViewCyclePayload {
            boot_id: "boot".into(),
            revision: 1,
            tab_id: Some("tab_1".into()),
        },
    );
    assert!(!outcome.repaint && !outcome.resize);
    assert_eq!(browser_state(&state), (false, false));
    assert!(state.visible_endpoint_notice.is_none(), "no notice");
}

// ---------------------------------------------------------------------------
// Fork (remote browser): a saved machine's browser helper

fn machine() -> crate::client::endpoint::ClientEndpointId {
    crate::client::endpoint::ClientEndpointId::Ssh(
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef")
            .expect("profile id"),
    )
}

fn helper_says(link: &str, companion: &str) -> super::super::browser_link::LinkedBrowser {
    super::super::browser_link::LinkedBrowser {
        link: link.into(),
        companion: BrowserCompanionAppInfo {
            state: companion.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The dock state showing a saved machine whose helper is attached; the
/// machine's own server says it runs no companion.
fn linked_state() -> ClientShellState {
    let mut state = dock_state();
    state.active_endpoint_id = machine();
    state.browser.info = Some(BrowserGetInfo {
        seq: 1,
        enabled: true,
        companion_app: None,
        ..Default::default()
    });
    state
        .browser_link
        .set(&machine(), Some(helper_says("linked", "running")), None);
    recompose(&mut state);
    state
}

fn link_placements(state: &mut ClientShellState) -> Vec<(String, Option<String>, bool)> {
    state
        .take_browser_link_intents()
        .into_iter()
        .filter_map(|intent| match intent {
            super::super::browser_link::LinkIntent::Placement {
                endpoint,
                placement,
            } => Some((
                endpoint.storage_key(),
                placement.spec.map(|spec| spec.tab_id),
                placement.active,
            )),
            super::super::browser_link::LinkIntent::Companion { .. }
            | super::super::browser_link::LinkIntent::ForgetLogins { .. } => None,
        })
        .collect()
}

/// A saved machine docks only while its helper is attached; without it the
/// notice says why, and nothing is published anywhere.
#[test]
fn a_machine_without_its_helper_offers_no_dock_and_says_why() {
    let mut state = dock_state();
    state.active_endpoint_id = machine();
    assert!(!state.browser_dock_available());
    let outcome = toggle_dock(&mut state);
    assert!(!outcome.resize);
    assert!(state.layout(COLS, ROWS).browser_band.is_empty());
    let notice = state.visible_endpoint_notice.as_ref().expect("a notice");
    assert_eq!(
        notice.body,
        super::super::browser_link::NOT_LINKED_REASON,
        "{}",
        notice.body
    );
    assert!(state
        .take_browser_placement_request(std::time::Instant::now())
        .is_none());
    assert!(state.take_browser_link_intents().is_empty());
    // the helper attaches: offered; it goes: gone again
    state.browser_link.set(
        &machine(),
        Some(helper_says("reconnecting", "stopped")),
        None,
    );
    assert!(state.browser_dock_available() && state.browser_dock_offered());
    state
        .browser_link
        .set(&machine(), None, Some("helper gone".into()));
    assert!(!state.browser_dock_available());
}

/// `herdr --remote` (its endpoint is Local) offers no dock, no strip and
/// no placement; the notice names the way that works.
#[test]
fn fork_smoke_over_herdr_remote_there_is_no_dock_and_the_reason_is_said() {
    let mut state = dock_state_with(&spaces_config());
    state.browser_link.remote_client = true;
    recompose(&mut state);
    assert!(state.active_endpoint_id.is_local());
    assert!(!state.browser_dock_available() && !state.browser_dock_offered());
    let outcome = toggle_dock(&mut state);
    assert!(!outcome.resize);
    assert!(state.layout(COLS, ROWS).browser_band.is_empty());
    assert!(!state.browser_view_strip_for_focused_tab(), "no strip");
    let notice = state.visible_endpoint_notice.as_ref().expect("a notice");
    assert_eq!(
        notice.body,
        "The browser is not available over herdr --remote; add the machine (herdr machine add)."
    );
    toggle_view(&mut state);
    assert!(!state.browser_view_open_for("tab_1"));
    assert!(state
        .take_browser_placement_request(std::time::Instant::now())
        .is_none());
}

/// A linked machine docks through its helper: the companion, its report
/// on the placement and the user's companion actions are the helper's;
/// the machine's server gets no placement (it never learns this Mac's
/// pids).
#[test]
fn fork_smoke_a_linked_machine_docks_through_its_helper_never_its_server() {
    let mut state = linked_state();
    assert!(
        state.browser_dock_offered(),
        "the server's own companion is off"
    );
    let outcome = toggle_dock(&mut state);
    assert!(outcome.resize, "the band narrows the panes");
    assert!(!state.layout(COLS, ROWS).browser_band.is_empty());
    let now = std::time::Instant::now();
    assert!(
        state.take_browser_placement_request(now).is_none(),
        "nothing for the machine's server"
    );
    assert_eq!(
        link_placements(&mut state),
        [(machine().storage_key(), Some("tab_1".into()), true)]
    );
    // unchanged: nothing more
    assert!(state
        .take_browser_placement_request(now + std::time::Duration::from_secs(1))
        .is_none());
    assert!(link_placements(&mut state).is_empty());
    // the helper's report on this placement drives the placeholder
    let mut said = helper_says("linked", "running");
    said.reports = vec![BrowserPlacementStatus {
        id: state.browser_dock.placement_id.clone(),
        state: "unbound".into(),
        ..Default::default()
    }];
    state.browser_link.set(&machine(), Some(said), None);
    assert_eq!(
        state.browser_placeholder(now).click,
        Some(super::super::browser_dock::BrowserDockTarget::Nudge)
    );
    // the user's restart goes to the helper
    let mut outcome = ClientShellInput::default();
    state.request_browser_companion(
        crate::api::schema::BrowserCompanionAction::Restart,
        &mut outcome,
    );
    assert!(outcome.requests.is_empty());
    assert_eq!(
        state.take_browser_link_intents(),
        [super::super::browser_link::LinkIntent::Companion {
            endpoint: machine(),
            action: crate::api::schema::BrowserCompanionAction::Restart,
        }]
    );
}

/// On a machine switch the machine left hides its browser (`active:
/// false`) before the endpoint shown publishes its own placement.
#[test]
fn a_machine_switch_hides_the_machine_left_before_the_next_is_placed() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let t0 = std::time::Instant::now();
    assert!(sent_placement(&mut state, t0).is_some(), "Local's own");
    // to the machine, docked there too
    state.active_endpoint_id = machine();
    state
        .browser_link
        .set(&machine(), Some(helper_says("linked", "running")), None);
    toggle_dock(&mut state);
    let t1 = t0 + std::time::Duration::from_secs(1);
    assert!(state.take_browser_placement_request(t1).is_none());
    assert_eq!(
        link_placements(&mut state),
        [(machine().storage_key(), Some("tab_1".into()), true)]
    );
    // back to Local, whose placement changed meanwhile (a narrower band)
    state.active_endpoint_id = crate::client::endpoint::ClientEndpointId::Local;
    state.browser_dock.percent = 30;
    recompose(&mut state);
    let t2 = t1 + std::time::Duration::from_secs(1);
    assert!(
        state.take_browser_placement_request(t2).is_none(),
        "Local waits a gap behind the hide"
    );
    assert_eq!(
        link_placements(&mut state),
        [(machine().storage_key(), Some("tab_1".into()), false)],
        "the machine left hides, its area kept"
    );
    assert!(state.next_browser_dock_deadline(t2).is_some());
    let spec = sent_placement(&mut state, t2 + std::time::Duration::from_millis(40))
        .expect("Local's placement after the gap")
        .expect("spec");
    assert!(spec.shown);
    assert!(link_placements(&mut state).is_empty(), "hidden once");
}

/// The helper attaching or going changes the band: the outcome resizes;
/// while its link is down and no companion runs, the area says so, and
/// another Mac holding the machine's browser is named.
#[test]
fn the_helper_coming_and_going_resizes_and_the_link_state_is_shown() {
    let mut state = linked_state();
    toggle_dock(&mut state);
    let mut outcome = ClientShellInput::default();
    state.receive_browser_link(&machine(), None, None, Vec::new(), Vec::new(), &mut outcome);
    assert!(outcome.repaint && outcome.resize, "the band goes");
    recompose(&mut state);
    assert!(state.layout(COLS, ROWS).browser_band.is_empty());
    let mut outcome = ClientShellInput::default();
    state.receive_browser_link(
        &machine(),
        Some(helper_says("reconnecting", "stopped")),
        None,
        Vec::new(),
        Vec::new(),
        &mut outcome,
    );
    assert!(outcome.repaint && outcome.resize, "the band comes back");
    recompose(&mut state);
    let now = std::time::Instant::now();
    let label = state.endpoint_label(&machine()).to_owned();
    let placeholder = state.browser_placeholder(now);
    assert_eq!(placeholder.lines[0].0, format!("{label}: reconnecting"));
    assert!(placeholder.spinner.is_some());
    assert_eq!(state.browser_companion_stopped(), Some("link reconnecting"));
    let mut said = helper_says("in_use", "stopped");
    said.text = Some("the browser of this machine is on another Mac (since 14:02)".into());
    state.browser_link.set(&machine(), Some(said), None);
    let placeholder = state.browser_placeholder(now);
    assert_eq!(
        placeholder.lines[0].0,
        format!("The browser of {label} is on another Mac")
    );
    assert!(placeholder.lines[1].0.contains("since 14:02"));
    // an other endpoint's news repaints nothing here
    let mut outcome = ClientShellInput::default();
    state.receive_browser_link(
        &crate::client::endpoint::ClientEndpointId::Local,
        None,
        None,
        Vec::new(),
        Vec::new(),
        &mut outcome,
    );
    assert!(!outcome.repaint && !outcome.resize);
}

/// A pane surface frame of `cols`×`rows` for projection `revision`.
fn sized_surface(cols: u16, rows: u16, revision: u64) -> PaneSurfaceFrame {
    let mut surface = surface();
    surface.projection_revision = revision;
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::empty(Rect::new(0, 0, cols, rows)),
        None,
        &[],
    );
    surface
}

/// The frame composed now reaches the terminal.
fn present(state: &mut ClientShellState) {
    let drawn = state.compose(COLS, ROWS).and_then(|frame| frame.drawn);
    state.note_presented(drawn);
}

fn placement_request_id(message: &crate::protocol::ClientMessage) -> String {
    let crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } = message else {
        panic!("a placement is an endpoint request");
    };
    serde_json::from_str::<Request>(request)
        .expect("request json")
        .id
}

/// Fork (dock sync, F4): a placement the server refused goes again after a
/// pause (never at once: a refusal can repeat), one it took does not, and a
/// new connection to the same server gets it again (the server forgot it
/// with the old connection).
#[test]
fn a_refused_placement_or_a_new_connection_gets_it_again() {
    let mut state = dock_state();
    toggle_dock(&mut state);
    let t0 = std::time::Instant::now();
    let message = state.take_browser_placement_request(t0).expect("sent");
    state.receive_browser_placement_answer(&placement_request_id(&message), false, t0);
    let soon = t0 + std::time::Duration::from_millis(100);
    assert!(sent_placement(&mut state, soon).is_none(), "no busy loop");
    let later = t0 + std::time::Duration::from_secs(2);
    let message = state
        .take_browser_placement_request(later)
        .expect("sent again after the pause");
    state.receive_browser_placement_answer(&placement_request_id(&message), true, later);
    assert!(sent_placement(&mut state, later + std::time::Duration::from_secs(5)).is_none());
    // the same server, a new connection
    state.set_endpoint_snapshot_for_generation(
        &crate::client::endpoint::ClientEndpointId::Local,
        2,
        Box::new(two_tab_snapshot("tab_1")),
    );
    let spec = sent_placement(&mut state, later + std::time::Duration::from_secs(6))
        .expect("placed again")
        .expect("spec");
    assert_eq!(spec.tab_id, "tab_1");
}

/// Fork (dock sync, F5): a placement newly shown (the dock opened, a switch
/// into a docked tab) waits for a drawn frame of its tab that makes room:
/// the band drawn and the tab's text no wider than the pane left of it.
/// Once shown, a divider drag (the text re-flowing a moment later) never
/// hides it.
#[test]
fn a_new_placement_is_shown_only_over_a_frame_that_makes_room() {
    let mut state = dock_state();
    let wide = state.layout(COLS, ROWS).pane_surface;
    state.set_pane_surface(sized_surface(wide.width, wide.height, 1));
    present(&mut state);
    let t0 = std::time::Instant::now();
    let mut outcome = ClientShellInput::default();
    state.toggle_browser_dock_for_focused_tab(&mut outcome);
    let spec = sent_placement(&mut state, t0).expect("sent").expect("spec");
    assert!(!spec.shown, "no frame with the band is on screen yet");
    // the band is drawn, the text still the width before
    present(&mut state);
    let t1 = t0 + std::time::Duration::from_secs(1);
    assert!(
        sent_placement(&mut state, t1).is_none(),
        "still hidden: the text runs under the band"
    );
    // the narrower surface arrives (the PTY followed the report)
    let narrow = state.layout(COLS, ROWS).pane_surface;
    assert!(narrow.width < wide.width);
    state.set_pane_surface(sized_surface(narrow.width, narrow.height, 1));
    present(&mut state);
    let spec = sent_placement(&mut state, t1 + std::time::Duration::from_secs(1))
        .expect("sent")
        .expect("spec");
    assert!(spec.shown);
    // a divider drag: the surface is a moment behind, the window stays
    let divider = state.hits.browser_dock.divider;
    state.handle_raw_events(vec![
        mouse(MouseEventKind::Down(MouseButton::Left), divider.x, 10),
        mouse(MouseEventKind::Drag(MouseButton::Left), divider.x - 12, 10),
    ]);
    present(&mut state);
    let spec = sent_placement(&mut state, t1 + std::time::Duration::from_secs(2))
        .expect("the area moved")
        .expect("spec");
    assert!(spec.shown, "a drag never blinks the window");
    // another tab and back: the snapshot lands before its surface
    state.handle_raw_events(vec![mouse(
        MouseEventKind::Up(MouseButton::Left),
        divider.x - 12,
        10,
    )]);
    let mut second = two_tab_snapshot("tab_2");
    second.revision = 2;
    state.set_snapshot(Box::new(second));
    let spec = sent_placement(&mut state, t1 + std::time::Duration::from_secs(3))
        .expect("sent")
        .expect("spec");
    assert!(!spec.shown);
    let mut back = two_tab_snapshot("tab_1");
    back.revision = 3;
    state.set_snapshot(Box::new(back));
    let spec = sent_placement(&mut state, t1 + std::time::Duration::from_secs(4))
        .expect("sent")
        .expect("spec");
    assert!(!spec.shown, "tab_2's frame is still on screen");
    let pane = state.layout(COLS, ROWS).pane_surface;
    state.set_pane_surface(sized_surface(pane.width, pane.height, 3));
    present(&mut state);
    let spec = sent_placement(&mut state, t1 + std::time::Duration::from_secs(5))
        .expect("sent")
        .expect("spec");
    assert!(spec.shown);
}
