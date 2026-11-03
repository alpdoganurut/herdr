//! The browser dock and Browser view on the client (fork, companion
//! launcher; design `.local/prd/companion/launcher-design.md` §3–§4).
//!
//! The companion browser (the Electron app the server runs) can sit over
//! part of this client's terminal window: docked on the right of a herdr
//! tab (a band the layout reserves, its divider dragged like the info
//! pane's) or over the whole tab (Browser view, switched in the tab's view
//! strip). The client only reserves and publishes the area
//! (`browser.placement.set`, a fact about this client: no dock, band or
//! strip in the API); the companion finds the terminal window and covers
//! the area. Under it the client draws a placeholder that says why the
//! browser is not there when it is not (`browser.get` `companion_app` and
//! the companion's report on this placement).
//!
//! Everything here is presentation state, per client process, in memory:
//! which tabs have the dock or Browser view, the dock's width (remembered
//! in the chrome preferences once dragged). Only the local endpoint docks:
//! the companion runs on this machine. Nothing here runs per pane, but the
//! layout (asked on every pane surface patch) asks for the band and the
//! strip: with no dock, no Browser view and no browser pages that is a few
//! emptiness checks; otherwise set lookups by `&str` and one scan of the
//! server's browser sessions, the endpoint's method check last, and no
//! allocation. The terminal's pixel extent is read once per resize, never
//! per input event.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::*;
use crate::api::schema::{
    BrowserCellRect, BrowserCompanionAction, BrowserGridSize, BrowserPlacementKind,
    BrowserPlacementSetParams, BrowserPlacementSpec, BrowserPxSize, Method,
};

/// The narrowest band (its divider included).
pub(crate) const BAND_MIN: u16 = 40;
/// The narrowest pane surface left beside the band.
pub(crate) const TERM_MIN: u16 = 30;
/// The most often a changed placement is published (a divider drag).
const PLACEMENT_GAP: Duration = Duration::from_millis(33);
const DIVIDER_DOUBLE_CLICK: Duration = Duration::from_millis(350);
/// The Browser view's swallowed-key toast, once per tab this often.
const GUARD_TOAST_EVERY: Duration = Duration::from_secs(60);
/// The placeholder's spinner while the companion starts.
const SPINNER: [&str; 4] = ["\u{25D0}", "\u{25D3}", "\u{25D1}", "\u{25D2}"]; // ◐◓◑◒
const SPIN_EVERY: Duration = Duration::from_millis(250);
/// A companion that has said nothing about a placement this long is not
/// docking it (an older companion).
const UNANSWERED_AFTER: Duration = Duration::from_secs(4);
/// The request id prefix of a placement (its answer is not waited for).
pub(crate) const PLACEMENT_REQUEST_PREFIX: &str = "client-shell-placement:";

/// A click target of the band, the strip, the placeholder or the Browser row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserDockTarget {
    /// The strip's `▣ Terminal`.
    ViewTerminal,
    /// The strip's `◎ Browser`.
    ViewBrowser,
    /// The strip's (or the Browser row's) `◨`.
    ToggleDock,
    /// The placeholder's `[ Start companion ]` (and the strip's `start`).
    Start,
    /// The placeholder's `[ Restart ]`.
    Restart,
    /// The unbound placeholder: bind to the frontmost terminal window.
    Nudge,
    /// Fork (session header): the dock title bar's `⤢`: Browser view, the
    /// dock slot freed.
    Promote,
    /// Fork (session header): the dock title bar's `✕`.
    Close,
}

/// What the band, strip and placeholder drew (the mouse's targets).
#[derive(Debug, Default, Clone)]
pub(crate) struct BrowserDockHits {
    /// The band, its divider included.
    pub(crate) band: Rect,
    pub(crate) divider: Rect,
    /// The Browser view's area (empty while the tab shows its terminal).
    pub(crate) full: Rect,
    pub(crate) targets: Vec<(Rect, BrowserDockTarget)>,
}

impl BrowserDockHits {
    fn resolve(&self, point: (u16, u16)) -> Option<BrowserDockTarget> {
        self.targets
            .iter()
            .rev()
            .find(|(rect, _)| super::contains(*rect, point))
            .map(|(_, target)| *target)
    }
}

/// How a placeholder line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaceholderTone {
    Title,
    Body,
    Dim,
    Error,
}

/// What the area under the browser says while the browser is not over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placeholder {
    /// A calm placeholder: only a faint `◎` (the browser covers it, or is
    /// hidden for a moment while the window moves).
    pub(crate) calm: bool,
    /// The spinner frame while the companion starts.
    pub(crate) spinner: Option<&'static str>,
    pub(crate) lines: Vec<(String, PlaceholderTone)>,
    pub(crate) button: Option<(&'static str, BrowserDockTarget)>,
    /// A click anywhere on the area (the unbound state).
    pub(crate) click: Option<BrowserDockTarget>,
}

impl Placeholder {
    fn calm() -> Self {
        Self {
            calm: true,
            spinner: None,
            lines: Vec::new(),
            button: None,
            click: None,
        }
    }

    fn say(lines: Vec<(String, PlaceholderTone)>) -> Self {
        Self {
            calm: false,
            spinner: None,
            lines,
            button: None,
            click: None,
        }
    }
}

/// The view strip, as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewStrip {
    /// Browser view is on.
    pub(crate) browser: bool,
    pub(crate) docked: bool,
    /// The tab's pages (`browser.get` `sessions`).
    pub(crate) pages: u32,
    /// `◎` lit: an agent used the browser for this tab just now.
    pub(crate) lit: bool,
    /// `◉`: a page of the tab needs the user.
    pub(crate) attention: bool,
    /// The companion is not running: `◌ companion stopped · start`.
    pub(crate) stopped: Option<&'static str>,
}

/// A herdr tab's browser facts for the tab row and the detail strip: its
/// pages and the view it is in (`docked`, `browser view`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TabBrowserMark {
    pub(crate) pages: u32,
    pub(crate) view: Option<&'static str>,
}

/// The last placement sent, for the server boot it went to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentPlacement {
    boot_id: String,
    spec: Option<BrowserPlacementSpec>,
}

/// Tab ids by endpoint; an endpoint without any is not listed (so an empty
/// map means none anywhere).
pub(crate) type TabsByEndpoint = HashMap<ClientEndpointId, HashSet<String>>;

