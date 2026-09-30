//! The herdr browser on the client (fork): the client-only state behind the
//! `tabs` sidebar's pinned Browser row, the `◎` glyph on tab rows whose panes
//! use the browser, and the Browser overlay.
//!
//! Everything shown comes from `browser.get {since_seq}`, pulled on the first
//! snapshot of a connection, after every `browser.*` reply the client sent,
//! and on a timer: every 2 s while a profile is running or starting, every
//! 15 s otherwise. Nothing is pulled from a server that does not advertise
//! `browser.get`. A remote endpoint is rendered read-only (no `focus`).

use std::collections::HashSet;

use super::*;
use crate::api::schema::{
    BrowserGetInfo, BrowserGetParams, BrowserProfileTarget, BrowserStopParams, BrowserTabTarget,
    Method,
};

/// The pull cadence while a profile is running or starting.
pub(super) const BROWSER_REFRESH_ACTIVE: std::time::Duration = std::time::Duration::from_secs(2);
/// The pull cadence otherwise.
pub(super) const BROWSER_REFRESH_IDLE: std::time::Duration = std::time::Duration::from_secs(15);
/// The row's label.
pub(crate) const BROWSER_ROW_LABEL: &str = "Browser";
/// The marker of a herdr tab whose pane uses the browser.
pub(crate) const TAB_BROWSER_MARKER: &str = "\u{25CE}"; // ◎

/// Client-only browser state for the active endpoint.
#[derive(Debug, Default)]
pub(crate) struct ClientBrowserState {
    /// The last `browser.get` reply; `None` before one, or after an error.
    pub(super) info: Option<BrowserGetInfo>,
    /// A `browser.get` is in flight.
    pub(super) loading: bool,
    /// Pull on the next tick (a `browser.*` reply, the overlay opening).
    pub(super) refresh_due: bool,
    /// The connection the last pull was made for.
    pub(super) pulled_for: Option<String>,
    /// When the last pull was sent, for the periodic refresh.
    pub(super) last_pull: Option<std::time::Instant>,
    /// A profile was seen running in this connection: the row stays even
    /// after every profile stopped.
    pub(super) seen_running: bool,
}

/// The pinned row's state, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserRowState {
    Crashed,
    Dialog,
    Starting,
    Running,
    Stopped,
}

impl BrowserRowState {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Crashed => "×",
            Self::Dialog => "◉",
            Self::Starting => "◐",
            Self::Running => TAB_BROWSER_MARKER,
            Self::Stopped => "◌",
        }
    }

    pub(crate) fn color(self, palette: &Palette, active: bool) -> ratatui::style::Color {
        match self {
            Self::Crashed | Self::Dialog => palette.red,
            Self::Starting => palette.yellow,
            Self::Running if active => palette.accent,
            Self::Running => palette.overlay1,
            Self::Stopped => palette.overlay0,
        }
    }
}

/// The pinned row, as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserRow {
    pub(crate) state: BrowserRowState,
    /// `3 tabs · 2 agents`, `starting`, `stopped`, `crashed`, `dialog open`.
    pub(crate) status: String,
    /// An agent touched a tab within `active_seconds`: the glyph is lit.
    pub(crate) active: bool,
    /// The profile the row's menu acts on (a running one, else the first).
    pub(crate) profile: String,
    pub(crate) running: bool,
}

/// The row's state and status text for `info`.
pub(crate) fn browser_row_state(info: &BrowserGetInfo) -> (BrowserRowState, String) {
    if let Some(crashed) = info.profiles.iter().find(|p| p.state == "crashed") {
        let detail = crashed
            .detail
            .as_deref()
            .filter(|d| !d.is_empty())
            .map(|_| "crashed".to_string())
            .unwrap_or_else(|| "crashed".into());
        return (BrowserRowState::Crashed, detail);
    }
    if info.tabs.iter().any(|tab| tab.dialog_open) {
        return (BrowserRowState::Dialog, "dialog open".into());
    }
    if info.profiles.iter().any(|p| p.state == "starting") {
        return (BrowserRowState::Starting, "starting".into());
    }
    let running: Vec<_> = info
        .profiles
        .iter()
        .filter(|p| p.state == "running")
        .collect();
    if !running.is_empty() {
        let tabs: u32 = running.iter().map(|p| p.tabs).sum();
        let agents: u32 = running.iter().map(|p| p.agents).sum();
        let status = match (tabs, agents) {
            (t, 0) => format!("{t} {}", if t == 1 { "tab" } else { "tabs" }),
            (t, a) => format!(
                "{t} {} · {a} {}",
                if t == 1 { "tab" } else { "tabs" },
                if a == 1 { "agent" } else { "agents" }
            ),
        };
        return (BrowserRowState::Running, status);
    }
    let status = match info.profiles.iter().map(|p| p.state.as_str()).next() {
        Some("user_quit") => "quit",
        Some("in_use") => "in use",
        _ => "stopped",
    };
    (BrowserRowState::Stopped, status.into())
}

