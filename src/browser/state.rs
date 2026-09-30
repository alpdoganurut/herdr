//! The browser ledger: which profiles run, which tabs exist, who opened and
//! last touched each one, each pane's current tab, and the activity log.
//! Pure data and reducers, no I/O; the hub drives it and `persist/browser.rs`
//! stores the durable part.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::api::schema::{
    BrowserActivity, BrowserActor, BrowserGetInfo, BrowserHostInfo, BrowserPaneCursor,
    BrowserProfileInfo, BrowserTabInfo, BrowserTouch,
};

/// Activity entries kept in memory (and in `browser.json`).
pub const LOG_CAPACITY: usize = 500;
/// How long a closed tab's record is kept for the log and for `tab_closed`.
pub const CLOSED_TAB_RETENTION_SECS: u64 = 600;
/// Most pane ids remembered per tab.
pub const MAX_TAB_USERS: usize = 8;
/// A crashed profile shows as crashed for this long, then as stopped.
pub const CRASH_RETENTION_SECS: u64 = 600;
/// Longest `detail` string in a touch or activity.
pub const MAX_DETAIL_CHARS: usize = 220;

/// A tab is identified by its profile and Chromium target id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TabKey {
    pub profile: String,
    pub target_id: String,
}

impl TabKey {
    pub fn new(profile: &str, target_id: &str) -> Self {
        Self {
            profile: profile.into(),
            target_id: target_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileStatus {
    Stopped,
    Starting {
        since: u64,
    },
    Running {
        pid: u32,
        port: u16,
        since: u64,
        exe: String,
    },
    Crashed {
        at: u64,
        detail: String,
    },
    UserQuit {
        at: u64,
    },
    InUse {
        pid: Option<u32>,
        at: u64,
    },
}

impl ProfileStatus {
    pub fn name(&self) -> &'static str {
        match self {
            ProfileStatus::Stopped => "stopped",
            ProfileStatus::Starting { .. } => "starting",
            ProfileStatus::Running { .. } => "running",
            ProfileStatus::Crashed { .. } => "crashed",
            ProfileStatus::UserQuit { .. } => "user_quit",
            ProfileStatus::InUse { .. } => "in_use",
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, ProfileStatus::Running { .. })
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            ProfileStatus::Running { pid, .. } => Some(*pid),
            ProfileStatus::InUse { pid, .. } => *pid,
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HostStatus {
    #[default]
    Absent,
    Starting,
    Running {
        pid: u32,
        playwright: String,
        node: String,
    },
    Failed {
        error: String,
        log_tail: String,
    },
}

impl HostStatus {
    pub fn info(&self) -> BrowserHostInfo {
        match self {
            HostStatus::Absent => BrowserHostInfo {
                state: "absent".into(),
                ..Default::default()
            },
            HostStatus::Starting => BrowserHostInfo {
                state: "starting".into(),
                ..Default::default()
            },
            HostStatus::Running {
                pid,
                playwright,
                node,
            } => BrowserHostInfo {
                state: "running".into(),
                pid: Some(*pid),
                playwright: Some(playwright.clone()),
                node: Some(node.clone()),
                error: None,
            },
            HostStatus::Failed { error, log_tail } => BrowserHostInfo {
                state: "failed".into(),
                error: Some(if log_tail.is_empty() {
                    error.clone()
                } else {
                    format!("{error}\n{log_tail}")
                }),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabState {
    #[default]
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTabRecord {
    /// `t3`, stable per target id while the record lives.
    pub short: String,
    pub profile: String,
    pub target_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub selected: bool,
    pub opened_by: BrowserActor,
    #[serde(default)]
    pub opened_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<BrowserTouch>,
    pub last_actor: BrowserActor,
    #[serde(default)]
    pub last_at: u64,
    #[serde(default)]
    pub users: Vec<String>,
    #[serde(default)]
    pub dialog_open: bool,
    #[serde(default)]
    pub console_errors: u32,
    #[serde(default)]
    pub state: TabState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
}

impl BrowserTabRecord {
    pub fn key(&self) -> TabKey {
        TabKey::new(&self.profile, &self.target_id)
    }

    /// `profile:tN`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.profile, self.short)
    }

    pub fn is_open(&self) -> bool {
        self.state == TabState::Open
    }
}

/// A pane's current browser tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub key: TabKey,
    /// The pane's herdr tab, as the caller looked when it was set (the
    /// client's `◎` glyph needs it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub last_at: u64,
}

/// A tab as the sidecar reports it (attach/reconcile and the `tabs` op).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct HostTab {
    pub target: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub dialog_open: bool,
}

/// One unsolicited tab event from the sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HostTabEvent {
    pub profile: String,
    pub target: String,
    pub kind: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub opener: Option<String>,
    /// `command` when a herdr operation caused it, else `other`.
    #[serde(default)]
    pub initiator: String,
    #[serde(default)]
    pub selected: Option<bool>,
}

/// The durable part of the ledger (`browser.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LedgerSnapshot {
    #[serde(default)]
    pub tabs: Vec<BrowserTabRecord>,
    #[serde(default)]
    pub cursors: BTreeMap<String, Cursor>,
    #[serde(default)]
    pub next_short: BTreeMap<String, u32>,
    #[serde(default)]
    pub log: Vec<BrowserActivity>,
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Default)]
pub struct BrowserState {
    pub seq: u64,
    pub host: HostStatus,
    pub profiles: BTreeMap<String, ProfileStatus>,
    pub tabs: BTreeMap<TabKey, BrowserTabRecord>,
    pub cursors: HashMap<String, Cursor>,
    pub next_short: BTreeMap<String, u32>,
    pub log: VecDeque<BrowserActivity>,
    /// Set when something changed since the last save.
    pub dirty: bool,
}

/// The herdr+ dashboard and new-tab pages (the companion's `dashboard.html`
/// and `newtab.html`, Chromium's own new tab): furniture, not tabs — never
/// adopted, grouped, overlaid, counted or made current.
pub fn is_dashboard_url(url: &str) -> bool {
    url.starts_with("chrome://newtab")
        || url.starts_with("chrome://new-tab-page")
        || (url.starts_with("chrome-extension://")
            && (url.ends_with("/dashboard.html") || url.ends_with("/newtab.html")))
}

fn clip_detail(detail: &str) -> String {
    let cleaned: String = detail
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if cleaned.chars().count() <= MAX_DETAIL_CHARS {
        cleaned
    } else {
        let mut out: String = cleaned.chars().take(MAX_DETAIL_CHARS - 1).collect();
        out.push('…');
        out
    }
}

impl BrowserState {
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&mut self) {
        self.seq += 1;
        self.dirty = true;
    }

    pub fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            tabs: self.tabs.values().cloned().collect(),
            cursors: self
                .cursors
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            next_short: self.next_short.clone(),
            log: self.log.iter().cloned().collect(),
            seq: self.seq,
        }
    }

