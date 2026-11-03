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
//! the companion runs on this machine. Nothing here runs per pane: the
//! layout asks two set lookups and one scan of the server's browser
//! sessions per call.

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
}

/// What the band, strip and placeholder drew (the mouse's targets).
#[derive(Debug, Default, Clone)]
pub(crate) struct BrowserDockHits {
    /// The band, its divider included.
    pub(crate) band: Rect,
    pub(crate) divider: Rect,
    /// The Browser view's area (empty while the tab shows its terminal).
    pub(crate) full: Rect,
    /// The Browser row's `◨`.
    pub(crate) row_button: Rect,
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

/// The last placement sent, for the server boot it went to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentPlacement {
    boot_id: String,
    spec: Option<BrowserPlacementSpec>,
}

/// The client's dock and Browser view state (see the module docs).
#[derive(Debug)]
pub(crate) struct ClientBrowserDockState {
    /// Tabs with the dock open, by endpoint.
    pub(crate) docked: HashSet<(ClientEndpointId, String)>,
    /// Tabs in Browser view, by endpoint.
    pub(crate) viewing: HashSet<(ClientEndpointId, String)>,
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
    /// Bumped by a click on the unbound placeholder.
    pub(crate) nudge: u32,
    guard_toasts: HashMap<String, Instant>,
    /// The live alignment smoke (`scripts/smoke_companion_dock.sh`):
    /// `HERDR_TEST_BROWSER_VIEW=side|full` docks (or opens Browser view on)
    /// the first focused tab once the server runs the companion, without
    /// input (the smoke cannot type into its terminal window).
    test_view: Option<BrowserPlacementKind>,
    /// Tests: the terminal's `(cols, rows, width px, height px)`.
    #[cfg(test)]
    pub(crate) pixels_for_test: Option<(u16, u16, u32, u32)>,
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
            docked: HashSet::new(),
            viewing: HashSet::new(),
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
            nudge: 0,
            guard_toasts: HashMap::new(),
            test_view: match std::env::var("HERDR_TEST_BROWSER_VIEW").as_deref() {
                Ok("side") => Some(BrowserPlacementKind::Side),
                Ok("full") => Some(BrowserPlacementKind::Full),
                _ => None,
            },
            #[cfg(test)]
            pixels_for_test: None,
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
/// resizes the panes.
pub(super) fn full_area(layout: &ClientShellLayout) -> Rect {
    let mut area = layout.pane_surface;
    for rect in [layout.info_dock, layout.browser_band] {
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
                placeholder,
                hits,
            );
        }
    }
    view.browser_view
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
        }
    }

    fn browser_dock_key(&self, tab_id: &str) -> (ClientEndpointId, String) {
        (self.active_endpoint_id.clone(), tab_id.to_owned())
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
        self.browser_dock
            .docked
            .contains(&self.browser_dock_key(tab_id))
    }

    pub(crate) fn browser_view_open_for(&self, tab_id: &str) -> bool {
        self.browser_dock
            .viewing
            .contains(&self.browser_dock_key(tab_id))
    }

    /// A tab of the active endpoint has the dock or Browser view.
    pub(crate) fn browser_dock_in_use(&self) -> bool {
        self.browser_dock
            .docked
            .iter()
            .chain(self.browser_dock.viewing.iter())
            .any(|(endpoint, _)| *endpoint == self.active_endpoint_id)
    }

    pub(crate) fn browser_dock_open_for_focused_tab(&self) -> bool {
        self.browser_dock_available()
            && self
                .browser_dock_focused_tab()
                .is_some_and(|tab_id| self.browser_dock_open_for(tab_id))
    }

    pub(crate) fn browser_view_open_for_focused_tab(&self) -> bool {
        self.browser_dock_available()
            && self
                .browser_dock_focused_tab()
                .is_some_and(|tab_id| self.browser_view_open_for(tab_id))
    }

    /// The band to carve for the focused tab (its width in percent), `None`
    /// while undocked (the layout still drops a band that does not fit).
    pub(crate) fn browser_band_for_focused_tab(&self) -> Option<u16> {
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

    /// The focused tab shows its view strip: `ui.browser_view_strip =
    /// "auto"`, docking works here, and the tab has pages, the dock or
    /// Browser view.
    pub(crate) fn browser_view_strip_for_focused_tab(&self) -> bool {
        if !self.config.browser_view_strip || !self.browser_dock_available() {
            return false;
        }
        let Some(tab_id) = self.browser_dock_focused_tab() else {
            return false;
        };
        self.browser_dock_open_for(tab_id)
            || self.browser_view_open_for(tab_id)
            || self.browser_tab_pages(tab_id) > 0
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
        let size_before = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let focused = self.browser_dock_focused_tab() == Some(tab_id.as_str());
        let key = self.browser_dock_key(&tab_id);
        let opened = if self.browser_dock.docked.remove(&key) {
            false
        } else {
            self.browser_dock.docked.insert(key);
            true
        };
        self.browser_dock.dragging = false;
        outcome.repaint = true;
        if opened {
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
        let size_before = self
            .last_composed_size
            .map(|(cols, rows)| self.surface_size(cols, rows));
        let key = self.browser_dock_key(&tab_id);
        let changed = if on {
            self.browser_dock.viewing.insert(key)
        } else {
            self.browser_dock.viewing.remove(&key)
        };
        if !changed {
            return;
        }
        outcome.repaint = true;
        if on {
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
        let endpoint_id = self.active_endpoint_id.clone();
        let listed = |(endpoint, tab_id): &(ClientEndpointId, String)| {
            *endpoint != endpoint_id || snapshot.tabs.iter().any(|tab| &tab.tab_id == tab_id)
        };
        self.browser_dock.docked.retain(listed);
        self.browser_dock.viewing.retain(listed);
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
        if super::contains(hits.row_button, point) {
            self.toggle_browser_dock_for_focused_tab(outcome);
            return true;
        }
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
            let key = self.browser_view_key_hint();
            outcome.repaint |= self.push_endpoint_notice(
                ClientEndpointNoticeKind::Rejected,
                "browser_view:keys",
                "Browser view",
                format!("Click the page to type there; {key} shows the terminal again."),
            );
        }
        true
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
                (
                    "run \"herdr browser setup --companion\" in a terminal".into(),
                    PlaceholderTone::Body,
                ),
                (
                    "(it downloads Electron, about 115 MB)".into(),
                    PlaceholderTone::Dim,
                ),
            ]),
            "stopped" => Placeholder {
                button: Some(("Start companion", BrowserDockTarget::Start)),
                ..Placeholder::say(vec![title("The companion is not running".into())])
            },
            "failed" => {
                let mut lines = vec![title("The companion stopped after 5 crashes".into())];
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
        if !self.browser_dock_available() {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        let tab_id = snapshot.focused_tab_id.clone()?;
        let (cols, rows) = self.last_composed_size?;
        if !self.browser_dock_in_use() {
            return None;
        }
        let layout = self.layout(cols, rows);
        let viewing = self.browser_view_open_for(&tab_id);
        let (kind, area, shown) = if viewing {
            let area = full_area(&layout);
            (BrowserPlacementKind::Full, area, !area.is_empty())
        } else if !layout.browser_band.is_empty() {
            let band = layout.browser_band;
            let area = Rect::new(
                band.x.saturating_add(1),
                band.y,
                band.width.saturating_sub(1),
                band.height,
            );
            (BrowserPlacementKind::Side, area, !area.is_empty())
        } else {
            // Another tab has the browser: keep the companion's dock mode
            // across the switch, uncovered.
            let viewing_elsewhere = self
                .browser_dock
                .viewing
                .iter()
                .any(|(endpoint, _)| *endpoint == self.active_endpoint_id);
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

    /// The terminal's grid and pixel extent (`TIOCGWINSZ`); the composed
    /// size and no pixels when the terminal does not say.
    fn browser_terminal_pixels(&self, cols: u16, rows: u16) -> (u16, u16, u32, u32) {
        #[cfg(test)]
        if let Some(pixels) = self.browser_dock.pixels_for_test {
            return pixels;
        }
        match crossterm::terminal::window_size() {
            Ok(size) if size.columns > 0 && size.rows > 0 => (
                size.columns,
                size.rows,
                u32::from(size.width),
                u32::from(size.height),
            ),
            _ => (cols, rows, 0, 0),
        }
    }

    /// The `browser.placement.set` to send now, if the placement changed
    /// (at most one per 33 ms: a change inside the gap waits for the
    /// deadline). Sent beside the command lane; its answer is not waited for.
    pub(crate) fn take_browser_placement_request(
        &mut self,
        now: Instant,
    ) -> Option<crate::protocol::ClientMessage> {
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
    /// its gap, the placeholder's next spinner frame.
    pub(crate) fn next_browser_dock_deadline(&self, now: Instant) -> Option<Instant> {
        let pending = self.browser_dock.pending.then(|| {
            self.browser_dock
                .sent_at
                .map_or(now, |at| (at + PLACEMENT_GAP).max(now))
        });
        let spin = self.browser_placeholder_spins().then(|| now + SPIN_EVERY);
        match (pending, spin) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The tick: a spinning placeholder repaints; the smoke's test view
    /// opens once docking is offered.
    pub(crate) fn tick_browser_dock(&mut self, _now: Instant, outcome: &mut ClientShellInput) {
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
        if self.browser_placeholder_spins() || self.browser_dock.pending {
            outcome.repaint = true;
        }
    }
}