fn tab_listed(tabs: &TabsByEndpoint, endpoint: &ClientEndpointId, tab_id: &str) -> bool {
    !tabs.is_empty()
        && tabs
            .get(endpoint)
            .is_some_and(|listed| listed.contains(tab_id))
}

fn list_tab(tabs: &mut TabsByEndpoint, endpoint: &ClientEndpointId, tab_id: String) -> bool {
    tabs.entry(endpoint.clone()).or_default().insert(tab_id)
}

fn unlist_tab(tabs: &mut TabsByEndpoint, endpoint: &ClientEndpointId, tab_id: &str) -> bool {
    let Some(listed) = tabs.get_mut(endpoint) else {
        return false;
    };
    let removed = listed.remove(tab_id);
    if listed.is_empty() {
        tabs.remove(endpoint);
    }
    removed
}

/// The client's dock and Browser view state (see the module docs).
#[derive(Debug)]
pub(crate) struct ClientBrowserDockState {
    /// Tabs with the dock open, by endpoint.
    pub(crate) docked: TabsByEndpoint,
    /// Tabs in Browser view, by endpoint.
    pub(crate) viewing: TabsByEndpoint,
    /// The dock's width in percent of the area right of the sidebar.
    pub(crate) percent: u16,
    /// Dragged: only then remembered.
    pub(crate) percent_manual: bool,
    pub(crate) dragging: bool,
    pub(crate) last_divider_press: Option<Instant>,
    /// This client process's placement id.
    pub(crate) placement_id: String,
    sent: Option<SentPlacement>,
    sent_at: Option<Instant>,
    /// When a placement was first sent for this boot (cleared with it): a
    /// companion that never reports on it is an old one.
    first_sent_at: Option<Instant>,
    /// A changed placement waits for the gap after the last one.
    pending: bool,
    /// The last tick that found a placement waiting: the next waits a gap
    /// too (a tick that cannot send, the endpoint offline or an activation
    /// pending, never turns into a busy loop).
    pending_ticked_at: Option<Instant>,
    /// Bumped by a click on the unbound placeholder.
    pub(crate) nudge: u32,
    guard_toasts: HashMap<String, Instant>,
    /// The live alignment smoke (`scripts/smoke_companion_dock.sh`):
    /// `HERDR_TEST_BROWSER_VIEW=side|full` docks (or opens Browser view on)
    /// the first focused tab once the server runs the companion, without
    /// input (the smoke cannot type into its terminal window).
    test_view: Option<BrowserPlacementKind>,
    /// The terminal's `(cols, rows, width px, height px)` (`TIOCGWINSZ`;
    /// zeros when it does not say), read when a placement needs it and
    /// again after a terminal resize (`note_terminal_resized`) or when a dock
    /// or Browser view opens: `None` until then.
    terminal_extent: Option<(u16, u16, u32, u32)>,
    /// Tests: what the terminal says (no `/dev/tty`), and how often it was asked.
    #[cfg(test)]
    pub(crate) extent_for_test: Option<(u16, u16, u32, u32)>,
    #[cfg(test)]
    pub(crate) extent_reads: u32,
}

fn placement_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_nanos())
            .unwrap_or(0),
    );
    format!("pl-{:016x}", hasher.finish())
}

impl ClientBrowserDockState {
    pub(crate) fn new(percent: Option<u16>, default_percent: u16) -> Self {
        Self {
            docked: HashMap::new(),
            viewing: HashMap::new(),
            percent: percent.unwrap_or(default_percent).clamp(
                crate::config::MIN_BROWSER_DOCK_WIDTH,
                crate::config::MAX_BROWSER_DOCK_WIDTH,
            ),
            percent_manual: percent.is_some(),
            dragging: false,
            last_divider_press: None,
            placement_id: placement_id(),
            sent: None,
            sent_at: None,
            first_sent_at: None,
            pending: false,
            pending_ticked_at: None,
            nudge: 0,
            guard_toasts: HashMap::new(),
            test_view: match std::env::var("HERDR_TEST_BROWSER_VIEW").as_deref() {
                Ok("side") => Some(BrowserPlacementKind::Side),
                Ok("full") => Some(BrowserPlacementKind::Full),
                _ => None,
            },
            terminal_extent: None,
            #[cfg(test)]
            extent_for_test: None,
            #[cfg(test)]
            extent_reads: 0,
        }
    }

    /// Ask the terminal for its extent (one `/dev/tty` open and ioctl).
    fn read_terminal_extent(&mut self) -> (u16, u16, u32, u32) {
        #[cfg(test)]
        {
            self.extent_reads += 1;
            if let Some(extent) = self.extent_for_test {
                return extent;
            }
        }
        match crossterm::terminal::window_size() {
            Ok(size) if size.columns > 0 && size.rows > 0 => (
                size.columns,
                size.rows,
                u32::from(size.width),
                u32::from(size.height),
            ),
            _ => (0, 0, 0, 0),
        }
    }
}

/// This client and its ancestors, nearest first (at most 16): the
/// companion finds the terminal window by them. Read once per process.
fn client_pids() -> Vec<u32> {
    static PIDS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    PIDS.get_or_init(|| {
        let mut pids = crate::platform::process_ancestry(std::process::id());
        pids.truncate(16);
        pids
    })
    .clone()
}

/// The Browser view's area: everything right of the sidebar below the
/// strip (the pane surface, the info dock and the band), so a toggle never
/// resizes the panes. Fork (session header): in the tabs layout the info
/// dock is the dock slot and stays beside the Browser view.
pub(super) fn full_area(layout: &ClientShellLayout) -> Rect {
    let mut area = layout.pane_surface;
    let info_dock = if layout.session_header.is_empty() {
        layout.info_dock
    } else {
        Rect::default()
    };
    for rect in [info_dock, layout.browser_band] {
        if !rect.is_empty() {
            area = area.union(rect);
        }
    }
    area
}

/// What one frame of the strip, the band and the Browser view's area shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserDockView {
    pub(crate) browser_view: bool,
    pub(crate) strip: Option<ViewStrip>,
    pub(crate) placeholder: Option<Placeholder>,
    pub(crate) dragging: bool,
    /// Fork (session header): the band's title bar (the tabs layout): the
    /// focused tab's page title, else its URL, else `Browser`.
    pub(crate) title: Option<String>,
}

