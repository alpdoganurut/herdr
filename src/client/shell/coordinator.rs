//! The coordinator on the client (fork): the client-only state behind the
//! `tabs` sidebar's pinned coordinator row, its context menu, the managed-tab
//! `+` marks and the `spaces` layout's coordinator-tab mark.
//!
//! Everything shown comes from `coordinator.get`. It is pulled on the tick
//! after the first snapshot of a connection, whenever the snapshot's
//! `CoordinatorSnapshotSignature` changes (the tab set, or the coordinator
//! tab's agent status or focus: a wake landing, the coordinator finishing,
//! the user looking at it), and otherwise once a minute. Every
//! `coordinator.*` method answers with the same read model, so a reply
//! refreshes the state without a second pull. Nothing is pulled from a
//! server that does not advertise `coordinator.get`, and without a reply
//! there is no row.
//!
//! This module holds no reference to `ClientShellState`: the shell feeds it
//! the snapshot and the endpoint facts and turns what it returns
//! (`CoordinatorRequest`, `CoordinatorEffect`) into endpoint methods,
//! focus changes and toasts. That keeps the coordinator's client logic
//! testable without an endpoint.
//!
//! The row exists whenever `coordinator.get` reports the coordinator
//! enabled (with or without a coordinator tab: a left click then asks
//! `coordinator.open`, which creates it), and while disabled only while the
//! reply names a tab the snapshot still lists (so a paused coordinator still
//! shows `○ off` and can be resumed from its menu).

#[cfg(test)]
use crate::api::schema::coordinator::method;
use crate::api::schema::coordinator::{CoordinatorGetInfo, CoordinatorStateInfo};
use crate::api::schema::AgentStatus;
use crate::app::state::Palette;
use crate::protocol::ClientShellSnapshot;
use std::collections::HashSet;

/// How often the read model is pulled without a visible reason (a board
/// edit with no status change shows within this bound).
pub(crate) const COORDINATOR_REFRESH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

/// The row's label, and the coordinator tab's label on the server.
pub(crate) const COORDINATOR_ROW_LABEL: &str = "coordinator";

/// The dim mark after a managed agent's tab label in the `tabs` layout.
pub(crate) const MANAGED_TAB_MARK: &str = "+";

/// One `coordinator.*` request the client sends. The shell maps each to its
/// `Method` arm; the TUI never sets `caller_pane` (absent means the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordinatorRequest {
    Get,
    /// Ensure the coordinator tab, focus it, start the coordinator if
    /// enabled and not up.
    Open,
    /// Clear unread suggestions; with `open` the server opens the URL.
    OpenDashboard {
        open: bool,
    },
    Wake,
    Start {
        resume: bool,
    },
    SetEnabled(bool),
    /// Both caps travel together (`coordinator.set_wake_caps` requires both).
    SetWakeCaps {
        cap_hour: u32,
        cap_day: u32,
    },
    /// `None` is Claude's default model.
    SetModel(Option<String>),
    SetNotify(bool),
}

/// What a pending `coordinator.*` request was, for routing its reply
/// (`PendingEndpointKind` carries it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinatorRequestKind {
    Get,
    Open,
    OpenDashboard { open: bool },
    Wake,
    Start,
    SetEnabled,
    SetWakeCaps,
    SetModel,
    SetNotify,
}