    /// Restore the durable part. Every tab is marked closed until the next
    /// reconcile shows it alive; short ids and cursors stay.
    pub fn restore(&mut self, snapshot: LedgerSnapshot, now: u64) {
        self.seq = snapshot.seq.max(self.seq);
        self.next_short = snapshot.next_short;
        for mut record in snapshot.tabs {
            if record.is_open() {
                record.state = TabState::Closed;
                record.closed_at = Some(now);
            }
            self.tabs.insert(record.key(), record);
        }
        self.cursors = snapshot.cursors.into_iter().collect();
        self.log = snapshot.log.into_iter().collect();
        while self.log.len() > LOG_CAPACITY {
            self.log.pop_front();
        }
        self.dirty = false;
    }

    fn allocate_short(&mut self, profile: &str) -> String {
        let next = self.next_short.entry(profile.to_string()).or_insert(1);
        let short = format!("t{next}");
        *next += 1;
        short
    }

    pub fn set_host(&mut self, host: HostStatus) {
        if self.host != host {
            self.host = host;
            self.bump();
        }
    }

    pub fn set_profile(&mut self, profile: &str, status: ProfileStatus) {
        if self.profiles.get(profile) != Some(&status) {
            self.profiles.insert(profile.to_string(), status);
            self.bump();
        }
    }

    pub fn profile(&self, profile: &str) -> ProfileStatus {
        self.profiles
            .get(profile)
            .cloned()
            .unwrap_or(ProfileStatus::Stopped)
    }