/// Draw `view` into the chrome buffer and record its targets in `hits` (the
/// Browser row's `◨`, drawn by the sidebar, stays). Returns whether the
/// focused tab is in Browser view (the caller hides its panes).
pub(super) fn compose(
    buffer: &mut Buffer,
    layout: &ClientShellLayout,
    view: &BrowserDockView,
    palette: &Palette,
    hits: &mut BrowserDockHits,
) -> bool {
    hits.band = Rect::default();
    hits.divider = Rect::default();
    hits.full = Rect::default();
    hits.targets.clear();
    if let Some(strip) = view.strip.as_ref() {
        super::browser_dock_render::render_view_strip(
            buffer,
            layout.view_strip,
            strip,
            palette,
            hits,
        );
    }
    if let Some(placeholder) = view.placeholder.as_ref() {
        if view.browser_view {
            super::browser_dock_render::render_full(buffer, full_area(layout), placeholder, hits);
        } else {
            super::browser_dock_render::render_band(
                buffer,
                layout.browser_band,
                view.dragging,
                view.title.as_deref(),
                placeholder,
                hits,
            );
        }
    }
    view.browser_view
}

/// The narrowest line a notice gets in Browser view.
const BANNER_MIN: u16 = 16;

/// Where herdr's notice, toast or agent cards show while the focused tab is
/// in Browser view (the companion covers the whole tab, cards drawn there
/// would be under it): one line on the view strip, between its switch and
/// its `◨` (as composed into `hits`), else the right half of the tab bar.
/// `None` outside Browser view, or with neither row (the cards stay where
/// they are, under the browser).
pub(super) fn banner_area(
    layout: &ClientShellLayout,
    browser_view: bool,
    hits: &BrowserDockHits,
    header: &super::session_header::SessionHeaderHits,
) -> Option<Rect> {
    if !browser_view {
        return None;
    }
    // Fork (session header): the header's gap before the view tabs.
    if !layout.session_header.is_empty() {
        return (!header.banner.is_empty()).then_some(header.banner);
    }
    let strip = layout.view_strip;
    if !strip.is_empty() {
        let on_strip = |rect: &Rect| rect.y == strip.y && rect.height == 1;
        let start = hits
            .targets
            .iter()
            .filter(|(rect, target)| on_strip(rect) && *target != BrowserDockTarget::ToggleDock)
            .map(|(rect, _)| rect.right())
            .max()
            .unwrap_or(strip.x)
            .saturating_add(1);
        let end = hits
            .targets
            .iter()
            .find(|(rect, target)| on_strip(rect) && *target == BrowserDockTarget::ToggleDock)
            .map_or(strip.right(), |(rect, _)| rect.x)
            .saturating_sub(1);
        if end >= start.saturating_add(BANNER_MIN) {
            return Some(Rect::new(start, strip.y, end - start, 1));
        }
    }
    let bar = layout.tab_bar;
    let half = bar.width / 2;
    (!bar.is_empty() && half >= BANNER_MIN).then(|| Rect::new(bar.right() - half, bar.y, half, 1))
}

/// What right-anchored toasts keep left of: the info dock and the band.
pub(super) fn toast_dock(layout: &ClientShellLayout) -> Rect {
    match (layout.info_dock.is_empty(), layout.browser_band.is_empty()) {
        (true, true) => Rect::default(),
        (false, true) => layout.info_dock,
        (true, false) => layout.browser_band,
        (false, false) => layout.info_dock.union(layout.browser_band),
    }
}

impl ClientShellState {
    /// What the strip, the band and the Browser view's area show now
    /// (part of `compute_view`; [`compose`] draws it).
    pub(super) fn browser_dock_view(&self, layout: &ClientShellLayout) -> BrowserDockView {
        let browser_view =
            self.browser_view_open_for_focused_tab() && !full_area(layout).is_empty();
        BrowserDockView {
            browser_view,
            strip: (!layout.view_strip.is_empty())
                .then(|| self.browser_view_strip())
                .flatten(),
            placeholder: (browser_view || !layout.browser_band.is_empty())
                .then(|| self.browser_placeholder(Instant::now())),
            dragging: matches!(self.chrome_drag, Some(ClientChromeDrag::BrowserDockWidth)),
            title: (!layout.session_header.is_empty() && !layout.browser_band.is_empty())
                .then(|| self.browser_dock_title()),
        }
    }