impl CoordinatorRequest {
    /// The JSON API method name (the shell maps requests to `Method` arms
    /// in `coordinator_shell.rs`; its test checks the two agree).
    #[cfg(test)]
    pub(crate) fn method_name(&self) -> &'static str {
        match self {
            Self::Get => method::GET,
            Self::Open => method::OPEN,
            Self::OpenDashboard { .. } => method::OPEN_DASHBOARD,
            Self::Wake => method::WAKE,
            Self::Start { .. } => method::START,
            Self::SetEnabled(_) => method::SET_ENABLED,
            Self::SetWakeCaps { .. } => method::SET_WAKE_CAPS,
            Self::SetModel(_) => method::SET_MODEL,
            Self::SetNotify(_) => method::SET_NOTIFY,
        }
    }

    pub(crate) fn kind(&self) -> CoordinatorRequestKind {
        match self {
            Self::Get => CoordinatorRequestKind::Get,
            Self::Open => CoordinatorRequestKind::Open,
            Self::OpenDashboard { open } => CoordinatorRequestKind::OpenDashboard { open: *open },
            Self::Wake => CoordinatorRequestKind::Wake,
            Self::Start { .. } => CoordinatorRequestKind::Start,
            Self::SetEnabled(_) => CoordinatorRequestKind::SetEnabled,
            Self::SetWakeCaps { .. } => CoordinatorRequestKind::SetWakeCaps,
            Self::SetModel(_) => CoordinatorRequestKind::SetModel,
            Self::SetNotify(_) => CoordinatorRequestKind::SetNotify,
        }
    }

    /// `coordinator.open` focuses the coordinator tab (the shell drops a
    /// pending workspace highlight, as for `news.open`).
    #[cfg(test)]
    pub(crate) fn changes_focus(&self) -> bool {
        matches!(self, Self::Open)
    }
}

/// What the shell does after a row click, a menu item or a settings row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordinatorEffect {
    Nothing,
    Request(CoordinatorRequest),
    /// `tab.focus` on the coordinator tab.
    FocusTab(String),
    /// Open the settings overlay on the coordinator section.
    OpenSettings,
    /// Write `ui.sidebar_layout = "tabs"` through the settings path.
    SwitchLayoutToTabs,
    /// The item is disabled; show why.
    Refused(String),
}

/// The snapshot facts that decide a fresh `coordinator.get`: the
/// connection, the tab ids (the coordinator tab appearing or going), and
/// the coordinator tab's agent status (a wake landing, the coordinator
/// finishing, usually right after it writes the board) and focus (the
/// server clears unread suggestions on focus).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorSnapshotSignature {
    boot_id: String,
    tab_ids: Vec<String>,
    coordinator_tab: Option<(AgentStatus, bool)>,
}

/// Client-only coordinator state for the active endpoint.
#[derive(Debug, Default)]
pub(crate) struct ClientCoordinatorState {
    /// The last `coordinator.get` (or other `coordinator.*`) reply; `None`
    /// before one, after an error, or on a server without the methods.
    pub(crate) info: Option<CoordinatorGetInfo>,
    /// A `coordinator.get` is in flight.
    pub(crate) loading: bool,
    /// Pull on the next tick (a failed action, a settings section opening).
    pub(crate) refresh_due: bool,
    /// The snapshot facts the last pull was made for; a change pulls again.
    pub(crate) pulled_for: Option<CoordinatorSnapshotSignature>,
    /// When the last pull was sent, for the periodic refresh.
    pub(crate) last_pull: Option<std::time::Instant>,
    /// Tabs holding a managed agent (not the coordinator's own), rebuilt
    /// only when a reply arrives: the renderer's `+` lookup is O(1).
    pub(crate) managed_tabs: HashSet<String>,
}

/// The pinned row's state, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinatorRowState {
    Down,
    Blocked,
    Unavailable,
    /// The coordinator is up or coming up and waits on the user in its tab
    /// (a permission or folder-trust prompt), or its state is unknown.
    NeedsYou,
    Waking,
    Working,
    Ideas,
    Capped,
    Off,
    Idle,
}

impl CoordinatorRowState {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Down => "×",
            Self::Blocked | Self::NeedsYou => "!",
            Self::Unavailable => "–",
            Self::Waking | Self::Working => "◐",
            Self::Ideas => "●",
            Self::Capped => "◌",
            Self::Off | Self::Idle => "○",
        }
    }

    pub(crate) fn color(self, palette: &Palette) -> ratatui::style::Color {
        match self {
            Self::Down => palette.red,
            Self::Blocked | Self::NeedsYou | Self::Waking | Self::Working => palette.yellow,
            Self::Ideas => palette.accent,
            Self::Unavailable | Self::Capped | Self::Off | Self::Idle => palette.overlay0,
        }
    }
}