    /// Adopt a tab herdr opened: the caller is `opened_by`.
    pub fn adopt_tab(&mut self, key: &TabKey, tab: &HostTab, actor: &BrowserActor, now: u64) {
        if let Some(existing) = self.tabs.get_mut(key) {
            // Only an agent `open` adopts: the target may already be tracked
            // because the sidecar's `opened` event (initiator other) arrived
            // before the open reply, so the opener is this actor.
            existing.state = TabState::Open;
            existing.closed_at = None;
            existing.url = tab.url.clone();
            existing.title = tab.title.clone();
            existing.selected = tab.selected;
            existing.opened_by = actor.clone();
            existing.opened_at = now;
            existing.last_actor = actor.clone();
            existing.last_at = now;
            existing.users = actor
                .pane_id()
                .map(|p| vec![p.to_string()])
                .unwrap_or_default();
            self.bump();
            return;
        }
        let short = self.allocate_short(&key.profile);
        self.tabs.insert(
            key.clone(),
            BrowserTabRecord {
                short,
                profile: key.profile.clone(),
                target_id: key.target_id.clone(),
                url: tab.url.clone(),
                title: tab.title.clone(),
                selected: tab.selected,
                opened_by: actor.clone(),
                opened_at: now,
                last: None,
                last_actor: actor.clone(),
                last_at: now,
                users: actor
                    .pane_id()
                    .map(|p| vec![p.to_string()])
                    .unwrap_or_default(),
                dialog_open: tab.dialog_open,
                console_errors: 0,
                state: TabState::Open,
                closed_at: None,
            },
        );
        self.bump();
    }

    /// Bring the profile's tab set in line with what the sidecar sees: unknown
    /// targets are adopted as the user's, missing ones are closed.
    pub fn reconcile(&mut self, profile: &str, tabs: &[HostTab], now: u64) {
        // The pinned dashboard and new-tab pages are the browser's furniture,
        // never tabs in the ledger.
        let seen: std::collections::HashSet<&str> = tabs
            .iter()
            .filter(|tab| !is_dashboard_url(&tab.url))
            .map(|tab| tab.target.as_str())
            .collect();
        let mut changed = false;
        for tab in tabs.iter().filter(|tab| !is_dashboard_url(&tab.url)) {
            let key = TabKey::new(profile, &tab.target);
            match self.tabs.get_mut(&key) {
                Some(record) => {
                    if !record.is_open()
                        || record.url != tab.url
                        || record.title != tab.title
                        || record.selected != tab.selected
                        || record.dialog_open != tab.dialog_open
                    {
                        record.state = TabState::Open;
                        record.closed_at = None;
                        record.url = tab.url.clone();
                        record.title = tab.title.clone();
                        record.selected = tab.selected;
                        record.dialog_open = tab.dialog_open;
                        changed = true;
                    }
                }
                None => {
                    self.adopt_tab(&key, tab, &BrowserActor::User, now);
                    changed = true;
                }
            }
        }
        let keys: Vec<TabKey> = self
            .tabs
            .values()
            .filter(|record| {
                record.profile == profile
                    && record.is_open()
                    && !seen.contains(record.target_id.as_str())
            })
            .map(BrowserTabRecord::key)
            .collect();
        for key in keys {
            self.close_tab(&key, now);
            changed = true;
        }
        if changed {
            self.bump();
        }
    }

    /// Mark every open tab of a profile closed (the browser quit or crashed).
    pub fn close_profile_tabs(&mut self, profile: &str, now: u64) {
        let keys: Vec<TabKey> = self
            .tabs
            .values()
            .filter(|record| record.profile == profile && record.is_open())
            .map(BrowserTabRecord::key)
            .collect();
        for key in keys {
            self.close_tab(&key, now);
        }
    }

    pub fn close_tab(&mut self, key: &TabKey, now: u64) {
        if let Some(record) = self.tabs.get_mut(key) {
            if record.is_open() {
                record.state = TabState::Closed;
                record.closed_at = Some(now);
                record.dialog_open = false;
                self.bump();
            }
        }
        let panes: Vec<String> = self
            .cursors
            .iter()
            .filter(|(_, cursor)| &cursor.key == key)
            .map(|(pane, _)| pane.clone())
            .collect();
        for pane in panes {
            self.cursors.remove(&pane);
            self.bump();
        }
    }