    /// Fork (session header): the browser dock's title: the focused tab's
    /// active page (`browser.get` `sessions`), its title else its URL, else
    /// `Browser`.
    fn browser_dock_title(&self) -> String {
        self.browser_dock_focused_tab()
            .and_then(|tab_id| {
                self.browser
                    .info
                    .as_ref()?
                    .sessions
                    .iter()
                    .find(|session| session.tab_id == tab_id)
            })
            .and_then(|session| {
                [session.title.trim(), session.url.trim()]
                    .into_iter()
                    .find(|text| !text.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "Browser".to_owned())
    }

    fn browser_dock_focused_tab(&self) -> Option<&str> {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_tab_id.as_deref())
    }

    /// Docking works here: the local endpoint, a server that takes
    /// placements.
    pub(crate) fn browser_dock_available(&self) -> bool {
        self.active_endpoint_id.is_local()
            && self.supports_endpoint_method(&Method::BrowserPlacementSet(
                BrowserPlacementSetParams::default(),
            ))
    }

    /// The dock's buttons and menu items show: docking works here and the
    /// server runs the companion (`browser.get` `companion_app`).
    pub(crate) fn browser_dock_offered(&self) -> bool {
        self.browser_companion().is_some() && self.browser_dock_available()
    }

    pub(crate) fn browser_dock_open_for(&self, tab_id: &str) -> bool {
        tab_listed(&self.browser_dock.docked, &self.active_endpoint_id, tab_id)
    }

    pub(crate) fn browser_view_open_for(&self, tab_id: &str) -> bool {
        tab_listed(&self.browser_dock.viewing, &self.active_endpoint_id, tab_id)
    }

    /// A tab of the active endpoint has the dock or Browser view.
    pub(crate) fn browser_dock_in_use(&self) -> bool {
        self.browser_dock
            .docked
            .contains_key(&self.active_endpoint_id)
            || self
                .browser_dock
                .viewing
                .contains_key(&self.active_endpoint_id)
    }

    // The lookups first, the endpoint's method check last (these run on
    // every layout).
    pub(crate) fn browser_dock_open_for_focused_tab(&self) -> bool {
        self.browser_dock_focused_tab()
            .is_some_and(|tab_id| self.browser_dock_open_for(tab_id))
            && self.browser_dock_available()
    }

    pub(crate) fn browser_view_open_for_focused_tab(&self) -> bool {
        self.browser_dock_focused_tab()
            .is_some_and(|tab_id| self.browser_view_open_for(tab_id))
            && self.browser_dock_available()
    }

    /// The band to carve for the focused tab (its width in percent), `None`
    /// while undocked (the layout still drops a band that does not fit).
    pub(crate) fn browser_band_for_focused_tab(&self) -> Option<u16> {
        if self.browser_dock.docked.is_empty() {
            return None;
        }
        self.browser_dock_open_for_focused_tab()
            .then_some(self.browser_dock.percent)
    }

    /// `tab_id`'s browser pages (`browser.get` `sessions`).
    pub(crate) fn browser_tab_pages(&self, tab_id: &str) -> u32 {
        self.browser
            .info
            .as_ref()
            .and_then(|info| {
                info.sessions
                    .iter()
                    .find(|session| session.tab_id == tab_id)
            })
            .map_or(0, |session| session.pages)
    }

    /// Tabs with browser pages, or the dock or Browser view (the dim `◎`
    /// and the detail strip's chips): one map per compose, O(sessions).
    pub(crate) fn browser_page_tabs(&self) -> HashMap<String, TabBrowserMark> {
        let mut tabs = HashMap::new();
        if let Some(info) = self.browser.info.as_ref() {
            for session in info.sessions.iter().filter(|session| session.pages > 0) {
                tabs.insert(
                    session.tab_id.clone(),
                    TabBrowserMark {
                        pages: session.pages,
                        view: None,
                    },
                );
            }
        }
        let endpoint = &self.active_endpoint_id;
        for (view, set) in [
            ("browser view", &self.browser_dock.viewing),
            ("docked", &self.browser_dock.docked),
        ] {
            for tab_id in set.get(endpoint).into_iter().flatten() {
                let mark = tabs.entry(tab_id.clone()).or_insert(TabBrowserMark {
                    pages: 0,
                    view: None,
                });
                // Browser view wins over the dock (it covers it)
                mark.view.get_or_insert(view);
            }
        }
        tabs
    }

    /// The focused tab shows its view strip: `ui.browser_view_strip =
    /// "auto"`, docking works here, and the tab has the dock, Browser view
    /// or pages while the server runs the companion (the lookups first, the
    /// sessions scan next, the endpoint's method check last: this runs on
    /// every layout).
    pub(crate) fn browser_view_strip_for_focused_tab(&self) -> bool {
        if !self.config.browser_view_strip {
            return false;
        }
        let Some(tab_id) = self.browser_dock_focused_tab() else {
            return false;
        };
        (self.browser_dock_open_for(tab_id)
            || self.browser_view_open_for(tab_id)
            || (self.browser_companion().is_some() && self.browser_tab_pages(tab_id) > 0))
            && self.browser_dock_available()
    }

    /// The band in the last composed layout (empty when not drawn).
    pub(crate) fn browser_band_area(&self) -> Rect {
        self.last_composed_size
            .map(|(cols, rows)| self.layout(cols, rows).browser_band)
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ toggles

    /// Why docking does not work here, for a notice.
    fn browser_dock_unavailable_reason(&self) -> &'static str {
        if !self.active_endpoint_id.is_local() {
            "The browser docks only for the herdr running on this machine."
        } else {
            "This herdr server cannot dock the browser yet; update and restart it."
        }
    }

    /// Why nothing docks although docking works here: the server answered
    /// `browser.get` without the companion, which its config keeps off.
    /// `None` before the first answer, or while the companion is there.
    fn browser_companion_off_reason(&self) -> Option<&'static str> {
        let info = self.browser.info.as_ref()?;
        if info.companion_app.is_some() {
            return None;
        }
        Some(if info.enabled {
            "This herdr uses only its own browser window ([browser] host = \"window\")."
        } else {
            "The browser is off ([browser] enabled = false)."
        })
    }

    /// A dock or Browser view about to open while the companion is off:
    /// the notice says why, and nothing opens.
    fn browser_dock_refused(&mut self, title: &str, outcome: &mut ClientShellInput) -> bool {
        let Some(reason) = self.browser_companion_off_reason() else {
            return false;
        };
        outcome.repaint |= self.push_endpoint_notice(
            ClientEndpointNoticeKind::Rejected,
            "browser_dock:off",
            title,
            reason,
        );
        true
    }

    /// A stopped companion starts when the user docks or opens Browser view.
    fn start_companion_for_dock(&mut self, outcome: &mut ClientShellInput) {
        if self
            .browser_companion()
            .is_some_and(|companion| companion.state == "stopped")
        {
            self.request_browser_companion(BrowserCompanionAction::Start, outcome);
        }
    }