/// The pinned row, as drawn (and the `spaces` layout's tab mark).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorRow {
    /// The coordinator tab, while the snapshot lists it; `None` with the
    /// coordinator enabled and no tab (a click creates it).
    pub(crate) tab_id: Option<String>,
    pub(crate) state: CoordinatorRowState,
    /// `down`, `locked`, `registry`, `n/a`, `needs you`, `waking`,
    /// `working`, `N ideas`, `capped`, `off`, `N agents`.
    pub(crate) status: String,
    pub(crate) focused: bool,
}

impl CoordinatorRow {
    /// The `spaces` layout's mark after the coordinator tab's label:
    /// the row's glyph and status, e.g. `● 2 ideas`.
    pub(crate) fn tab_mark(&self) -> String {
        format!("{} {}", self.state.glyph(), self.status)
    }
}

/// The row's state and status for `info`, the coordinator tab's agent
/// status and whether that tab is focused (both read from the snapshot,
/// which is fresher than the last reply). Priority: down, blocked,
/// unavailable, a coordinator waiting on the user (blocked while starting
/// or running, or unknown while running), a live turn or a working
/// coordinator, unread suggestions
/// (not while the tab is focused: the server clears them on focus),
/// capped, off, else the managed-agent count. A state this client does not
/// know reads as the idle row.
pub(crate) fn coordinator_row_state(
    info: &CoordinatorGetInfo,
    tab_status: Option<AgentStatus>,
    tab_focused: bool,
) -> (CoordinatorRowState, String) {
    match info.state {
        CoordinatorStateInfo::Down => return (CoordinatorRowState::Down, "down".into()),
        CoordinatorStateInfo::Blocked => {
            return match info.blocked_reason.as_deref() {
                Some("locked_elsewhere") => (CoordinatorRowState::Blocked, "locked".into()),
                Some("registry_corrupt") => (CoordinatorRowState::Blocked, "registry".into()),
                Some("unavailable") => (CoordinatorRowState::Unavailable, "n/a".into()),
                _ => (CoordinatorRowState::Blocked, "blocked".into()),
            };
        }
        CoordinatorStateInfo::Unavailable => {
            return (CoordinatorRowState::Unavailable, "n/a".into());
        }
        _ => {}
    }
    let status = tab_status.or_else(|| {
        Some(match info.coordinator_status.as_deref()? {
            "blocked" => AgentStatus::Blocked,
            "unknown" => AgentStatus::Unknown,
            "working" => AgentStatus::Working,
            _ => return None,
        })
    });
    // Unknown is also the moment before the agent paints at launch: only a
    // running coordinator reads it as needing the user.
    let needs_you = match info.state {
        CoordinatorStateInfo::Starting => status == Some(AgentStatus::Blocked),
        CoordinatorStateInfo::Running => {
            matches!(status, Some(AgentStatus::Blocked | AgentStatus::Unknown))
        }
        _ => false,
    };
    if needs_you {
        return (CoordinatorRowState::NeedsYou, "needs you".into());
    }
    if status == Some(AgentStatus::Working) {
        return (CoordinatorRowState::Working, "working".into());
    }
    if info.turn.is_some() {
        return (CoordinatorRowState::Waking, "waking".into());
    }
    if info.unread_suggestions > 0 && !tab_focused {
        let n = info.unread_suggestions;
        let noun = if n == 1 { "idea" } else { "ideas" };
        return (CoordinatorRowState::Ideas, format!("{n} {noun}"));
    }
    if info.wake.capped {
        return (CoordinatorRowState::Capped, "capped".into());
    }
    if !info.enabled || info.state == CoordinatorStateInfo::Off {
        return (CoordinatorRowState::Off, "off".into());
    }
    let n = info.managed.len();
    let noun = if n == 1 { "agent" } else { "agents" };
    (CoordinatorRowState::Idle, format!("{n} {noun}"))
}

/// The row's context menu items, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinatorMenuAction {
    OpenDashboard,
    Focus,
    WakeNow,
    Restart,
    /// Pause when enabled, resume when disabled.
    ToggleEnabled,
    Settings,
}