    /// Apply a sidecar tab event.
    pub fn apply_tab_event(&mut self, event: &HostTabEvent, now: u64) {
        let key = TabKey::new(&event.profile, &event.target);
        match event.kind.as_str() {
            "opened" => {
                if is_dashboard_url(&event.url) {
                    return;
                }
                if !self.tabs.contains_key(&key) {
                    let tab = HostTab {
                        target: event.target.clone(),
                        url: event.url.clone(),
                        title: event.title.clone(),
                        selected: event.selected.unwrap_or(false),
                        dialog_open: false,
                    };
                    let actor = if event.initiator == "command" {
                        // The op that caused it adopts the tab itself with the caller;
                        // a command-opened tab that reaches us first is still herdr's.
                        BrowserActor::External {
                            raw: Some("herdr".into()),
                        }
                    } else {
                        BrowserActor::User
                    };
                    self.adopt_tab(&key, &tab, &actor, now);
                }
            }
            "navigated" => {
                let Some(record) = self.tabs.get_mut(&key) else {
                    return;
                };
                if is_dashboard_url(&event.url) {
                    // A tab turned into the dashboard leaves the ledger.
                    record.state = TabState::Closed;
                    record.closed_at = Some(now);
                    self.bump();
                    return;
                }
                if record.url != event.url {
                    record.url = event.url.clone();
                }
                if !event.title.is_empty() {
                    record.title = event.title.clone();
                }
                record.dialog_open = false;
                if event.initiator != "command" {
                    record.last_actor = BrowserActor::User;
                    record.last_at = now;
                    let profile = record.profile.clone();
                    let tab_id = record.id();
                    let detail = display_url(&event.url);
                    self.push_activity(
                        &profile,
                        Some(tab_id),
                        BrowserActor::User,
                        "navigate",
                        &detail,
                        true,
                        0,
                        now,
                    );
                }
                self.bump();
            }
            "title" => {
                if let Some(record) = self.tabs.get_mut(&key) {
                    if record.title != event.title {
                        record.title = event.title.clone();
                        self.bump();
                    }
                }
            }
            "selected" => {
                let mut changed = false;
                for record in self.tabs.values_mut() {
                    if record.profile != event.profile {
                        continue;
                    }
                    let selected = record.target_id == event.target;
                    if record.selected != selected {
                        record.selected = selected;
                        changed = true;
                    }
                }
                if changed {
                    self.bump();
                }
            }
            "closed" => self.close_tab(&key, now),
            "crashed" => {
                if let Some(record) = self.tabs.get_mut(&key) {
                    record.title = format!("(crashed) {}", record.title);
                    self.bump();
                }
            }
            "console_error" => {
                if let Some(record) = self.tabs.get_mut(&key) {
                    record.console_errors = record.console_errors.saturating_add(1);
                    // Not a seq bump: a chatty page must not make every client re-pull.
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    pub fn set_dialog(&mut self, key: &TabKey, open: bool) {
        if let Some(record) = self.tabs.get_mut(key) {
            if record.dialog_open != open {
                record.dialog_open = open;
                self.bump();
            }
        }
    }

    /// Record an agent operation on a tab (or on the profile when `key` is
    /// `None`) and log it.
    #[allow(clippy::too_many_arguments)]
    pub fn touch(
        &mut self,
        profile: &str,
        key: Option<&TabKey>,
        actor: &BrowserActor,
        op: &str,
        detail: &str,
        ok: bool,
        ms: u64,
        now: u64,
    ) {
        let detail = clip_detail(detail);
        let mut tab_id = None;
        if let Some(key) = key {
            if let Some(record) = self.tabs.get_mut(key) {
                record.last = Some(BrowserTouch {
                    actor: actor.clone(),
                    op: op.to_string(),
                    detail: detail.clone(),
                    at: now,
                    ok,
                });
                record.last_actor = actor.clone();
                record.last_at = now;
                if let Some(pane) = actor.pane_id() {
                    record.users.retain(|p| p != pane);
                    record.users.insert(0, pane.to_string());
                    record.users.truncate(MAX_TAB_USERS);
                }
                tab_id = Some(record.id());
            }
        }
        self.push_activity(profile, tab_id, actor.clone(), op, &detail, ok, ms, now);
        self.bump();
    }

    #[allow(clippy::too_many_arguments)]
    fn push_activity(
        &mut self,
        profile: &str,
        tab: Option<String>,
        actor: BrowserActor,
        op: &str,
        detail: &str,
        ok: bool,
        ms: u64,
        now: u64,
    ) {
        let seq = self.seq + 1;
        self.log.push_back(BrowserActivity {
            seq,
            at: now,
            profile: profile.to_string(),
            tab,
            actor,
            op: op.to_string(),
            detail: detail.to_string(),
            ok,
            ms,
        });
        while self.log.len() > LOG_CAPACITY {
            self.log.pop_front();
        }
    }

    pub fn set_cursor(&mut self, pane_id: &str, key: &TabKey, herdr_tab: Option<&str>, now: u64) {
        self.cursors.insert(
            pane_id.to_string(),
            Cursor {
                key: key.clone(),
                tab_id: herdr_tab.map(str::to_string),
                last_at: now,
            },
        );
        self.bump();
    }

    pub fn touch_cursor(&mut self, pane_id: &str, now: u64) {
        if let Some(cursor) = self.cursors.get_mut(pane_id) {
            cursor.last_at = now;
            self.dirty = true;
        }
    }

    /// The pane's current tab, if it is still open.
    pub fn cursor(&self, pane_id: &str) -> Option<&BrowserTabRecord> {
        let cursor = self.cursors.get(pane_id)?;
        self.tabs.get(&cursor.key).filter(|record| record.is_open())
    }

    /// The profile a pane's cursor points at (open or not).
    pub fn cursor_profile(&self, pane_id: &str) -> Option<&str> {
        self.cursors
            .get(pane_id)
            .map(|cursor| cursor.key.profile.as_str())
    }

    /// Resolve `main:t3` / `t3` (in `default_profile`) to a tab record.
    pub fn resolve_tab(&self, default_profile: &str, text: &str) -> Option<&BrowserTabRecord> {
        let text = text.trim();
        let (profile, short) = match text.split_once(':') {
            Some((profile, short)) => (profile, short),
            None => (default_profile, text),
        };
        self.tabs
            .values()
            .find(|record| record.profile == profile && record.short == short)
    }

    /// Drop closed-tab records past their retention and old crash marks.
    pub fn prune(&mut self, now: u64) {
        let before = self.tabs.len();
        self.tabs.retain(|_, record| match record.closed_at {
            Some(at) if !record.is_open() => now.saturating_sub(at) < CLOSED_TAB_RETENTION_SECS,
            _ => true,
        });
        let mut changed = before != self.tabs.len();
        let expired: Vec<String> = self
            .profiles
            .iter()
            .filter_map(|(name, status)| match status {
                ProfileStatus::Crashed { at, .. } | ProfileStatus::UserQuit { at }
                    if now.saturating_sub(*at) >= CRASH_RETENTION_SECS =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        for name in expired {
            self.profiles.insert(name, ProfileStatus::Stopped);
            changed = true;
        }
        if changed {
            self.bump();
        }
    }

    pub fn open_tabs<'a>(
        &'a self,
        profile: &'a str,
    ) -> impl Iterator<Item = &'a BrowserTabRecord> + 'a {
        self.tabs
            .values()
            .filter(move |record| record.profile == profile && record.is_open())
    }

    fn tab_info(&self, record: &BrowserTabRecord, now: u64, active_seconds: u64) -> BrowserTabInfo {
        let active = record.is_open()
            && !record.last_actor.is_user()
            && now.saturating_sub(record.last_at) <= active_seconds;
        BrowserTabInfo {
            id: record.id(),
            profile: record.profile.clone(),
            target_id: record.target_id.clone(),
            url: record.url.clone(),
            title: record.title.clone(),
            selected: record.selected,
            opened_by: record.opened_by.clone(),
            last: record.last.clone(),
            last_actor: record.last_actor.clone(),
            users: record.users.clone(),
            dialog_open: record.dialog_open,
            console_errors: record.console_errors,
            active,
            state: match record.state {
                TabState::Open => "open".into(),
                TabState::Closed => "closed".into(),
            },
            closed_at: record.closed_at,
        }
    }

    /// The compact projection the client shell pulls.
    pub fn get_info(
        &self,
        since_seq: Option<u64>,
        now: u64,
        active_seconds: u64,
        enabled: bool,
        temporary: &dyn Fn(&str) -> bool,
    ) -> BrowserGetInfo {
        if since_seq == Some(self.seq) {
            return BrowserGetInfo {
                seq: self.seq,
                unchanged: true,
                enabled,
                ..Default::default()
            };
        }
        let tabs: Vec<BrowserTabInfo> = self
            .tabs
            .values()
            .filter(|record| record.is_open())
            .map(|record| self.tab_info(record, now, active_seconds))
            .collect();
        let profiles = self
            .profiles
            .iter()
            .map(|(name, status)| {
                let open: Vec<&BrowserTabRecord> = self.open_tabs(name).collect();
                let agents = open
                    .iter()
                    .filter(|record| {
                        !record.last_actor.is_user()
                            && now.saturating_sub(record.last_at) <= active_seconds
                    })
                    .filter_map(|record| record.last_actor.pane_id())
                    .collect::<std::collections::HashSet<_>>()
                    .len() as u32;
                let (pid, port, since, exe, detail) = match status {
                    ProfileStatus::Running {
                        pid,
                        port,
                        since,
                        exe,
                    } => (
                        Some(*pid),
                        Some(*port),
                        Some(*since),
                        Some(exe.clone()),
                        None,
                    ),
                    ProfileStatus::Starting { since } => (None, None, Some(*since), None, None),
                    ProfileStatus::Crashed { at, detail } => {
                        (None, None, Some(*at), None, Some(detail.clone()))
                    }
                    ProfileStatus::UserQuit { at } => (None, None, Some(*at), None, None),
                    ProfileStatus::InUse { pid, at } => (
                        *pid,
                        None,
                        Some(*at),
                        None,
                        Some("another process holds this profile".into()),
                    ),
                    ProfileStatus::Stopped => (None, None, None, None, None),
                };
                BrowserProfileInfo {
                    name: name.clone(),
                    state: status.name().into(),
                    pid,
                    port,
                    exe,
                    since,
                    tabs: open.len() as u32,
                    agents,
                    dialogs: open.iter().filter(|record| record.dialog_open).count() as u32,
                    temporary: temporary(name),
                    detail,
                    companion: None,
                }
            })
            .collect();
        let mut recent_panes: Vec<BrowserPaneCursor> = self
            .cursors
            .iter()
            .filter_map(|(pane_id, cursor)| {
                let record = self.tabs.get(&cursor.key)?;
                Some(BrowserPaneCursor {
                    pane_id: pane_id.clone(),
                    tab_id: cursor.tab_id.clone(),
                    current: record.id(),
                    last_at: cursor.last_at,
                })
            })
            .collect();
        recent_panes.sort_by(|a, b| b.last_at.cmp(&a.last_at).then(a.pane_id.cmp(&b.pane_id)));
        BrowserGetInfo {
            seq: self.seq,
            unchanged: false,
            enabled,
            host: self.host.info(),
            profiles,
            tabs,
            recent_panes,
        }
    }

    /// Activity, newest first, filtered by pane and/or tab id.
    pub fn activity(
        &self,
        limit: usize,
        pane_id: Option<&str>,
        tab: Option<&str>,
    ) -> Vec<BrowserActivity> {
        self.log
            .iter()
            .rev()
            .filter(|entry| pane_id.is_none_or(|pane| entry.actor.pane_id() == Some(pane)))
            .filter(|entry| tab.is_none_or(|tab| entry.tab.as_deref() == Some(tab)))
            .take(limit)
            .cloned()
            .collect()
    }
}

/// `github.com/foo/bar` for a URL (scheme dropped, query and fragment kept
/// short); the whole string when it is not a URL.
pub fn display_url(url: &str) -> String {
    let stripped = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let stripped = stripped.strip_suffix('/').unwrap_or(stripped);
    clip_detail(stripped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str) -> BrowserActor {
        BrowserActor::Pane {
            pane_id: id.into(),
            tab_id: "w2:tD".into(),
            workspace_id: "w2".into(),
            tab_label: "planner".into(),
            workspace_label: None,
            agent: Some("claude".into()),
            session: "default".into(),
            gone: false,
            shell_pid: None,
        }
    }

    fn host_tab(target: &str, url: &str) -> HostTab {
        HostTab {
            target: target.into(),
            url: url.into(),
            title: url.into(),
            selected: false,
            dialog_open: false,
        }
    }

    #[test]
    fn short_ids_are_stable_per_target_and_per_profile() {
        let mut state = BrowserState::new();
        let a = TabKey::new("main", "A");
        let b = TabKey::new("main", "B");
        let w = TabKey::new("work", "W");
        state.adopt_tab(&a, &host_tab("A", "https://a/"), &pane("w2:pD"), 10);
        state.adopt_tab(&b, &host_tab("B", "https://b/"), &BrowserActor::User, 11);
        state.adopt_tab(&w, &host_tab("W", "https://w/"), &BrowserActor::User, 12);
        assert_eq!(state.tabs[&a].short, "t1");
        assert_eq!(state.tabs[&b].short, "t2");
        assert_eq!(state.tabs[&w].short, "t1");
        assert_eq!(state.tabs[&a].id(), "main:t1");
        assert_eq!(state.resolve_tab("main", "t2").unwrap().target_id, "B");
        assert_eq!(state.resolve_tab("main", "work:t1").unwrap().target_id, "W");
        assert!(state.resolve_tab("main", "t9").is_none());

        // A reload keeps ids and closes everything until reconciled.
        let snapshot = state.snapshot();
        let mut reloaded = BrowserState::new();
        reloaded.restore(snapshot, 20);
        assert!(reloaded.tabs.values().all(|record| !record.is_open()));
        reloaded.reconcile("main", &[host_tab("A", "https://a/2")], 21);
        assert!(reloaded.tabs[&a].is_open());
        assert_eq!(reloaded.tabs[&a].short, "t1");
        assert_eq!(reloaded.tabs[&a].url, "https://a/2");
        assert!(!reloaded.tabs[&b].is_open());
        let c = TabKey::new("main", "C");
        reloaded.reconcile(
            "main",
            &[host_tab("A", "https://a/2"), host_tab("C", "x")],
            22,
        );
        assert_eq!(
            reloaded.tabs[&c].short, "t3",
            "the counter survived the reload"
        );
        assert!(reloaded.tabs[&c].opened_by.is_user());
    }

    #[test]
    fn an_opened_event_that_beats_the_open_reply_still_attributes_the_agent() {
        let mut state = BrowserState::new();
        state.apply_tab_event(
            &HostTabEvent {
                profile: "main".into(),
                target: "T".into(),
                kind: "opened".into(),
                url: "https://a/".into(),
                title: String::new(),
                opener: None,
                initiator: "other".into(),
                selected: None,
            },
            5,
        );
        let key = TabKey::new("main", "T");
        assert!(state.tabs[&key].opened_by.is_user());
        state.adopt_tab(&key, &host_tab("T", "https://a/"), &pane("w2:pD"), 6);
        let record = &state.tabs[&key];
        assert_eq!(record.opened_by.pane_id(), Some("w2:pD"));
        assert_eq!(record.last_actor.pane_id(), Some("w2:pD"));
        assert_eq!(record.opened_at, 6);
        assert_eq!(record.users, ["w2:pD"]);
        assert_eq!(record.short, "t1", "the id allocated for the event is kept");
    }

    #[test]
    fn user_navigation_flips_last_actor_but_keeps_the_last_touch() {
        let mut state = BrowserState::new();
        let a = TabKey::new("main", "A");
        state.adopt_tab(&a, &host_tab("A", "https://a/"), &pane("w2:pD"), 10);
        state.touch(
            "main",
            Some(&a),
            &pane("w2:pD"),
            "read",
            "markdown 20k/48k",
            true,
            300,
            11,
        );
        state.apply_tab_event(
            &HostTabEvent {
                profile: "main".into(),
                target: "A".into(),
                kind: "navigated".into(),
                url: "https://a/b".into(),
                title: String::new(),
                opener: None,
                initiator: "other".into(),
                selected: None,
            },
            12,
        );
        let record = &state.tabs[&a];
        assert!(record.last_actor.is_user());
        assert_eq!(record.last.as_ref().unwrap().op, "read");
        assert_eq!(record.url, "https://a/b");
        let log = state.activity(10, None, None);
        assert_eq!(log[0].op, "navigate");
        assert!(log[0].actor.is_user());
        assert_eq!(log[0].detail, "a/b");
        assert_eq!(log[1].op, "read");
        assert_eq!(state.activity(10, Some("w2:pD"), None).len(), 1);
        assert_eq!(state.activity(10, None, Some("main:t1")).len(), 2);
    }

    #[test]
    fn closing_a_tab_clears_cursors_and_the_next_call_has_none() {
        let mut state = BrowserState::new();
        let a = TabKey::new("main", "A");
        state.adopt_tab(&a, &host_tab("A", "https://a/"), &pane("w2:pD"), 10);
        state.set_cursor("w2:pD", &a, Some("w2:tD"), 10);
        assert_eq!(state.cursor("w2:pD").unwrap().target_id, "A");
        state.apply_tab_event(
            &HostTabEvent {
                profile: "main".into(),
                target: "A".into(),
                kind: "closed".into(),
                url: String::new(),
                title: String::new(),
                opener: None,
                initiator: "other".into(),
                selected: None,
            },
            20,
        );
        assert!(state.cursor("w2:pD").is_none());
        assert!(!state.tabs[&a].is_open());
        state.prune(20 + CLOSED_TAB_RETENTION_SECS);
        assert!(!state.tabs.contains_key(&a));
    }

    #[test]
    fn dashboard_and_new_tab_pages_never_enter_the_ledger() {
        let mut state = BrowserState::new();
        let tabs = vec![
            HostTab {
                target: "D".into(),
                url: "chrome-extension://jmegdddadjhcnkdfpmeeockadkdbfpop/dashboard.html".into(),
                title: "herdr+ dashboard".into(),
                selected: false,
                dialog_open: false,
            },
            HostTab {
                target: "N".into(),
                url: "chrome://newtab/".into(),
                title: "New Tab".into(),
                selected: true,
                dialog_open: false,
            },
            HostTab {
                target: "S".into(),
                url: "https://site.test/".into(),
                title: "Site".into(),
                selected: false,
                dialog_open: false,
            },
        ];
        state.reconcile("main", &tabs, 10);
        assert_eq!(state.open_tabs("main").count(), 1);
        assert!(!state.tabs.contains_key(&TabKey::new("main", "D")));
        assert!(!state.tabs.contains_key(&TabKey::new("main", "N")));
        // an `opened` event for the dashboard is ignored; a tab that turns into it leaves
        state.apply_tab_event(
            &HostTabEvent {
                profile: "main".into(),
                target: "D2".into(),
                kind: "opened".into(),
                url: "chrome-extension://x/dashboard.html".into(),
                title: String::new(),
                initiator: "other".into(),
                selected: None,
                opener: None,
            },
            11,
        );
        assert!(!state.tabs.contains_key(&TabKey::new("main", "D2")));
        state.apply_tab_event(
            &HostTabEvent {
                profile: "main".into(),
                target: "S".into(),
                kind: "navigated".into(),
                url: "chrome-extension://x/newtab.html".into(),
                title: String::new(),
                initiator: "other".into(),
                selected: None,
                opener: None,
            },
            12,
        );
        assert_eq!(state.open_tabs("main").count(), 0);
        assert!(is_dashboard_url("chrome://new-tab-page/"));
        assert!(!is_dashboard_url("https://example.com/dashboard.html"));
    }

    #[test]
    fn get_info_reports_unchanged_and_activity_windows() {
        let mut state = BrowserState::new();
        state.set_profile(
            "main",
            ProfileStatus::Running {
                pid: 1,
                port: 2,
                since: 5,
                exe: "Chromium".into(),
            },
        );
        let a = TabKey::new("main", "A");
        state.adopt_tab(&a, &host_tab("A", "https://a/"), &pane("w2:pD"), 10);
        state.touch("main", Some(&a), &pane("w2:pD"), "read", "x", true, 1, 100);
        state.set_cursor("w2:pD", &a, Some("w2:tD"), 100);
        let info = state.get_info(None, 150, 120, true, &|_| false);
        assert!(!info.unchanged);
        assert_eq!(info.profiles[0].tabs, 1);
        assert_eq!(info.profiles[0].agents, 1);
        assert!(info.tabs[0].active);
        assert_eq!(info.recent_panes[0].current, "main:t1");
        assert_eq!(info.recent_panes[0].tab_id.as_deref(), Some("w2:tD"));
        let same = state.get_info(Some(info.seq), 150, 120, true, &|_| false);
        assert!(same.unchanged);
        assert!(same.tabs.is_empty());
        let later = state.get_info(None, 100 + 121, 120, true, &|_| false);
        assert!(!later.tabs[0].active);
        assert_eq!(later.profiles[0].agents, 0);
    }

    #[test]
    fn users_are_most_recent_first_and_bounded() {
        let mut state = BrowserState::new();
        let a = TabKey::new("main", "A");
        state.adopt_tab(&a, &host_tab("A", "x"), &pane("p0"), 1);
        for i in 0..12 {
            state.touch(
                "main",
                Some(&a),
                &pane(&format!("p{i}")),
                "read",
                "",
                true,
                1,
                2 + i,
            );
        }
        let users = &state.tabs[&a].users;
        assert_eq!(users.len(), MAX_TAB_USERS);
        assert_eq!(users[0], "p11");
        state.touch("main", Some(&a), &pane("p5"), "read", "", true, 1, 99);
        assert_eq!(state.tabs[&a].users[0], "p5");
        assert_eq!(
            state.tabs[&a].users.iter().filter(|u| *u == "p5").count(),
            1
        );
    }

    #[test]
    fn log_is_a_ring_and_details_are_clipped() {
        let mut state = BrowserState::new();
        for i in 0..(LOG_CAPACITY + 7) {
            state.touch(
                "main",
                None,
                &BrowserActor::User,
                "eval",
                &"x".repeat(500),
                true,
                1,
                i as u64,
            );
        }
        assert_eq!(state.log.len(), LOG_CAPACITY);
        assert_eq!(state.log.front().unwrap().at, 7);
        assert_eq!(
            state.log.back().unwrap().detail.chars().count(),
            MAX_DETAIL_CHARS
        );
        assert_eq!(
            display_url("https://github.com/foo/bar/"),
            "github.com/foo/bar"
        );
        assert_eq!(display_url("about:blank"), "about:blank");
    }
}