    /// Open or close the dock of `tab_id` on the active endpoint. A tab
    /// that is not focused changes state only.
    pub(crate) fn toggle_browser_dock(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        if !self.browser_dock_available() {
            let reason = self.browser_dock_unavailable_reason();
            outcome.repaint |= self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "browser_dock:unavailable",
                "Browser dock",
                reason,
            );
            return;
        }
        // a dock always closes; it opens only while the server runs the companion
        if !self.browser_dock_open_for(&tab_id)
            && self.browser_dock_refused("Browser dock", outcome)
        {
            return;
        }
        let size_before = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let focused = self.browser_dock_focused_tab() == Some(tab_id.as_str());
        // Fork (session header): one dock slot in the tabs layout.
        if !self.browser_dock_open_for(&tab_id) {
            let endpoint = self.active_endpoint_id.clone();
            self.free_dock_slot_of_notes(&endpoint, &tab_id);
        }
        let opened = if unlist_tab(
            &mut self.browser_dock.docked,
            &self.active_endpoint_id,
            &tab_id,
        ) {
            false
        } else {
            list_tab(
                &mut self.browser_dock.docked,
                &self.active_endpoint_id,
                tab_id.clone(),
            )
        };
        self.browser_dock.dragging = false;
        outcome.repaint = true;
        if opened {
            // The terminal is asked for its extent again (a missed resize).
            self.browser_dock.terminal_extent = None;
            self.start_companion_for_dock(outcome);
            // The placeholder follows the companion's state at the active cadence.
            self.refresh_browser();
        }
        if !focused {
            return;
        }
        let size_after = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        if size_before != size_after {
            outcome.resize = true;
        }
        if opened
            && !self.browser_view_open_for(&tab_id)
            && self.last_composed_size.is_some()
            && self.browser_band_area().is_empty()
        {
            self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "browser_dock:narrow",
                "Browser dock",
                "The window is too narrow for the browser dock; it appears once the window is wider.",
            );
        }
    }

    /// Fork (session header): the tabs layout has one dock slot, so the
    /// notes dock opening closes `tab_id`'s browser dock (state only; the
    /// caller resizes). Returns whether it was open.
    pub(crate) fn free_dock_slot_of_browser(
        &mut self,
        endpoint: &ClientEndpointId,
        tab_id: &str,
    ) -> bool {
        if self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs {
            return false;
        }
        let closed = unlist_tab(&mut self.browser_dock.docked, endpoint, tab_id);
        if closed {
            self.browser_dock.dragging = false;
        }
        closed
    }

    /// The `toggle_browser_dock` keybind: the focused tab.
    pub(crate) fn toggle_browser_dock_for_focused_tab(&mut self, outcome: &mut ClientShellInput) {
        if let Some(tab_id) = self.browser_dock_focused_tab().map(str::to_owned) {
            self.toggle_browser_dock(tab_id, outcome);
        }
    }

    /// Browser view of `tab_id` on or off. Never resizes the panes, except
    /// that a tab's first view strip takes one row.
    pub(crate) fn set_browser_view(
        &mut self,
        tab_id: String,
        on: bool,
        outcome: &mut ClientShellInput,
    ) {
        if !self.browser_dock_available() {
            let reason = self.browser_dock_unavailable_reason();
            outcome.repaint |= self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "browser_dock:unavailable",
                "Browser view",
                reason,
            );
            return;
        }
        if on
            && !self.browser_view_open_for(&tab_id)
            && self.browser_dock_refused("Browser view", outcome)
        {
            return;
        }
        let size_before = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let changed = if on {
            list_tab(
                &mut self.browser_dock.viewing,
                &self.active_endpoint_id,
                tab_id.clone(),
            )
        } else {
            unlist_tab(
                &mut self.browser_dock.viewing,
                &self.active_endpoint_id,
                &tab_id,
            )
        };
        if !changed {
            return;
        }
        outcome.repaint = true;
        if on {
            self.browser_dock.terminal_extent = None;
            self.start_companion_for_dock(outcome);
            self.refresh_browser();
        }
        let size_after = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        if self.browser_dock_focused_tab() == Some(tab_id.as_str()) && size_before != size_after {
            outcome.resize = true;
        }
    }

    pub(crate) fn toggle_browser_view(&mut self, tab_id: String, outcome: &mut ClientShellInput) {
        let on = !self.browser_view_open_for(&tab_id);
        self.set_browser_view(tab_id, on, outcome);
    }

    /// The `toggle_browser_view` keybind: the focused tab.
    pub(crate) fn toggle_browser_view_for_focused_tab(&mut self, outcome: &mut ClientShellInput) {
        if let Some(tab_id) = self.browser_dock_focused_tab().map(str::to_owned) {
            self.toggle_browser_view(tab_id, outcome);
        }
    }

    /// The `focus_browser` keybind: the companion takes the keyboard.
    pub(crate) fn focus_browser(&mut self, outcome: &mut ClientShellInput) {
        if self.browser_companion().is_some() {
            self.request_browser_companion(BrowserCompanionAction::Focus, outcome);
        }
    }

    /// Keep only docks and views whose tab the active endpoint still lists.
    pub(crate) fn prune_browser_dock_tabs(&mut self, snapshot: &ClientShellSnapshot) {
        let listed = |tab_id: &String| snapshot.tabs.iter().any(|tab| &tab.tab_id == tab_id);
        for tabs in [
            &mut self.browser_dock.docked,
            &mut self.browser_dock.viewing,
        ] {
            if let Some(open) = tabs.get_mut(&self.active_endpoint_id) {
                open.retain(listed);
                if open.is_empty() {
                    tabs.remove(&self.active_endpoint_id);
                }
            }
        }
        self.browser_dock
            .guard_toasts
            .retain(|tab_id, _| snapshot.tabs.iter().any(|tab| &tab.tab_id == tab_id));
    }

    // ------------------------------------------------------------ width

    /// The widest band the current window allows.
    fn browser_dock_max_percent(&self) -> u16 {
        crate::config::MAX_BROWSER_DOCK_WIDTH
    }

    /// A drag of the divider to `column`: the band runs from it to the
    /// right edge.
    pub(crate) fn browser_dock_drag(&mut self, column: u16, outcome: &mut ClientShellInput) {
        let Some((cols, rows)) = self.last_composed_size else {
            return;
        };
        let layout = self.layout(cols, rows);
        let main_x = layout.sidebar.right();
        let main_width = cols.saturating_sub(main_x).max(1);
        let right = if layout.browser_band.is_empty() {
            cols
        } else {
            layout.browser_band.right()
        };
        let width = right.saturating_sub(column);
        // Rounded, so the band lands on the column the divider was dragged to.
        let percent =
            ((u32::from(width) * 100 + u32::from(main_width) / 2) / u32::from(main_width)) as u16;
        let percent = percent.clamp(
            crate::config::MIN_BROWSER_DOCK_WIDTH,
            self.browser_dock_max_percent(),
        );
        self.browser_dock.last_divider_press = None;
        if percent == self.browser_dock.percent {
            return;
        }
        self.browser_dock.percent = percent;
        self.browser_dock.percent_manual = true;
        outcome.repaint = true;
        outcome.resize = true;
    }

    /// The divider drag ended: keep and remember the width.
    pub(crate) fn browser_dock_release(&mut self, outcome: &mut ClientShellInput) {
        self.browser_dock.dragging = false;
        outcome.repaint = true;
        self.persist_chrome_preferences(outcome);
    }

    // ------------------------------------------------------------ mouse

    /// A left press on the band, the strip, the Browser view's area or the
    /// Browser row's `◨` (checked before any pane hit). Returns whether the
    /// press was theirs.
    pub(crate) fn browser_dock_press(
        &mut self,
        point: (u16, u16),
        outcome: &mut ClientShellInput,
    ) -> bool {
        let hits = &self.hits.browser_dock;
        if !hits.divider.is_empty() && super::contains(hits.divider, point) {
            let now = Instant::now();
            let double_click = self
                .browser_dock
                .last_divider_press
                .is_some_and(|last| now.duration_since(last) <= DIVIDER_DOUBLE_CLICK);
            if double_click {
                self.browser_dock.last_divider_press = None;
                self.browser_dock.dragging = false;
                self.browser_dock.percent = self.config.browser_dock_width;
                self.browser_dock.percent_manual = false;
                outcome.resize = true;
                outcome.repaint = true;
                self.persist_chrome_preferences(outcome);
                return true;
            }
            self.browser_dock.last_divider_press = Some(now);
            self.browser_dock.dragging = true;
            self.chrome_drag = Some(ClientChromeDrag::BrowserDockWidth);
            outcome.repaint = true;
            return true;
        }
        let target = hits.resolve(point);
        let inside = super::contains(hits.band, point) || super::contains(hits.full, point);
        let Some(target) = target else {
            // The band and the Browser view's area belong to the browser: a
            // press there never reaches a pane.
            return inside;
        };
        let tab_id = self.browser_dock_focused_tab().map(str::to_owned);
        match target {
            BrowserDockTarget::ViewTerminal => {
                if let Some(tab_id) = tab_id {
                    self.set_browser_view(tab_id, false, outcome);
                }
            }
            BrowserDockTarget::ViewBrowser => {
                if let Some(tab_id) = tab_id {
                    self.set_browser_view(tab_id, true, outcome);
                }
            }
            BrowserDockTarget::ToggleDock => self.toggle_browser_dock_for_focused_tab(outcome),
            BrowserDockTarget::Start => {
                self.request_browser_companion(BrowserCompanionAction::Start, outcome);
            }
            BrowserDockTarget::Restart => {
                self.request_browser_companion(BrowserCompanionAction::Restart, outcome);
            }
            BrowserDockTarget::Nudge => {
                self.browser_dock.nudge = self.browser_dock.nudge.wrapping_add(1);
                outcome.repaint = true;
            }
            BrowserDockTarget::Promote => {
                if let Some(tab_id) = tab_id {
                    self.set_browser_view(tab_id.clone(), true, outcome);
                    if self.browser_view_open_for(&tab_id) && self.browser_dock_open_for(&tab_id) {
                        self.toggle_browser_dock(tab_id, outcome);
                    }
                }
            }
            BrowserDockTarget::Close => self.toggle_browser_dock_for_focused_tab(outcome),
        }
        true
    }

    // ------------------------------------------------------------ keys

    /// Browser view hides the focused tab's panes: a key typed while the
    /// terminal window has the keyboard never reaches them. The first one
    /// says how to type into the page (once per tab per minute).
    pub(crate) fn browser_view_swallows_key(&mut self, outcome: &mut ClientShellInput) -> bool {
        if !self.browser_view_open_for_focused_tab() {
            return false;
        }
        let Some(tab_id) = self.browser_dock_focused_tab().map(str::to_owned) else {
            return false;
        };
        let now = Instant::now();
        let due = self
            .browser_dock
            .guard_toasts
            .get(&tab_id)
            .is_none_or(|at| now.duration_since(*at) >= GUARD_TOAST_EVERY);
        if due {
            self.browser_dock.guard_toasts.insert(tab_id, now);
            let body = self.browser_view_guard_text(now);
            outcome.repaint |= self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "browser_view:keys",
                "Browser view",
                body,
            );
        }
        true
    }

    /// The swallowed key's toast: how to type into the page, or, while no
    /// page covers the tab, what the area says to do (Start, Restart, the
    /// setup command, a click to dock).
    fn browser_view_guard_text(&self, now: Instant) -> String {
        let key = self.browser_view_key_hint();
        let what = match self
            .browser_companion()
            .map(|companion| companion.state.as_str())
        {
            Some("stopped") => "The companion is not running: click Start companion".to_owned(),
            Some("not_installed") => {
                "The companion is not installed: run herdr browser setup --companion".to_owned()
            }
            Some("failed") => format!(
                "The companion stopped after {}: click Restart",
                crate::browser::companion_app::GIVE_UP_TEXT
            ),
            Some("starting") => "The companion is starting".to_owned(),
            Some("reconnecting") => "The companion is reconnecting".to_owned(),
            Some("crashed") => "The companion is restarting".to_owned(),
            Some("stopping") => "The companion is stopping".to_owned(),
            _ => {
                let placeholder = self.browser_placeholder(now);
                match (placeholder.click, placeholder.lines.first()) {
                    (_, _) if placeholder.calm => "Click the page to type there".to_owned(),
                    (Some(BrowserDockTarget::Nudge), _) => {
                        "Click the browser's area once to dock it to this window".to_owned()
                    }
                    (_, Some((title, _))) => match placeholder.button {
                        Some((label, _)) => format!("{title}: click {label}"),
                        None => title.clone(),
                    },
                    (_, None) => "Click the page to type there".to_owned(),
                }
            }
        };
        format!("{what}; {key} shows the terminal again.")
    }

    /// `^b Z` (the toggle_browser_view binding, compact), else its name.
    pub(crate) fn browser_view_key_hint(&self) -> String {
        self.config
            .keybinds
            .keybinds
            .toggle_browser_view
            .bindings
            .first()
            .map(|binding| {
                super::context_menu_look::compact_trigger(
                    binding.trigger.is_prefix(),
                    binding.trigger.combo(),
                    self.config.keybinds.primary_prefix(),
                )
            })
            .unwrap_or_else(|| "the Terminal tab".to_owned())
    }

    // ------------------------------------------------------------ what is drawn

    /// The view strip of the focused tab (`None` when it has none).
    pub(crate) fn browser_view_strip(&self) -> Option<ViewStrip> {
        let tab_id = self.browser_dock_focused_tab()?;
        let pages = self.browser_tab_pages(tab_id);
        let attention = self.browser_attention_tabs().contains(tab_id);
        let lit = self.browser_marked_tabs().contains(tab_id);
        let stopped =
            self.browser_companion()
                .and_then(|companion| match companion.state.as_str() {
                    "stopped" => Some("companion stopped"),
                    "not_installed" => Some("companion not installed"),
                    "failed" => Some("companion failed"),
                    _ => None,
                });
        Some(ViewStrip {
            browser: self.browser_view_open_for(tab_id),
            docked: self.browser_dock_open_for(tab_id),
            pages,
            lit,
            attention,
            stopped,
        })
    }

    /// The companion's report on this client's placement.
    fn browser_placement_status(&self) -> Option<&crate::api::schema::BrowserPlacementStatus> {
        let id = &self.browser_dock.placement_id;
        self.browser
            .info
            .as_ref()?
            .placements
            .iter()
            .find(|status| &status.id == id)
    }

    /// What the area under the browser says, for the focused tab's label.
    pub(crate) fn browser_placeholder(&self, now: Instant) -> Placeholder {
        let tab_label = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| {
                let tab_id = snapshot.focused_tab_id.as_deref()?;
                snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)
            })
            .map(|tab| tab.label.clone())
            .unwrap_or_default();
        let Some(companion) = self.browser_companion() else {
            // No `browser.get` yet, or a server without the companion.
            return Placeholder::calm();
        };
        let title = |text: String| (text, PlaceholderTone::Title);
        let body = |text: &str| (text.to_owned(), PlaceholderTone::Body);
        match companion.state.as_str() {
            "not_installed" => Placeholder::say(vec![
                title("The companion is not installed".into()),
                // Two short lines: a ~50-column band never strands a word.
                body("In a terminal, run"),
                body("herdr browser setup --companion"),
                (
                    format!(
                        "(it downloads Electron, {})",
                        crate::browser::companion_install::DOWNLOAD_SIZE
                    ),
                    PlaceholderTone::Dim,
                ),
            ]),
            "stopped" => Placeholder {
                button: Some(("Start companion", BrowserDockTarget::Start)),
                ..Placeholder::say(vec![title("The companion is not running".into())])
            },
            "failed" => {
                let mut lines = vec![title(format!(
                    "The companion stopped after {}",
                    crate::browser::companion_app::GIVE_UP_TEXT
                ))];
                if let Some(error) = companion.last_error.as_deref() {
                    let first = error.lines().next().unwrap_or(error);
                    lines.push((first.to_owned(), PlaceholderTone::Error));
                }
                Placeholder {
                    button: Some(("Restart", BrowserDockTarget::Restart)),
                    ..Placeholder::say(lines)
                }
            }
            "starting" | "reconnecting" | "crashed" | "stopping" => {
                let frame = (now.duration_since(self.browser_dock_epoch()).as_millis()
                    / SPIN_EVERY.as_millis()) as usize
                    % SPINNER.len();
                let text = match companion.state.as_str() {
                    "crashed" => "Restarting the companion…",
                    "stopping" => "Stopping the companion…",
                    "reconnecting" => "Reconnecting the companion…",
                    _ => "Starting the companion…",
                };
                let heading = if tab_label.is_empty() {
                    "Browser".to_owned()
                } else {
                    format!("Browser · {tab_label}")
                };
                Placeholder {
                    spinner: Some(SPINNER[frame]),
                    ..Placeholder::say(vec![title(heading), body(text)])
                }
            }
            _ => match self.browser_placement_status() {
                Some(status) => match status.state.as_str() {
                    "unbound" => Placeholder {
                        click: Some(BrowserDockTarget::Nudge),
                        ..Placeholder::say(vec![title(
                            "Click here once to dock the browser to this window".into(),
                        )])
                    },
                    "unsupported" => Placeholder::say(vec![title(
                        status
                            .reason
                            .clone()
                            .unwrap_or_else(|| "The browser cannot dock to this terminal".into()),
                    )]),
                    "elsewhere" => Placeholder::say(vec![title(
                        "The browser is docked to another herdr window".into(),
                    )]),
                    _ => Placeholder::calm(),
                },
                // A companion that never reports on the placement predates
                // docking (herdr was updated, the companion was not restarted).
                // It must have been up a while too: a companion the dock just
                // started is still binding.
                None if self
                    .browser_dock
                    .first_sent_at
                    .is_some_and(|at| now.duration_since(at) >= UNANSWERED_AFTER)
                    && companion.since_unix.is_none_or(|since| {
                        super::browser::unix_now().saturating_sub(since)
                            >= UNANSWERED_AFTER.as_secs()
                    }) =>
                {
                    if companion.state == "external" {
                        Placeholder::say(vec![
                            title("The companion has not docked".into()),
                            body("it was started by hand; quit it and start it from herdr"),
                        ])
                    } else {
                        Placeholder {
                            button: Some(("Restart", BrowserDockTarget::Restart)),
                            ..Placeholder::say(vec![
                                title("The companion has not docked".into()),
                                body(if companion.update_ready {
                                    "a newer companion is installed: Restart applies it"
                                } else {
                                    "Restart it if this lasts"
                                }),
                            ])
                        }
                    }
                }
                None => Placeholder::calm(),
            },
        }
    }

    /// The instant spinner frames count from (any fixed point works).
    fn browser_dock_epoch(&self) -> Instant {
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *EPOCH.get_or_init(Instant::now)
    }

    /// Whether the placeholder animates (the next frame is due).
    fn browser_placeholder_spins(&self) -> bool {
        (self.browser_dock_open_for_focused_tab() || self.browser_view_open_for_focused_tab())
            && self.browser_companion().is_some_and(|companion| {
                matches!(
                    companion.state.as_str(),
                    "starting" | "reconnecting" | "crashed" | "stopping"
                )
            })
    }

    // ------------------------------------------------------------ placement

    /// Whether anything of herdr's own is over `area`: a modal hides the
    /// browser; a menu only where it overlaps.
    fn herdr_ui_over(&self, area: Rect) -> bool {
        match &self.overlay {
            None => false,
            Some(ClientShellOverlay::ContextMenu(_)) => {
                self.hits.context_menu_area.intersects(area)
            }
            Some(ClientShellOverlay::GlobalMenu(_)) => self
                .hits
                .global_menu_rows
                .iter()
                .any(|(rect, _)| rect.intersects(area)),
            Some(_) => true,
        }
    }

    /// This client's placement now (`None`: none to publish).
    fn browser_placement_spec(&self) -> Option<BrowserPlacementSpec> {
        if !self.browser_dock_in_use() || !self.browser_dock_available() {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        let tab_id = snapshot.focused_tab_id.clone()?;
        let (cols, rows) = self.last_composed_size?;
        let layout = self.layout(cols, rows);
        let viewing = self.browser_view_open_for(&tab_id);
        let (kind, area, shown) = if viewing {
            let area = full_area(&layout);
            (BrowserPlacementKind::Full, area, !area.is_empty())
        } else if !layout.browser_band.is_empty() {
            let band = layout.browser_band;
            // Fork (session header): below the band's title bar.
            let top = u16::from(!layout.session_header.is_empty()).min(band.height);
            let area = Rect::new(
                band.x.saturating_add(1),
                band.y + top,
                band.width.saturating_sub(1),
                band.height - top,
            );
            (BrowserPlacementKind::Side, area, !area.is_empty())
        } else {
            // Another tab has the browser: keep the companion's dock mode
            // across the switch, uncovered.
            let viewing_elsewhere = self
                .browser_dock
                .viewing
                .contains_key(&self.active_endpoint_id);
            let kind = if !self.browser_dock_open_for(&tab_id) && viewing_elsewhere {
                BrowserPlacementKind::Full
            } else {
                BrowserPlacementKind::Side
            };
            (kind, Rect::default(), false)
        };
        let shown = shown && !self.herdr_ui_over(area);
        let (grid_cols, grid_rows, width_px, height_px) = self.browser_terminal_pixels(cols, rows);
        let cell = |extent: u32, cells: u16| {
            if cells == 0 {
                0
            } else {
                extent / u32::from(cells)
            }
        };
        Some(BrowserPlacementSpec {
            id: self.browser_dock.placement_id.clone(),
            kind,
            tab_id,
            area: BrowserCellRect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: area.height,
            },
            grid: BrowserGridSize {
                cols: grid_cols,
                rows: grid_rows,
            },
            cell_px: BrowserPxSize {
                width: cell(width_px, grid_cols),
                height: cell(height_px, grid_rows),
            },
            extent_px: BrowserPxSize {
                width: width_px,
                height: height_px,
            },
            pids: client_pids(),
            shown,
            nudge: self.browser_dock.nudge,
        })
    }

    /// The terminal's grid and pixel extent (`TIOCGWINSZ`, as last read);
    /// the composed size and no pixels when the terminal does not say.
    fn browser_terminal_pixels(&self, cols: u16, rows: u16) -> (u16, u16, u32, u32) {
        match self.browser_dock.terminal_extent {
            Some((grid_cols, grid_rows, width, height)) if grid_cols > 0 && grid_rows > 0 => {
                (grid_cols, grid_rows, width, height)
            }
            _ => (cols, rows, 0, 0),
        }
    }

    /// The terminal resized: its extent is read again when a placement
    /// needs it (the extent beyond whole cells matters to the companion, so
    /// it is never derived from the grid).
    pub(crate) fn note_terminal_resized(&mut self) {
        self.browser_dock.terminal_extent = None;
    }

    /// The `browser.placement.set` to send now, if the placement changed
    /// (at most one per 33 ms: a change inside the gap waits for the
    /// deadline). Sent beside the command lane; its answer is not waited for.
    pub(crate) fn take_browser_placement_request(
        &mut self,
        now: Instant,
    ) -> Option<crate::protocol::ClientMessage> {
        // Nothing docked and nothing left to clear: nothing to do (this runs
        // after every input event), nothing waits for the gap either.
        let in_use = self.browser_dock_in_use();
        if !in_use
            && self
                .browser_dock
                .sent
                .as_ref()
                .is_none_or(|sent| sent.spec.is_none())
        {
            self.browser_dock.pending = false;
            return None;
        }
        if in_use && self.browser_dock.terminal_extent.is_none() {
            self.browser_dock.terminal_extent = Some(self.browser_dock.read_terminal_extent());
        }
        let boot_id = self.snapshot.as_deref()?.boot_id.clone();
        let spec = self.browser_placement_spec();
        let current = SentPlacement {
            boot_id: boot_id.clone(),
            spec,
        };
        match &self.browser_dock.sent {
            Some(sent) if *sent == current => {
                self.browser_dock.pending = false;
                return None;
            }
            // Never sent for this boot and nothing to say: nothing to clear.
            None if current.spec.is_none() => return None,
            Some(sent) if sent.boot_id != boot_id && current.spec.is_none() => {
                self.browser_dock.sent = None;
                return None;
            }
            _ => {}
        }
        if self
            .browser_dock
            .sent_at
            .is_some_and(|at| now.duration_since(at) < PLACEMENT_GAP)
        {
            self.browser_dock.pending = true;
            return None;
        }
        let request_id = format!("{PLACEMENT_REQUEST_PREFIX}{}", self.next_request_id);
        self.next_request_id = self.next_request_id.saturating_add(1);
        let request = crate::api::schema::Request {
            id: request_id,
            method: Method::BrowserPlacementSet(BrowserPlacementSetParams {
                placement: current.spec.clone(),
            }),
        };
        let request = serde_json::to_string(&request).ok()?;
        let fresh = !matches!(
            &self.browser_dock.sent,
            Some(sent) if sent.boot_id == current.boot_id && sent.spec.is_some()
        );
        if current.spec.is_none() {
            self.browser_dock.first_sent_at = None;
        } else if fresh {
            self.browser_dock.first_sent_at = Some(now);
        }
        self.browser_dock.sent = Some(current);
        self.browser_dock.sent_at = Some(now);
        self.browser_dock.pending = false;
        Some(crate::protocol::ClientMessage::ClientShellEndpointRequest { boot_id, request })
    }

    /// When the tick loop must wake for the dock: a placement waiting for
    /// its gap (a gap after the last send and after the last tick that
    /// found it waiting), the placeholder's next spinner frame.
    pub(crate) fn next_browser_dock_deadline(&self, now: Instant) -> Option<Instant> {
        let pending = self.browser_dock.pending.then(|| {
            let gap_after = |at: Option<Instant>| at.map_or(now, |at| at + PLACEMENT_GAP);
            gap_after(self.browser_dock.sent_at)
                .max(gap_after(self.browser_dock.pending_ticked_at))
                .max(now)
        });
        let spin = self.browser_placeholder_spins().then(|| now + SPIN_EVERY);
        match (pending, spin) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The tick: a spinning placeholder repaints; a waiting placement goes
    /// out after it (the runtime takes it, no frame needed); the smoke's
    /// test view opens once docking is offered.
    pub(crate) fn tick_browser_dock(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if self.browser_dock.pending {
            self.browser_dock.pending_ticked_at = Some(now);
        }
        if self.browser_dock.test_view.is_some() && self.browser_dock_offered() {
            if let Some(tab_id) = self.browser_dock_focused_tab().map(str::to_owned) {
                match self.browser_dock.test_view.take() {
                    Some(BrowserPlacementKind::Full) => {
                        self.set_browser_view(tab_id, true, outcome)
                    }
                    Some(_) => self.toggle_browser_dock(tab_id, outcome),
                    None => {}
                }
            }
        }
        if self.browser_placeholder_spins() {
            outcome.repaint = true;
        }
    }
}