/// One context menu item: its label and, when disabled, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorMenuItem {
    pub(crate) action: CoordinatorMenuAction,
    pub(crate) label: &'static str,
    pub(crate) disabled_reason: Option<String>,
}

/// Why the dashboard cannot be opened, or `None` when it can.
pub(crate) fn dashboard_unavailable_reason(info: &CoordinatorGetInfo) -> Option<String> {
    if info.dashboard_url.is_some() {
        return None;
    }
    if !info.enabled {
        return Some("coordinator off".into());
    }
    Some(match info.dashboard_error.as_deref() {
        Some(error) => format!("dashboard unavailable: {error}"),
        None => "dashboard unavailable".into(),
    })
}

impl ClientCoordinatorState {
    /// The coordinator tab id the last reply named, if any.
    pub(crate) fn tab_id(&self) -> Option<&str> {
        self.info.as_ref()?.tab_id.as_deref()
    }

    /// Whether `tab_id` is the coordinator tab: exempt from idle reminders
    /// and from keyboard numbering. An O(1) compare.
    pub(crate) fn is_coordinator_tab(&self, tab_id: &str) -> bool {
        self.tab_id() == Some(tab_id)
    }

    /// Whether `tab_id` holds a managed agent (the `+` mark; the renderer
    /// reads `managed_tabs` directly).
    #[cfg(test)]
    pub(crate) fn is_managed_tab(&self, tab_id: &str) -> bool {
        self.managed_tabs.contains(tab_id)
    }