/// The browser overlay: what it lists and where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientBrowserOverlay {
    pub(crate) rows: Vec<BrowserOverlayRow>,
    pub(crate) cursor: usize,
    pub(crate) scroll: usize,
    /// The endpoint is local: Enter focuses the window, `s` starts and stops.
    pub(crate) local: bool,
    /// `browser.get` not answered yet.
    pub(crate) loading: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserOverlayRow {
    Profile {
        name: String,
        state: String,
        running: bool,
    },
    Tab {
        profile: String,
        short: String,
        title: String,
        url: String,
        opener: String,
        /// `planner · claude read 12s`, `you 3m`, or empty.
        last: String,
        dialog: bool,
        active: bool,
    },
    Empty(String),
}

impl BrowserOverlayRow {
    fn selectable(&self) -> bool {
        matches!(self, Self::Tab { .. } | Self::Profile { .. })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// `12s`, `3m`, `2h`, `4d`.
pub(super) fn age(then: u64, now: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The overlay's rows for `info`: a header per profile, its open tabs
/// newest activity first.
pub(crate) fn browser_overlay_rows(info: &BrowserGetInfo, now: u64) -> Vec<BrowserOverlayRow> {
    let mut rows = Vec::new();
    let mut profiles: Vec<&str> = info.profiles.iter().map(|p| p.name.as_str()).collect();
    for tab in &info.tabs {
        if !profiles.contains(&tab.profile.as_str()) {
            profiles.push(tab.profile.as_str());
        }
    }
    if profiles.is_empty() {
        rows.push(BrowserOverlayRow::Empty(
            "no browser yet: an agent's first `browser open` starts it, or `s` here".into(),
        ));
        return rows;
    }
    for profile in profiles {
        let state = info
            .profiles
            .iter()
            .find(|p| p.name == profile)
            .map(|p| p.state.clone())
            .unwrap_or_else(|| "stopped".into());
        let running = state == "running";
        rows.push(BrowserOverlayRow::Profile {
            name: profile.to_string(),
            state: state.clone(),
            running,
        });
        let mut tabs: Vec<_> = info
            .tabs
            .iter()
            .filter(|tab| tab.profile == profile)
            .collect();
        tabs.sort_by(|a, b| {
            let at = |tab: &crate::api::schema::BrowserTabInfo| {
                tab.last.as_ref().map(|touch| touch.at).unwrap_or(0)
            };
            at(b).cmp(&at(a)).then_with(|| a.id.cmp(&b.id))
        });
        if tabs.is_empty() {
            rows.push(BrowserOverlayRow::Empty(if running {
                "  no tabs".into()
            } else {
                "  (not running)".into()
            }));
        }
        for tab in tabs {
            let short = tab
                .id
                .rsplit_once(':')
                .map(|(_, short)| short.to_string())
                .unwrap_or_else(|| tab.id.clone());
            let last = match &tab.last {
                Some(touch) => format!(
                    "{} {} {}",
                    touch.actor.label(),
                    touch.op,
                    age(touch.at, now)
                ),
                None if tab.last_actor.is_user() => "you".into(),
                None => String::new(),
            };
            rows.push(BrowserOverlayRow::Tab {
                profile: profile.to_string(),
                short,
                title: tab.title.clone(),
                url: tab.url.clone(),
                opener: tab.opened_by.label(),
                last,
                dialog: tab.dialog_open,
                active: tab.active,
            });
        }
    }
    rows
}

impl ClientShellState {
    /// The pinned row: whenever the feature is enabled, or a profile has been
    /// seen (running now, or earlier in this connection).
    pub(crate) fn browser_row(&self) -> Option<BrowserRow> {
        let info = self.browser.info.as_ref()?;
        self.snapshot.as_deref()?;
        if !info.enabled && info.profiles.is_empty() && !self.browser.seen_running {
            return None;
        }
        let (state, status) = browser_row_state(info);
        let active = info.tabs.iter().any(|tab| tab.active);
        let running = info
            .profiles
            .iter()
            .find(|p| p.state == "running" || p.state == "starting");
        let profile = running
            .map(|p| p.name.clone())
            .or_else(|| info.profiles.first().map(|p| p.name.clone()))
            .unwrap_or_else(|| "main".into());
        Some(BrowserRow {
            state,
            status,
            active,
            profile,
            running: running.is_some(),
        })
    }

    /// The herdr tabs whose panes used the browser within the glyph window:
    /// each pane's cursor clock and each browser tab's last touch.
    pub(crate) fn browser_marked_tabs(&self) -> HashSet<String> {
        let mut tabs = HashSet::new();
        let Some(info) = self.browser.info.as_ref() else {
            return tabs;
        };
        let window = self.config.browser_active_glyph_secs;
        if window == 0 {
            return tabs;
        }
        let now = unix_now();
        let pane_tab = |pane_id: &str| -> Option<String> {
            self.snapshot.as_deref().and_then(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .find(|pane| pane.pane_id == pane_id)
                    .map(|pane| pane.tab_id.clone())
            })
        };
        for cursor in &info.recent_panes {
            if now.saturating_sub(cursor.last_at) > window {
                continue;
            }
            if let Some(tab) = cursor.tab_id.clone().or_else(|| pane_tab(&cursor.pane_id)) {
                tabs.insert(tab);
            }
        }
        for tab in &info.tabs {
            let Some(touch) = tab.last.as_ref() else {
                continue;
            };
            if now.saturating_sub(touch.at) > window {
                continue;
            }
            if let Some(tab_id) = touch
                .actor
                .tab_id()
                .map(str::to_string)
                .or_else(|| touch.actor.pane_id().and_then(pane_tab))
            {
                tabs.insert(tab_id);
            }
        }
        tabs
    }

    fn browser_boot_id(&self) -> Option<String> {
        self.snapshot
            .as_deref()
            .map(|snapshot| snapshot.boot_id.clone())
    }

    fn browser_refresh_interval(&self) -> std::time::Duration {
        let busy = self.browser.info.as_ref().is_some_and(|info| {
            info.profiles
                .iter()
                .any(|p| p.state == "running" || p.state == "starting")
        });
        let overlay_open = matches!(self.overlay, Some(ClientShellOverlay::Browser(_)));
        if busy || overlay_open {
            BROWSER_REFRESH_ACTIVE
        } else {
            BROWSER_REFRESH_IDLE
        }
    }

    /// The tick: pull `browser.get` when due.
    pub(crate) fn tick_browser(&mut self, now: std::time::Instant, outcome: &mut ClientShellInput) {
        let Some(boot_id) = self.browser_boot_id() else {
            return;
        };
        if self
            .browser
            .pulled_for
            .as_ref()
            .is_some_and(|pulled| pulled != &boot_id)
        {
            // Another connection: nothing of the old server's browser applies.
            self.browser = ClientBrowserState::default();
            outcome.repaint = true;
        }
        let first = self.browser.pulled_for.is_none();
        let periodic = self
            .browser
            .last_pull
            .is_some_and(|last| now.duration_since(last) >= self.browser_refresh_interval());
        if (first || self.browser.refresh_due || periodic) && !self.browser.loading {
            self.queue_browser_get(boot_id, now, outcome);
        }
    }

    /// When the tick loop must wake for the browser: the next periodic pull.
    pub(crate) fn next_browser_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        if self.browser.pulled_for.is_none() && self.snapshot.is_none() {
            return None;
        }
        let method = Method::BrowserGet(BrowserGetParams::default());
        if !self.supports_endpoint_method(&method) {
            return None;
        }
        let last = self.browser.last_pull?;
        Some((last + self.browser_refresh_interval()).max(now))
    }

    /// `browser.get {since_seq}`, remembered for `boot_id`; silently skipped
    /// on an endpoint that does not advertise it or is offline.
    fn queue_browser_get(
        &mut self,
        boot_id: String,
        now: std::time::Instant,
        outcome: &mut ClientShellInput,
    ) {
        self.browser.refresh_due = false;
        self.browser.pulled_for = Some(boot_id);
        self.browser.last_pull = Some(now);
        let since_seq = self.browser.info.as_ref().map(|info| info.seq);
        let method = Method::BrowserGet(BrowserGetParams { since_seq });
        if !self.supports_endpoint_method(&method)
            || !self.endpoint_is_online(&self.active_endpoint_id)
        {
            return;
        }
        if self.push_endpoint_method_with_kind(method, PendingEndpointKind::BrowserGet, outcome) {
            self.browser.loading = true;
        }
    }

    /// Ask for a fresh `browser.get` on the next tick.
    pub(super) fn refresh_browser(&mut self) {
        self.browser.refresh_due = true;
    }

    /// A left click on the row opens the overlay.
    pub(super) fn activate_browser_row(&mut self, outcome: &mut ClientShellInput) {
        if self.browser_row().is_none() {
            return;
        }
        self.open_browser_overlay(outcome);
    }

    /// The Browser overlay, filled from the last `browser.get`; a fresh pull
    /// follows at once.
    pub(crate) fn open_browser_overlay(&mut self, outcome: &mut ClientShellInput) {
        let rows = self
            .browser
            .info
            .as_ref()
            .map(|info| browser_overlay_rows(info, unix_now()))
            .unwrap_or_default();
        let cursor = rows
            .iter()
            .position(BrowserOverlayRow::selectable)
            .unwrap_or(0);
        self.overlay = Some(ClientShellOverlay::Browser(ClientBrowserOverlay {
            rows,
            cursor,
            scroll: 0,
            local: self.active_endpoint_id.is_local(),
            loading: self.browser.info.is_none(),
        }));
        outcome.repaint = true;
        self.refresh_browser();
    }

    /// Re-list the overlay after a `browser.get`, keeping the cursor on the
    /// same tab when it is still listed.
    fn sync_browser_overlay(&mut self) {
        let Some(info) = self.browser.info.as_ref() else {
            return;
        };
        let rows = browser_overlay_rows(info, unix_now());
        if let Some(ClientShellOverlay::Browser(overlay)) = self.overlay.as_mut() {
            let selected = overlay.rows.get(overlay.cursor).cloned();
            let cursor = selected
                .and_then(|selected| match selected {
                    BrowserOverlayRow::Tab { profile, short, .. } => rows.iter().position(|row| {
                        matches!(row, BrowserOverlayRow::Tab { profile: p, short: s, .. } if *p == profile && *s == short)
                    }),
                    BrowserOverlayRow::Profile { name, .. } => rows.iter().position(|row| {
                        matches!(row, BrowserOverlayRow::Profile { name: n, .. } if *n == name)
                    }),
                    BrowserOverlayRow::Empty(_) => None,
                })
                .or_else(|| rows.iter().position(BrowserOverlayRow::selectable))
                .unwrap_or(0);
            overlay.rows = rows;
            overlay.cursor = cursor;
            overlay.loading = false;
        }
    }

    /// Keys while the Browser overlay is open. `true` when consumed.
    pub(super) fn route_browser_overlay_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::KeyCode;
        let Some(ClientShellOverlay::Browser(overlay)) = self.overlay.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = None;
                outcome.repaint = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(next) = overlay
                    .rows
                    .iter()
                    .enumerate()
                    .skip(overlay.cursor + 1)
                    .find(|(_, row)| row.selectable())
                    .map(|(index, _)| index)
                {
                    overlay.cursor = next;
                    outcome.repaint = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(previous) = overlay
                    .rows
                    .iter()
                    .enumerate()
                    .take(overlay.cursor)
                    .rev()
                    .find(|(_, row)| row.selectable())
                    .map(|(index, _)| index)
                {
                    overlay.cursor = previous;
                    outcome.repaint = true;
                }
            }
            KeyCode::Enter => {
                if !overlay.local {
                    return true;
                }
                if let Some(BrowserOverlayRow::Tab { profile, short, .. }) =
                    overlay.rows.get(overlay.cursor).cloned()
                {
                    self.push_endpoint_method_with_kind(
                        Method::BrowserFocus(BrowserTabTarget {
                            profile: Some(profile),
                            tab: short,
                        }),
                        PendingEndpointKind::BrowserFocus,
                        outcome,
                    );
                }
            }
            KeyCode::Char('s') => {
                if !overlay.local {
                    return true;
                }
                let target = match overlay.rows.get(overlay.cursor).cloned() {
                    Some(BrowserOverlayRow::Profile { name, running, .. }) => Some((name, running)),
                    Some(BrowserOverlayRow::Tab { profile, .. }) => {
                        let running = overlay.rows.iter().any(|row| {
                            matches!(row, BrowserOverlayRow::Profile { name, running: true, .. } if *name == profile)
                        });
                        Some((profile, running))
                    }
                    _ => self.browser_row().map(|row| (row.profile, row.running)),
                };
                if let Some((profile, running)) = target {
                    self.toggle_browser_profile(&profile, running, outcome);
                }
            }
            _ => return false,
        }
        true
    }

    /// `browser.stop` for a running profile, `browser.start` otherwise.
    fn toggle_browser_profile(
        &mut self,
        profile: &str,
        running: bool,
        outcome: &mut ClientShellInput,
    ) {
        if running {
            self.push_endpoint_method_with_kind(
                Method::BrowserStop(BrowserStopParams {
                    profile: Some(profile.to_string()),
                    all: false,
                }),
                PendingEndpointKind::BrowserStop,
                outcome,
            );
        } else {
            self.push_endpoint_method_with_kind(
                Method::BrowserStart(BrowserProfileTarget {
                    profile: Some(profile.to_string()),
                }),
                PendingEndpointKind::BrowserStart,
                outcome,
            );
        }
    }

    /// Right-click on the row: Focus window, Start/Stop profile, Open overlay.
    pub(super) fn open_browser_context_menu(&mut self, x: u16, y: u16) {
        let Some(row) = self.browser_row() else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Browser {
                profile: row.profile,
                running: row.running,
                local: self.active_endpoint_id.is_local(),
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    /// The Browser row menu's items.
    pub(super) fn activate_browser_context_action(
        &mut self,
        profile: String,
        running: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::BrowserFocusWindow => {
                // The selected tab of the profile, else its first; the server
                // raises the window either way.
                let tab = self.browser.info.as_ref().and_then(|info| {
                    let mut tabs = info.tabs.iter().filter(|tab| tab.profile == profile);
                    tabs.clone()
                        .find(|tab| tab.selected)
                        .or_else(|| tabs.next())
                        .and_then(|tab| tab.id.rsplit_once(':').map(|(_, s)| s.to_string()))
                });
                if let Some(tab) = tab {
                    self.push_endpoint_method_with_kind(
                        Method::BrowserFocus(BrowserTabTarget {
                            profile: Some(profile),
                            tab,
                        }),
                        PendingEndpointKind::BrowserFocus,
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::BrowserToggleProfile => {
                self.toggle_browser_profile(&profile, running, outcome);
            }
            ClientContextMenuAction::BrowserOpenOverlay => {
                self.open_browser_overlay(outcome);
            }
            _ => {}
        }
    }

    /// `browser.*` replies: `browser.get` fills the state (an `unchanged`
    /// answer keeps the last record); the others trigger a fresh pull.
    pub(super) fn handle_browser_endpoint_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        match kind {
            PendingEndpointKind::BrowserGet => {
                self.browser.loading = false;
                match result {
                    Ok(crate::api::schema::ResponseResult::BrowserGet { browser }) => {
                        if browser.unchanged {
                            if let Some(info) = self.browser.info.as_mut() {
                                info.enabled = browser.enabled;
                            }
                        } else {
                            if browser
                                .profiles
                                .iter()
                                .any(|p| p.state == "running" || p.state == "starting")
                            {
                                self.browser.seen_running = true;
                            }
                            self.browser.info = Some(browser);
                        }
                    }
                    Ok(_) => {
                        self.set_endpoint_error("endpoint returned an unexpected browser result");
                        self.browser.info = None;
                    }
                    Err(_) => self.browser.info = None,
                }
                self.sync_browser_overlay();
                (true, Vec::new())
            }
            _ => {
                if let Ok(crate::api::schema::ResponseResult::BrowserGet { browser }) = result {
                    if !browser.unchanged {
                        self.browser.info = Some(browser);
                        self.sync_browser_overlay();
                    }
                }
                self.refresh_browser();
                (true, Vec::new())
            }
        }
    }
}