    /// The row's tab id while the snapshot lists it: the id the `tabs`
    /// sidebar leaves out of its list (it is pinned).
    pub(crate) fn pinned_tab_id<'a>(&'a self, snapshot: &ClientShellSnapshot) -> Option<&'a str> {
        let tab_id = self.tab_id()?;
        snapshot
            .tabs
            .iter()
            .any(|tab| tab.tab_id == tab_id)
            .then_some(tab_id)
    }

    /// The pinned row: whenever the last reply reports the coordinator
    /// enabled (with its tab while the snapshot lists it), else only while
    /// it named a tab the snapshot still lists.
    pub(crate) fn row(&self, snapshot: &ClientShellSnapshot) -> Option<CoordinatorRow> {
        let info = self.info.as_ref()?;
        let tab = info
            .tab_id
            .as_deref()
            .and_then(|tab_id| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id));
        if tab.is_none() && !info.enabled {
            return None;
        }
        let focused = tab.is_some_and(|tab| tab.focused);
        let (state, status) = coordinator_row_state(info, tab.map(|tab| tab.agent_status), focused);
        Some(CoordinatorRow {
            tab_id: tab.map(|tab| tab.tab_id.clone()),
            state,
            status,
            focused,
        })
    }

    /// A left click on the row: focus the coordinator tab, or without one
    /// create and focus it (`coordinator.open`).
    pub(crate) fn activate_row(&self, snapshot: &ClientShellSnapshot) -> CoordinatorEffect {
        match self.row(snapshot) {
            Some(CoordinatorRow {
                tab_id: Some(tab_id),
                ..
            }) => CoordinatorEffect::FocusTab(tab_id),
            Some(_) => CoordinatorEffect::Request(CoordinatorRequest::Open),
            None => CoordinatorEffect::Nothing,
        }
    }

    /// The row's context menu: Open dashboard, Focus coordinator, Wake now,
    /// Restart coordinator, Pause/Resume coordinator, Settings. Empty
    /// without a reply.
    pub(crate) fn menu_items(&self) -> Vec<CoordinatorMenuItem> {
        let Some(info) = self.info.as_ref() else {
            return Vec::new();
        };
        let off = (!info.enabled).then(|| "coordinator off".to_owned());
        vec![
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::OpenDashboard,
                label: "Open dashboard",
                disabled_reason: dashboard_unavailable_reason(info),
            },
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::Focus,
                label: "Focus coordinator",
                disabled_reason: None,
            },
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::WakeNow,
                label: "Wake now",
                disabled_reason: off.clone(),
            },
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::Restart,
                label: "Restart coordinator",
                disabled_reason: off,
            },
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::ToggleEnabled,
                label: if info.enabled {
                    "Pause coordinator"
                } else {
                    "Resume coordinator"
                },
                disabled_reason: None,
            },
            CoordinatorMenuItem {
                action: CoordinatorMenuAction::Settings,
                label: "Settings",
                disabled_reason: None,
            },
        ]
    }

    /// A menu item (or the matching settings row). `remote` is true on a
    /// connection whose server is not on this machine (`ClientEndpointId::Ssh`):
    /// there the dashboard's `127.0.0.1` URL is not reachable and a
    /// server-side open would land on the wrong machine, so the client asks
    /// with `open: false` and shows the URL from the reply instead.
    pub(crate) fn activate_menu(
        &self,
        action: CoordinatorMenuAction,
        snapshot: &ClientShellSnapshot,
        remote: bool,
    ) -> CoordinatorEffect {
        let Some(info) = self.info.as_ref() else {
            return CoordinatorEffect::Nothing;
        };
        match action {
            CoordinatorMenuAction::OpenDashboard => match dashboard_unavailable_reason(info) {
                Some(reason) => CoordinatorEffect::Refused(reason),
                None => {
                    CoordinatorEffect::Request(CoordinatorRequest::OpenDashboard { open: !remote })
                }
            },
            CoordinatorMenuAction::Focus => match self.pinned_tab_id(snapshot) {
                Some(tab_id) => CoordinatorEffect::FocusTab(tab_id.to_owned()),
                None => CoordinatorEffect::Request(CoordinatorRequest::Open),
            },
            CoordinatorMenuAction::WakeNow if !info.enabled => {
                CoordinatorEffect::Refused("coordinator off".into())
            }
            CoordinatorMenuAction::WakeNow => CoordinatorEffect::Request(CoordinatorRequest::Wake),
            CoordinatorMenuAction::Restart if !info.enabled => {
                CoordinatorEffect::Refused("coordinator off".into())
            }
            CoordinatorMenuAction::Restart => {
                CoordinatorEffect::Request(CoordinatorRequest::Start { resume: true })
            }
            CoordinatorMenuAction::ToggleEnabled => {
                CoordinatorEffect::Request(CoordinatorRequest::SetEnabled(!info.enabled))
            }
            CoordinatorMenuAction::Settings => CoordinatorEffect::OpenSettings,
        }
    }

    /// The coordinator tab's agent status and focus in `snapshot`.
    fn coordinator_tab_facts(&self, snapshot: &ClientShellSnapshot) -> Option<(AgentStatus, bool)> {
        let tab_id = self.tab_id()?;
        snapshot
            .tabs
            .iter()
            .find(|tab| tab.tab_id == tab_id)
            .map(|tab| (tab.agent_status, tab.focused))
    }

    /// The snapshot facts the pull follows.
    pub(crate) fn signature(&self, snapshot: &ClientShellSnapshot) -> CoordinatorSnapshotSignature {
        CoordinatorSnapshotSignature {
            boot_id: snapshot.boot_id.clone(),
            tab_ids: snapshot.tabs.iter().map(|tab| tab.tab_id.clone()).collect(),
            coordinator_tab: self.coordinator_tab_facts(snapshot),
        }
    }

    /// Whether `snapshot` still has the facts of the last pull, compared in
    /// place: the tick runs at the client's timer rate, so it allocates
    /// only when a pull is actually sent.
    fn pulled_for_matches(&self, snapshot: &ClientShellSnapshot) -> bool {
        let Some(pulled) = self.pulled_for.as_ref() else {
            return false;
        };
        pulled.boot_id == snapshot.boot_id
            && pulled.tab_ids.len() == snapshot.tabs.len()
            && pulled
                .tab_ids
                .iter()
                .zip(&snapshot.tabs)
                .all(|(pulled, tab)| *pulled == tab.tab_id)
            && pulled.coordinator_tab == self.coordinator_tab_facts(snapshot)
    }

    /// The tick: `Some(Get)` when a pull is due and may be sent (the server
    /// advertises `coordinator.get` and the endpoint is online). The
    /// returned flag asks for a repaint (a new connection dropped the old
    /// server's state). A pull that is due but cannot be sent is still
    /// remembered, so an unadvertised server is not asked every tick.
    pub(crate) fn tick(
        &mut self,
        snapshot: &ClientShellSnapshot,
        advertised: bool,
        online: bool,
        now: std::time::Instant,
    ) -> (bool, Option<CoordinatorRequest>) {
        let mut repaint = false;
        if self
            .pulled_for
            .as_ref()
            .is_some_and(|pulled| pulled.boot_id != snapshot.boot_id)
        {
            // Another connection: nothing of the old server's state applies.
            repaint = self.info.is_some();
            *self = Self::default();
        }
        let stale = !self.pulled_for_matches(snapshot);
        let periodic = self
            .last_pull
            .is_some_and(|last| now.duration_since(last) >= COORDINATOR_REFRESH_INTERVAL);
        if !(stale || self.refresh_due || periodic) || self.loading {
            return (repaint, None);
        }
        self.refresh_due = false;
        self.pulled_for = Some(self.signature(snapshot));
        self.last_pull = Some(now);
        if !advertised || !online {
            return (repaint, None);
        }
        self.loading = true;
        (repaint, Some(CoordinatorRequest::Get))
    }

    /// The shell could not send the `Get` the tick returned.
    pub(crate) fn get_not_sent(&mut self) {
        self.loading = false;
    }

    /// Ask for a fresh `coordinator.get` on the next tick.
    pub(crate) fn refresh(&mut self) {
        self.refresh_due = true;
    }

    /// A `coordinator.*` reply: `Some(info)` for a `CoordinatorGet` result,
    /// `None` for an error (already raised as the generic notice) or an
    /// unexpected result. Returns a notice to show: the URL for an
    /// `open_dashboard {open: false}` reply, or why a wake waits.
    pub(crate) fn on_reply(
        &mut self,
        kind: CoordinatorRequestKind,
        reply: Option<CoordinatorGetInfo>,
        snapshot: Option<&ClientShellSnapshot>,
    ) -> Option<String> {
        match kind {
            CoordinatorRequestKind::Get => {
                self.loading = false;
                self.set_info(reply);
                // The reply names the coordinator tab, which the signature
                // then tracks: seat it on the current snapshot so learning
                // the tab is not itself a change.
                self.pulled_for = snapshot.map(|snapshot| self.signature(snapshot));
                None
            }
            _ => {
                let Some(mut info) = reply else {
                    // A refusal leaves the state as it was; pull to be sure.
                    self.refresh();
                    return None;
                };
                // Only this reply's: the stored read model does not keep it.
                let queued = info.wake_queued.take();
                let notice = match kind {
                    CoordinatorRequestKind::OpenDashboard { open: false } => {
                        info.dashboard_url.as_deref().map(dashboard_url_notice)
                    }
                    CoordinatorRequestKind::Wake => queued.as_deref().map(wake_queued_notice),
                    _ => None,
                };
                self.set_info(Some(info));
                notice
            }
        }
    }

    fn set_info(&mut self, info: Option<CoordinatorGetInfo>) {
        self.managed_tabs.clear();
        if let Some(info) = info.as_ref() {
            let own = info.tab_id.as_deref();
            self.managed_tabs.extend(
                info.managed
                    .iter()
                    .filter_map(|agent| agent.tab_id.as_deref())
                    .filter(|tab_id| Some(*tab_id) != own)
                    .map(str::to_owned),
            );
        }
        self.info = info;
    }
}

/// The text a remote connection shows for the dashboard URL.
pub(crate) fn dashboard_url_notice(url: &str) -> String {
    format!("coordinator dashboard on the server host: {url}")
}

/// The text a wake that cannot be delivered now shows.
pub(crate) fn wake_queued_notice(reason: &str) -> String {
    format!("wake queued: {reason}")
}
