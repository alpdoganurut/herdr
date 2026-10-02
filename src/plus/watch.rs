//! `herdr plus run`: the singleton watcher (live data, wake-ups) and the
//! coordinator lifecycle.
//!
//! Every tick the watcher rebuilds `live.json` from the JSON API, diffs the
//! managed agents against what it saw last, and queues the changes. A
//! wake-up (`agent.prompt` into the coordinator agent, pointing at a digest
//! file) fires only for an idle, settled coordinator with no live turn,
//! within the gap and the hourly/daily caps, and only when something is
//! worth it: a high item, a normal item older than the debounce, the slow
//! periodic check with anything pending, or `herdr plus coordinator wake`.
//! [`tick`] is the pure policy; [`run`] does the I/O around it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::api::{self, Api, ApiError};
use super::launch::{self, ClaudeSession, LaunchCtx};
use super::live::{self, LiveAgent, LiveData, WatchSummary};
use super::messages::{self, AgentMessage};
use super::registry::{self, ManagePatch, Registry};
use super::turn::{self, Turn, TurnStep};
use super::{
    now_unix, registry_path, wake_dir, wake_request_path, wakeups_path, write_atomically,
    COORDINATOR_ROLE, WATCH_LOCK,
};
use crate::api::schema::{BrowserOp, BrowserRunParams, Method};

pub const DEBOUNCE_ENV: &str = "HERDR_PLUS_WAKE_DEBOUNCE_S";
pub const GAP_ENV: &str = "HERDR_PLUS_WAKE_GAP_S";
pub const PERIODIC_ENV: &str = "HERDR_PLUS_PERIODIC_S";
pub const CAP_HOUR_ENV: &str = "HERDR_PLUS_WAKE_CAP_HOUR";
pub const CAP_DAY_ENV: &str = "HERDR_PLUS_WAKE_CAP_DAY";

/// The coordinator's sidebar group and tab.
pub const COORDINATOR_GROUP: &str = "herdr+";
pub const COORDINATOR_TAB: &str = "coordinator";
const COORDINATOR_NAME: &str = "coordinator";
const START_TIMEOUT_MS: u64 = 60_000;
/// How long a resumed coordinator gets to show up before a fresh session replaces it.
const RESUME_CHECK_S: u64 = 20;

/// Pending items beyond this fold into one `+N earlier` line.
const PENDING_CAP: usize = 30;
const DIGEST_MAX_LINES: usize = 25;
const DIGESTS_KEPT: usize = 50;
const PREVIEWS: usize = 3;
const PREVIEW_CHARS: usize = 80;
const HELD_LOG_EVERY_S: u64 = 60;
/// A sender's refused messages to the coordinator wake it at most once per
/// this window.
const REFUSAL_REPEAT_S: u64 = 600;
const LIVE_REFRESH_S: u64 = 10;
const ROTATE_EVERY_S: u64 = 300;
const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
const ERROR_BACKOFF: Duration = Duration::from_secs(10);
const COORDINATOR_RETRY_S: u64 = 60;
/// Messages kept in `live.json` for the dashboard's log.
const LIVE_MESSAGES: usize = 50;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

pub struct WatchOpts {
    pub dir: PathBuf,
    pub port: u16,
    pub serve: bool,
    pub coordinator: bool,
    pub interval_ms: u64,
    pub ctx: LaunchCtx,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeCfg {
    pub debounce_s: u64,
    pub gap_s: u64,
    pub gap_high_s: u64,
    pub settle_s: u64,
    pub periodic_s: u64,
    pub cap_hour: u32,
    pub cap_day: u32,
    pub missing_s: u64,
    pub relaunch_cap_hour: u32,
    /// Relaunch a coordinator that went missing (off with `--no-coordinator`).
    pub relaunch: bool,
}

impl Default for WakeCfg {
    fn default() -> Self {
        Self {
            debounce_s: 60,
            gap_s: 120,
            gap_high_s: 45,
            settle_s: 5,
            periodic_s: 3600,
            cap_hour: 12,
            cap_day: 80,
            missing_s: 30,
            relaunch_cap_hour: 3,
            relaunch: true,
        }
    }
}

impl WakeCfg {
    /// The §4 defaults with the `HERDR_PLUS_*` overrides applied.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let number = |name: &str| get(name).and_then(|value| value.trim().parse::<u64>().ok());
        let mut cfg = Self::default();
        if let Some(value) = number(DEBOUNCE_ENV) {
            cfg.debounce_s = value;
        }
        if let Some(value) = number(GAP_ENV) {
            cfg.gap_s = value;
        }
        if let Some(value) = number(PERIODIC_ENV) {
            cfg.periodic_s = value.max(1);
        }
        if let Some(value) = number(CAP_HOUR_ENV) {
            cfg.cap_hour = u32::try_from(value).unwrap_or(u32::MAX);
        }
        if let Some(value) = number(CAP_DAY_ENV) {
            cfg.cap_day = u32::try_from(value).unwrap_or(u32::MAX);
        }
        cfg
    }

    /// The gap that applies; the high lane is never slower than the normal one.
    fn gap(&self, high: bool) -> u64 {
        if high {
            self.gap_high_s.min(self.gap_s)
        } else {
            self.gap_s
        }
    }
}

/// How urgently a pending item wants a wake-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Prio {
    /// Only rides along, or wakes with the periodic check.
    Low,
    /// Wakes once the oldest one is older than the debounce.
    Normal,
    /// Wakes after the (shorter) high gap.
    High,
}

/// A managed agent as the digest names it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Who {
    pub name: String,
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Who {
    fn of(agent: &LiveAgent) -> Self {
        Self {
            name: agent.name.clone(),
            pane_id: agent.pane_id.clone(),
            agent: agent.agent.clone(),
            role: agent.role.clone(),
            project: agent.project.clone(),
        }
    }

    /// `rev (w2:p4, codex, reviewer/demo)`.
    fn label(&self) -> String {
        let mut parts = vec![self.pane_id.clone()];
        if let Some(agent) = &self.agent {
            parts.push(agent.clone());
        }
        match (&self.role, &self.project) {
            (Some(role), Some(project)) => parts.push(format!("{role}/{project}")),
            (Some(tag), None) | (None, Some(tag)) => parts.push(tag.clone()),
            (None, None) => {}
        }
        format!("{} ({})", self.name, parts.join(", "))
    }
}

/// Which messages a [`Ev::Messages`] item counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgScope {
    /// Addressed to the coordinator but not typed in (busy, blocked, ...): high.
    ToCoordinator,
    /// Between other agents: normal.
    Between,
    /// The coordinator's own: low.
    FromCoordinator,
}

/// One pending change. Items coalesce: one `Status` per agent key (first
/// `from`, latest `to`, a change count), one `Messages` per scope, one
/// `Registry`. They persist in `watch_state.json` until a wake-up delivers them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Ev {
    Status {
        key: String,
        who: Who,
        from: String,
        to: String,
        first_at: u64,
        at: u64,
        changes: u32,
        /// Set once the agent stayed `blocked` for two ticks: the high lane.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_since: Option<u64>,
    },
    Appeared {
        key: String,
        who: Who,
        at: u64,
        /// A known agent under a new pane id (moved, restored).
        relinked: bool,
    },
    /// A managed agent stopped running (its registry entry is offline).
    Gone { key: String, who: Who, at: u64 },
    Messages {
        scope: MsgScope,
        count: usize,
        preview: Vec<String>,
        first_at: u64,
        at: u64,
    },
    Registry {
        first_at: u64,
        at: u64,
        changes: u32,
    },
    /// Older items folded away by the pending cap (digest only).
    Earlier { count: u64, since: u64 },
}

fn is_idle(status: &str) -> bool {
    matches!(status, "idle" | "done")
}

impl Ev {
    /// The lane, or `None` for an item that should not be reported (a status
    /// flap that ended where it started, fewer than 4 changes).
    pub fn prio(&self) -> Option<Prio> {
        Some(match self {
            Ev::Status {
                from,
                to,
                changes,
                blocked_since,
                ..
            } => {
                if blocked_since.is_some() {
                    Prio::High
                } else if from == to {
                    if *changes < 4 {
                        return None;
                    }
                    if is_idle(to) {
                        Prio::Normal
                    } else {
                        Prio::Low
                    }
                } else if is_idle(to) && matches!(from.as_str(), "working" | "blocked") {
                    // Finished (or answered and finished).
                    Prio::Normal
                } else if from == "blocked" && to == "working" {
                    Prio::Normal
                } else {
                    Prio::Low
                }
            }
            Ev::Appeared { .. } => Prio::Normal,
            Ev::Gone { .. } => Prio::High,
            Ev::Messages { scope, .. } => match scope {
                MsgScope::ToCoordinator => Prio::High,
                MsgScope::Between => Prio::Normal,
                MsgScope::FromCoordinator => Prio::Low,
            },
            Ev::Registry { .. } | Ev::Earlier { .. } => Prio::Low,
        })
    }

    fn first_at(&self) -> u64 {
        match self {
            Ev::Status { first_at, .. }
            | Ev::Messages { first_at, .. }
            | Ev::Registry { first_at, .. } => *first_at,
            Ev::Appeared { at, .. } | Ev::Gone { at, .. } => *at,
            Ev::Earlier { since, .. } => *since,
        }
    }

    fn key(&self) -> Option<&str> {
        match self {
            Ev::Status { key, .. } | Ev::Appeared { key, .. } | Ev::Gone { key, .. } => Some(key),
            _ => None,
        }
    }

    fn rekey(&mut self, old: &str, new: &str) {
        if let Ev::Status { key, .. } | Ev::Appeared { key, .. } | Ev::Gone { key, .. } = self {
            if key == old {
                *key = new.to_string();
            }
        }
    }
}

/// What the watcher last saw of a managed agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    status: String,
    pane_id: String,
    since: u64,
    blocked_ticks: u32,
}

/// The watcher's state. Counters, pending items and the message offset
/// persist in `watch_state.json`; what the watcher saw of the agents does
/// not: after a restart (or a server error) it takes a fresh baseline, so
/// nothing replays as events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchState {
    /// The last delivered wake-up.
    #[serde(default)]
    pub wake_seq: u64,
    #[serde(default)]
    pub last_wake: u64,
    /// Delivery times within the last day (the caps).
    #[serde(default)]
    pub wakes: Vec<u64>,
    /// Coordinator relaunch times within the last hour.
    #[serde(default)]
    pub relaunches: Vec<u64>,
    #[serde(default)]
    pub coordinator_down: bool,
    /// Byte offset into `messages.jsonl`.
    #[serde(default)]
    pub msg_offset: u64,
    #[serde(default)]
    pub registry_mtime_ms: u64,
    /// `herdr plus coordinator wake` is waiting for an idle coordinator.
    #[serde(default)]
    pub forced: bool,
    /// No wake-up before this (a failed prompt backs off by the gap).
    #[serde(default)]
    pub retry_after: u64,
    #[serde(default)]
    pub pending: Vec<Ev>,
    /// Items folded away by the pending cap since the last wake-up.
    #[serde(default)]
    pub folded: u64,
    #[serde(default)]
    pub folded_since: u64,
    /// Last status change per pane id (`live.json` `last_change_unix`).
    #[serde(default)]
    pub last_change: BTreeMap<String, u64>,
    /// Where the digests live (for the wake-up prompt); set by [`WatchState::load`].
    #[serde(skip)]
    pub dir: PathBuf,
    /// `false` until the first good tick after a (re)start or a server error.
    #[serde(skip)]
    baselined: bool,
    #[serde(skip)]
    baseline_at: u64,
    #[serde(skip)]
    seen: BTreeMap<String, Seen>,
    #[serde(skip)]
    pane_status: BTreeMap<String, String>,
    #[serde(skip)]
    coord_idle_since: Option<u64>,
    #[serde(skip)]
    coord_missing_since: Option<u64>,
    #[serde(skip)]
    held_log: Option<(String, u64)>,
    /// When each sender's last refused message to the coordinator was queued.
    #[serde(skip)]
    refusal_queued: BTreeMap<String, u64>,
}

impl WatchState {
    /// The persisted state (default on a missing or unparseable file); it
    /// takes a fresh baseline on its first tick.
    pub fn load(dir: &Path) -> Self {
        let mut state: WatchState = std::fs::read(super::watch_state_path(dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        state.dir = dir.to_path_buf();
        state
    }

    pub fn save(&self) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomically(&super::watch_state_path(&self.dir), &json)
    }

    /// Take a fresh baseline on the next tick (a server error or restart).
    pub fn rebaseline(&mut self) {
        self.baselined = false;
    }

    /// Wake-up `seq` was typed into the coordinator.
    pub fn wake_delivered(&mut self, seq: u64, now: u64) {
        self.wake_seq = self.wake_seq.max(seq);
        self.last_wake = now;
        self.wakes.push(now);
        self.wakes.retain(|at| now.saturating_sub(*at) < DAY);
        self.pending.clear();
        self.folded = 0;
        self.folded_since = 0;
        self.forced = false;
        self.retry_after = 0;
        self.held_log = None;
    }

    /// The prompt failed: keep everything pending and back off by the gap.
    pub fn wake_failed(&mut self, now: u64, cfg: &WakeCfg) {
        self.retry_after = now + cfg.gap_s;
    }

    fn wakes_within(&self, window: u64, now: u64) -> usize {
        self.wakes
            .iter()
            .filter(|at| now.saturating_sub(**at) < window)
            .count()
    }

    fn capped(&self, cfg: &WakeCfg, now: u64) -> bool {
        self.wakes_within(HOUR, now) >= cfg.cap_hour as usize
            || self.wakes_within(DAY, now) >= cfg.cap_day as usize
    }

    /// Reportable pending items (flaps dropped, folded ones counted).
    fn reportable(&self) -> usize {
        self.pending.iter().filter(|ev| ev.prio().is_some()).count() + self.folded as usize
    }

    /// The dashboard's view of the watcher.
    pub fn summary(&self, cfg: &WakeCfg, turn_live: bool, now: u64) -> WatchSummary {
        WatchSummary {
            last_wake_unix: self.last_wake,
            wake_seq: self.wake_seq,
            pending: self.reportable() as u64,
            capped: self.capped(cfg, now),
            coordinator_down: self.coordinator_down,
            turn_live,
            wakes_last_hour: self.wakes_within(HOUR, now) as u64,
            board_unix: mtime_ms(&super::board_path(&self.dir)) / 1000,
        }
    }

    fn status_item(&mut self, key: &str) -> Option<&mut Ev> {
        self.pending
            .iter_mut()
            .find(|ev| matches!(ev, Ev::Status { key: k, .. } if k == key))
    }

    fn status_change(&mut self, key: &str, who: Who, from: &str, to: &str, now: u64) {
        if let Some(Ev::Status {
            who: w,
            to: t,
            at,
            changes,
            blocked_since,
            ..
        }) = self.status_item(key)
        {
            *w = who;
            *t = to.to_string();
            *at = now;
            *changes += 1;
            *blocked_since = None;
            return;
        }
        self.pending.push(Ev::Status {
            key: key.to_string(),
            who,
            from: from.to_string(),
            to: to.to_string(),
            first_at: now,
            at: now,
            changes: 1,
            blocked_since: None,
        });
    }

    fn confirm_blocked(&mut self, key: &str, who: Who, since: u64, now: u64) {
        if let Some(Ev::Status {
            to, blocked_since, ..
        }) = self.status_item(key)
        {
            if to == "blocked" {
                *blocked_since = Some(since);
                return;
            }
        }
        self.pending.push(Ev::Status {
            key: key.to_string(),
            who,
            from: "blocked".into(),
            to: "blocked".into(),
            first_at: now,
            at: now,
            changes: 0,
            blocked_since: Some(since),
        });
    }

    fn add_messages(&mut self, scope: MsgScope, previews: Vec<String>, now: u64) {
        let count = previews.len();
        if count == 0 {
            return;
        }
        let existing = self
            .pending
            .iter_mut()
            .find(|ev| matches!(ev, Ev::Messages { scope: s, .. } if *s == scope));
        if let Some(Ev::Messages {
            count: c,
            preview,
            at,
            ..
        }) = existing
        {
            *c += count;
            *at = now;
            let room = PREVIEWS.saturating_sub(preview.len());
            preview.extend(previews.into_iter().take(room));
            return;
        }
        self.pending.push(Ev::Messages {
            scope,
            count,
            preview: previews.into_iter().take(PREVIEWS).collect(),
            first_at: now,
            at: now,
        });
    }

    fn registry_changed(&mut self, now: u64) {
        for ev in &mut self.pending {
            if let Ev::Registry { at, changes, .. } = ev {
                *at = now;
                *changes += 1;
                return;
            }
        }
        self.pending.push(Ev::Registry {
            first_at: now,
            at: now,
            changes: 1,
        });
    }

    /// Fold the oldest items into `+N earlier` beyond the pending cap.
    fn cap_pending(&mut self) {
        while self.pending.len() > PENDING_CAP {
            let Some((oldest, _)) = self
                .pending
                .iter()
                .enumerate()
                .min_by_key(|(_, ev)| ev.first_at())
            else {
                break;
            };
            let ev = self.pending.remove(oldest);
            if self.folded == 0 || ev.first_at() < self.folded_since {
                self.folded_since = ev.first_at();
            }
            self.folded += 1;
        }
    }

    /// The items a digest reports: high first, then oldest first.
    fn digest_events(&self) -> Vec<Ev> {
        let mut evs: Vec<(Prio, &Ev)> = self
            .pending
            .iter()
            .filter_map(|ev| Some((ev.prio()?, ev)))
            .collect();
        evs.sort_by(|(pa, a), (pb, b)| pb.cmp(pa).then(a.first_at().cmp(&b.first_at())));
        let mut evs: Vec<Ev> = evs.into_iter().map(|(_, ev)| ev.clone()).collect();
        if self.folded > 0 {
            evs.push(Ev::Earlier {
                count: self.folded,
                since: self.folded_since,
            });
        }
        evs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write `wake/<seq>.md` and the turn marker, then prompt `pane`.
    Wake {
        seq: u64,
        digest: String,
        prompt: String,
        pane: String,
        items: usize,
    },
    Relaunch {
        resume: Option<String>,
    },
    /// Rewrite the turn marker with `seen_working`.
    MarkTurnWorking,
    /// Delete the turn marker.
    ClearTurn,
    Log(String),
}

/// The key the watcher tracks an agent by: native session id, else pane id.
fn agent_key(agent: &LiveAgent) -> String {
    agent
        .session
        .clone()
        .unwrap_or_else(|| agent.pane_id.clone())
}

/// The coordinator's pane: live, else its offline registry entry's last pane.
fn coordinator_pane(live: &LiveData) -> Option<&str> {
    live.coordinator_pane.as_deref().or_else(|| {
        live.offline
            .iter()
            .find(|entry| entry.role.as_deref() == Some(COORDINATOR_ROLE))
            .and_then(|entry| entry.pane_id.as_deref())
    })
}

/// Pure core: diff + policy. No I/O.
#[allow(clippy::too_many_arguments)] // the design's fixed signature: each input is a separate fact
pub fn tick(
    state: &mut WatchState,
    live: &LiveData,
    new_msgs: &[AgentMessage],
    registry_changed: bool,
    turn: Option<&Turn>,
    wake_requested: bool,
    cfg: &WakeCfg,
    now: u64,
) -> Vec<Action> {
    let mut actions = Vec::new();
    if wake_requested {
        state.forced = true;
    }
    track_last_change(state, live, now);
    let coordinator = live.agents.iter().find(|agent| agent.coordinator);
    let mut turn_live = turn.is_some();
    if let Some(turn) = turn {
        let status = coordinator.map_or("offline", |agent| agent.status.as_str());
        match turn::advance(turn, status, now) {
            TurnStep::MarkWorking if !turn.seen_working => actions.push(Action::MarkTurnWorking),
            TurnStep::Clear => {
                actions.push(Action::ClearTurn);
                turn_live = false;
            }
            _ => {}
        }
    }
    liveness(state, live, coordinator, cfg, now, &mut actions);
    if !state.baselined {
        baseline(state, live, now);
        actions.push(Action::Log("baseline".into()));
        return actions;
    }
    diff_agents(state, live, now);
    queue_messages(state, live, new_msgs, now);
    if registry_changed {
        state.registry_changed(now);
    }
    state.cap_pending();
    gate(state, coordinator, turn_live, cfg, now, &mut actions);
    actions
}

/// `last_change` per pane, for every live agent including the coordinator.
fn track_last_change(state: &mut WatchState, live: &LiveData, now: u64) {
    let mut statuses = BTreeMap::new();
    for agent in &live.agents {
        if state
            .pane_status
            .get(&agent.pane_id)
            .is_some_and(|before| *before != agent.status)
        {
            state.last_change.insert(agent.pane_id.clone(), now);
        }
        statuses.insert(agent.pane_id.clone(), agent.status.clone());
    }
    state
        .last_change
        .retain(|pane, _| statuses.contains_key(pane));
    state.pane_status = statuses;
}

fn liveness(
    state: &mut WatchState,
    live: &LiveData,
    coordinator: Option<&LiveAgent>,
    cfg: &WakeCfg,
    now: u64,
    actions: &mut Vec<Action>,
) {
    if let Some(agent) = coordinator {
        state.coord_missing_since = None;
        if state.coordinator_down {
            state.coordinator_down = false;
            actions.push(Action::Log(format!(
                "coordinator back in {}",
                agent.pane_id
            )));
        }
        if is_idle(&agent.status) {
            state.coord_idle_since.get_or_insert(now);
        } else {
            state.coord_idle_since = None;
        }
        return;
    }
    state.coord_idle_since = None;
    let entry = live
        .offline
        .iter()
        .find(|entry| entry.role.as_deref() == Some(COORDINATOR_ROLE));
    let Some(entry) = entry else {
        // No coordinator registered: nothing to relaunch.
        state.coord_missing_since = None;
        return;
    };
    if !cfg.relaunch || state.coordinator_down {
        return;
    }
    let since = *state.coord_missing_since.get_or_insert(now);
    if now.saturating_sub(since) < cfg.missing_s {
        return;
    }
    state.relaunches.retain(|at| now.saturating_sub(*at) < HOUR);
    if state.relaunches.len() >= cfg.relaunch_cap_hour as usize {
        state.coordinator_down = true;
        actions.push(Action::Log(format!(
            "coordinator down: {} relaunches in the last hour; not relaunching (herdr plus coordinator start)",
            state.relaunches.len()
        )));
        return;
    }
    state.relaunches.push(now);
    state.coord_missing_since = Some(now);
    let resume = if entry.agent.as_deref() == Some("claude") {
        entry.session.clone()
    } else {
        None
    };
    actions.push(Action::Relaunch { resume });
}

/// Managed agents other than the coordinator, with their keys.
fn managed_others(live: &LiveData) -> Vec<(String, &LiveAgent)> {
    live.agents
        .iter()
        .filter(|agent| agent.managed && !agent.coordinator)
        .map(|agent| (agent_key(agent), agent))
        .collect()
}

fn seen_now(agent: &LiveAgent, now: u64) -> Seen {
    Seen {
        status: agent.status.clone(),
        pane_id: agent.pane_id.clone(),
        since: now,
        blocked_ticks: u32::from(agent.status == "blocked"),
    }
}

fn baseline(state: &mut WatchState, live: &LiveData, now: u64) {
    state.seen = managed_others(live)
        .into_iter()
        .map(|(key, agent)| {
            let mut seen = seen_now(agent, now);
            // Already blocked before the baseline: known, not news.
            seen.blocked_ticks *= 2;
            (key, seen)
        })
        .collect();
    state.baselined = true;
    state.baseline_at = now;
}

fn diff_agents(state: &mut WatchState, live: &LiveData, now: u64) {
    let current = managed_others(live);
    let current_keys: Vec<&str> = current.iter().map(|(key, _)| key.as_str()).collect();
    // An entry that learned its session id changes key: same pane, not a new agent.
    for (key, agent) in &current {
        if state.seen.contains_key(key) {
            continue;
        }
        let renamed = state
            .seen
            .iter()
            .find(|(old, seen)| {
                seen.pane_id == agent.pane_id && !current_keys.contains(&old.as_str())
            })
            .map(|(old, _)| old.clone());
        if let Some(old) = renamed {
            if let Some(seen) = state.seen.remove(&old) {
                state.seen.insert(key.clone(), seen);
            }
            for ev in &mut state.pending {
                ev.rekey(&old, key);
            }
        }
    }
    let gone: Vec<String> = state
        .seen
        .keys()
        .filter(|key| !current_keys.contains(&key.as_str()))
        .cloned()
        .collect();
    for key in gone {
        let Some(seen) = state.seen.remove(&key) else {
            continue;
        };
        // Gone from the live list but still registered: offline. Not
        // registered any more: it was unmanaged (the registry item covers that).
        let offline = live.offline.iter().any(|entry| {
            entry.session.as_deref() == Some(key.as_str())
                || entry.pane_id.as_deref() == Some(seen.pane_id.as_str())
        });
        if !offline {
            continue;
        }
        let who = state
            .pending
            .iter()
            .find_map(|ev| match ev {
                Ev::Status { key: k, who, .. } | Ev::Appeared { key: k, who, .. } if *k == key => {
                    Some(who.clone())
                }
                _ => None,
            })
            .or_else(|| {
                let entry = live.offline.iter().find(|entry| {
                    entry.session.as_deref() == Some(key.as_str())
                        || entry.pane_id.as_deref() == Some(seen.pane_id.as_str())
                })?;
                Some(Who {
                    name: entry.role.clone().unwrap_or_else(|| seen.pane_id.clone()),
                    pane_id: seen.pane_id.clone(),
                    agent: entry.agent.clone(),
                    role: entry.role.clone(),
                    project: entry.project.clone(),
                })
            })
            .unwrap_or_default();
        state.pending.push(Ev::Gone { key, who, at: now });
    }
    for (key, agent) in current {
        let who = Who::of(agent);
        let Some(seen) = state.seen.get_mut(&key) else {
            // New, or back after going offline: the return replaces a pending Gone.
            state
                .pending
                .retain(|ev| !(matches!(ev, Ev::Gone { .. }) && ev.key() == Some(key.as_str())));
            state.seen.insert(key.clone(), seen_now(agent, now));
            state.pending.push(Ev::Appeared {
                key,
                who,
                at: now,
                relinked: false,
            });
            continue;
        };
        let relinked = seen.pane_id != agent.pane_id;
        seen.pane_id = agent.pane_id.clone();
        let changed = (seen.status != agent.status).then(|| seen.status.clone());
        if changed.is_some() {
            seen.status = agent.status.clone();
            seen.since = now;
            seen.blocked_ticks = 0;
        }
        let mut confirm = None;
        if agent.status == "blocked" {
            seen.blocked_ticks += 1;
            if seen.blocked_ticks == 2 {
                confirm = Some(seen.since);
            }
        }
        if relinked {
            state.pending.push(Ev::Appeared {
                key: key.clone(),
                who: who.clone(),
                at: now,
                relinked: true,
            });
        }
        if let Some(from) = changed {
            state.status_change(&key, who.clone(), &from, &agent.status, now);
        }
        if let Some(since) = confirm {
            state.confirm_blocked(&key, who, since, now);
        }
    }
}

/// `lead→rev "please review…"`, with the outcome when it was not delivered.
fn preview(message: &AgentMessage) -> String {
    let from = message
        .from_name
        .as_deref()
        .or(message.from_pane.as_deref())
        .unwrap_or("?");
    let to = message.to_name.as_deref().unwrap_or(&message.to_pane);
    let text = message
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut short: String = text.chars().take(PREVIEW_CHARS).collect();
    if short.len() < text.len() {
        short.push('\u{2026}');
    }
    let outcome = if message.outcome == "sent" {
        String::new()
    } else {
        format!(" [{}]", message.outcome)
    };
    format!("{from}\u{2192}{to}{outcome} \"{short}\"")
}

fn queue_messages(state: &mut WatchState, live: &LiveData, new_msgs: &[AgentMessage], now: u64) {
    let coordinator = coordinator_pane(live);
    let mut to_coordinator = Vec::new();
    let mut between = Vec::new();
    let mut from_coordinator = Vec::new();
    state
        .refusal_queued
        .retain(|_, at| now.saturating_sub(*at) < REFUSAL_REPEAT_S);
    let pending_to_coordinator = state.pending.iter().any(|ev| {
        matches!(
            ev,
            Ev::Messages {
                scope: MsgScope::ToCoordinator,
                ..
            }
        )
    });
    for message in new_msgs {
        if coordinator == Some(message.to_pane.as_str()) {
            // Typed into the coordinator: it has seen that one already.
            if message.outcome != "sent" {
                // A sender retrying on `busy` wakes the coordinator once per
                // REFUSAL_REPEAT_S; repeats only ride along (they stay in the log).
                let sender = message.from_pane.clone().unwrap_or_default();
                let repeat = state
                    .refusal_queued
                    .get(&sender)
                    .is_some_and(|at| now.saturating_sub(*at) < REFUSAL_REPEAT_S);
                if !repeat {
                    state.refusal_queued.insert(sender, now);
                } else if !pending_to_coordinator && to_coordinator.is_empty() {
                    continue;
                }
                to_coordinator.push(preview(message));
            }
        } else if coordinator.is_some() && message.from_pane.as_deref() == coordinator {
            from_coordinator.push(preview(message));
        } else {
            between.push(preview(message));
        }
    }
    state.add_messages(MsgScope::ToCoordinator, to_coordinator, now);
    state.add_messages(MsgScope::Between, between, now);
    state.add_messages(MsgScope::FromCoordinator, from_coordinator, now);
}

/// Why a triggered wake-up waits (`held`/`suppressed` and the reason), if it does.
fn held_reason(
    state: &WatchState,
    coordinator: Option<&LiveAgent>,
    turn_live: bool,
    high: bool,
    cfg: &WakeCfg,
    now: u64,
) -> Option<(&'static str, String)> {
    let Some(agent) = coordinator else {
        return Some(("held", "coordinator offline".into()));
    };
    if !is_idle(&agent.status) {
        return Some(("held", format!("coordinator {}", agent.status)));
    }
    if state
        .coord_idle_since
        .is_none_or(|since| now.saturating_sub(since) < cfg.settle_s)
    {
        return Some(("held", "coordinator settling".into()));
    }
    if turn_live {
        return Some(("held", "turn live".into()));
    }
    if now < state.retry_after {
        return Some(("held", format!("retry {}s left", state.retry_after - now)));
    }
    if state.forced {
        return None;
    }
    if state.capped(cfg, now) {
        return Some(("suppressed", "capped".into()));
    }
    let gap = cfg.gap(high);
    let since = now.saturating_sub(state.last_wake);
    (state.last_wake > 0 && since < gap).then(|| ("held", format!("gap {}s left", gap - since)))
}

fn gate(
    state: &mut WatchState,
    coordinator: Option<&LiveAgent>,
    turn_live: bool,
    cfg: &WakeCfg,
    now: u64,
    actions: &mut Vec<Action>,
) {
    let lanes: Vec<(Prio, u64)> = state
        .pending
        .iter()
        .filter_map(|ev| Some((ev.prio()?, ev.first_at())))
        .collect();
    let high = lanes.iter().any(|(prio, _)| *prio == Prio::High);
    let oldest_normal = lanes
        .iter()
        .filter(|(prio, _)| *prio == Prio::Normal)
        .map(|(_, at)| *at)
        .min();
    let anything = !lanes.is_empty() || state.folded > 0;
    // The periodic check counts from the last wake-up or from this watcher's start.
    let periodic_from = state.last_wake.max(state.baseline_at);
    let triggered = state.forced
        || high
        || oldest_normal.is_some_and(|at| now.saturating_sub(at) >= cfg.debounce_s)
        || (anything && now.saturating_sub(periodic_from) >= cfg.periodic_s);
    if !triggered {
        return;
    }
    if let Some((verb, reason)) = held_reason(state, coordinator, turn_live, high, cfg, now) {
        // Once a minute per kind of reason (the gap countdown is one kind).
        let kind: String = reason.chars().take_while(|c| !c.is_ascii_digit()).collect();
        let due = state
            .held_log
            .as_ref()
            .is_none_or(|(last, at)| *last != kind || now.saturating_sub(*at) >= HELD_LOG_EVERY_S);
        if due {
            state.held_log = Some((kind, now));
            actions.push(Action::Log(format!(
                "{verb} {reason} (pending {})",
                state.reportable()
            )));
        }
        return;
    }
    let Some(coordinator) = coordinator else {
        return;
    };
    let seq = state.wake_seq + 1;
    let evs = state.digest_events();
    actions.push(Action::Wake {
        seq,
        digest: digest_markdown(seq, &evs, now),
        prompt: wake_prompt(&state.dir, seq),
        pane: coordinator.pane_id.clone(),
        items: evs.len(),
    });
}

/// The line typed into the coordinator for wake-up `seq`.
pub fn wake_prompt(dir: &Path, seq: u64) -> String {
    format!(
        "[herdr+ wake-up #{seq} \u{2014} not the user; read-only turn] If the coordinator rules are not in your context (after /clear or a compaction), Read {} first. Read {}. Re-read memory/MEMORY.md, update dashboard/board.json and memory, record proposed actions as suggestions; do not act. Use the Read/Write/Edit file tools, not shell commands (nobody may be there to approve them). If nothing material changed, reply in one line.",
        super::instructions_path(dir).display(),
        wake_dir(dir).join(format!("{seq}.md")).display()
    )
}

fn local_offset_s() -> i64 {
    crate::platform::local_datetime()
        .map(|local| local.assume_utc().unix_timestamp() - now_unix() as i64)
        .map(|seconds| (seconds as f64 / 60.0).round() as i64 * 60)
        .unwrap_or(0)
}

/// `hh:mm`, local time.
fn clock(unix: u64) -> String {
    let seconds = (unix as i64 + local_offset_s()).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// `2026-10-02T03:14:05`, local time.
fn iso(unix: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix as i64 + local_offset_s())
        .map(|at| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                at.year(),
                u8::from(at.month()),
                at.day(),
                at.hour(),
                at.minute(),
                at.second()
            )
        })
        .unwrap_or_else(|_| unix.to_string())
}

fn digest_line(ev: &Ev) -> String {
    match ev {
        Ev::Status {
            who,
            from,
            changes,
            blocked_since: Some(since),
            ..
        } => {
            let mut line = format!("- {}: BLOCKED since {}", who.label(), clock(*since));
            if from != "blocked" {
                line.push_str(&format!(" (was {from})"));
            }
            if *changes > 1 {
                line.push_str(&format!(" ({changes} changes)"));
            }
            line
        }
        Ev::Status {
            who,
            from,
            to,
            at,
            changes,
            ..
        } => {
            let mut line = format!("- {}: {from} \u{2192} {to} at {}", who.label(), clock(*at));
            if *changes > 1 {
                line.push_str(&format!(" ({changes} changes)"));
            }
            line
        }
        Ev::Appeared {
            who,
            at,
            relinked: false,
            ..
        } => format!("- {}: new managed agent at {}", who.label(), clock(*at)),
        Ev::Appeared { who, at, .. } => {
            format!(
                "- {}: now in pane {} ({})",
                who.label(),
                who.pane_id,
                clock(*at)
            )
        }
        Ev::Gone { who, at, .. } => format!("- {}: OFFLINE since {}", who.label(), clock(*at)),
        Ev::Messages {
            scope,
            count,
            preview,
            ..
        } => {
            let what = match scope {
                MsgScope::ToCoordinator => "messages to you that were not typed in",
                MsgScope::Between => "messages",
                MsgScope::FromCoordinator => "messages you sent",
            };
            let mut line = format!("- {what}: {count} new");
            for item in preview {
                line.push_str("; ");
                line.push_str(item);
            }
            line
        }
        Ev::Registry { changes, at, .. } => format!(
            "- registry changed (opt-ins, roles or notes; {changes}\u{d7}, last {})",
            clock(*at)
        ),
        Ev::Earlier { count, since } => {
            format!("- +{count} earlier changes since {}", clock(*since))
        }
    }
}

/// The wake-up digest: a header and at most 24 more lines, the last of them
/// `+N more` when the items do not fit.
pub fn digest_markdown(seq: u64, evs: &[Ev], now: u64) -> String {
    let changes: u64 = evs
        .iter()
        .map(|ev| match ev {
            Ev::Earlier { count, .. } => *count,
            _ => 1,
        })
        .sum();
    let mut out = match evs.iter().map(Ev::first_at).min() {
        Some(since) => format!(
            "# wake {seq} {} \u{2014} {changes} changes since {}\n",
            iso(now),
            clock(since)
        ),
        None => format!(
            "# wake {seq} {} \u{2014} no changes (requested check-in)\n",
            iso(now)
        ),
    };
    let room = DIGEST_MAX_LINES - 1;
    let shown = if evs.len() > room {
        room - 1
    } else {
        evs.len()
    };
    for ev in &evs[..shown] {
        out.push_str(&digest_line(ev));
        out.push('\n');
    }
    if shown < evs.len() {
        out.push_str(&format!("+{} more\n", evs.len() - shown));
    }
    out
}

// ----- the driver ---------------------------------------------------------

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_millis() as u64)
        .unwrap_or(0)
}

/// One `wakeups.log` line: `2026-10-02T03:14:05 delivered #7 5 items -> w1:p1`.
fn log_line(dir: &Path, now: u64, line: &str) {
    tracing::info!("herdr+ watcher: {line}");
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(wakeups_path(dir))
        .and_then(|mut file| file.write_all(format!("{} {line}\n", iso(now)).as_bytes()));
    if let Err(err) = result {
        tracing::warn!("herdr+ cannot write wakeups.log: {err}");
    }
}

/// Keep the newest [`DIGESTS_KEPT`] `wake/<seq>.md` files.
fn prune_digests(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(wake_dir(dir)) else {
        return;
    };
    let mut digests: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let seq = path
                .file_name()?
                .to_str()?
                .strip_suffix(".md")?
                .parse()
                .ok()?;
            Some((seq, path))
        })
        .collect();
    if digests.len() <= DIGESTS_KEPT {
        return;
    }
    digests.sort();
    for (_, path) in &digests[..digests.len() - DIGESTS_KEPT] {
        if let Err(err) = std::fs::remove_file(path) {
            tracing::warn!("herdr+ cannot prune {}: {err}", path.display());
        }
    }
}

struct Driver<A: Api> {
    api: A,
    dir: PathBuf,
    ctx: LaunchCtx,
    cfg: WakeCfg,
    interval: Duration,
    state: WatchState,
    /// Start a coordinator when none is registered (until one start succeeds).
    need_coordinator: bool,
    next_coordinator_try: u64,
    /// Newest messages for `live.json`, fed from the offset reader.
    recent: VecDeque<AgentMessage>,
    /// The last `live.json` written, `generated_unix` zeroed.
    last_live: Option<LiveData>,
    last_live_write: u64,
    last_state: Vec<u8>,
    last_rotate: u64,
    last_error_log: u64,
}

impl<A: Api> Driver<A> {
    fn new(api: A, opts: &WatchOpts, cfg: WakeCfg, mut state: WatchState) -> Self {
        // The log's tail seeds `recent`; skip what was appended while the
        // watcher was down so it is not added twice (the baseline would
        // ignore those lines anyway).
        let (_, offset) = messages::since_offset(&opts.dir, state.msg_offset);
        state.msg_offset = offset;
        Self {
            api,
            dir: opts.dir.clone(),
            ctx: opts.ctx.clone(),
            cfg,
            interval: Duration::from_millis(opts.interval_ms.max(200)),
            last_state: serde_json::to_vec_pretty(&state).unwrap_or_default(),
            state,
            need_coordinator: opts.coordinator,
            next_coordinator_try: 0,
            recent: messages::recent(&opts.dir, LIVE_MESSAGES, None).into(),
            last_live: None,
            last_live_write: 0,
            last_rotate: 0,
            last_error_log: 0,
        }
    }

    fn log(&self, now: u64, line: &str) {
        log_line(&self.dir, now, line);
    }

    /// Start a coordinator when none is registered; `true` when it started
    /// (and so wrote the registry).
    fn ensure_coordinator(&mut self, now: u64) -> bool {
        if !self.need_coordinator || now < self.next_coordinator_try {
            return false;
        }
        // A corrupt registry reads as empty here; coordinator_start refuses it.
        if Registry::load(&self.dir).coordinator().is_some() {
            self.need_coordinator = false;
            return false;
        }
        match coordinator_start(&self.api, &self.dir, &self.ctx, None) {
            Ok(pane) => {
                self.need_coordinator = false;
                self.log(now, &format!("coordinator started -> {pane}"));
                true
            }
            Err(err) => {
                self.next_coordinator_try = now + COORDINATOR_RETRY_S;
                self.log(now, &format!("coordinator start failed: {err}"));
                false
            }
        }
    }

    /// One poll; returns how long to sleep before the next.
    fn step(&mut self, now: u64) -> Duration {
        let fetched = api::agents(&self.api)
            .and_then(|agents| Ok((agents, api::workspaces(&self.api)?, api::tabs(&self.api)?)));
        let (agents, workspaces, tabs) = match fetched {
            Ok(fetched) => fetched,
            Err(err) => {
                // A server restart must not replay as events: re-baseline.
                // live.json is left alone; its age is the dashboard's signal.
                self.state.rebaseline();
                if now.saturating_sub(self.last_error_log) >= HELD_LOG_EVERY_S {
                    self.last_error_log = now;
                    self.log(now, &format!("server unavailable: {err}"));
                }
                return ERROR_BACKOFF;
            }
        };
        let inputs = live::Inputs {
            agents: &agents,
            workspaces: &workspaces,
            tabs: &tabs,
        };
        let registry_file = registry_path(&self.dir);
        let observed_mtime = mtime_ms(&registry_file);
        let mut registry_changed = observed_mtime != self.state.registry_mtime_ms;
        // The mtime to remember: the one observed, unless our own write below
        // replaced it (a coordinator registered here is our own write).
        let mut registry_mtime = observed_mtime;
        if self.ensure_coordinator(now) {
            registry_mtime = mtime_ms(&registry_file);
        }
        let last_change: HashMap<String, u64> = self
            .state
            .last_change
            .iter()
            .map(|(pane, at)| (pane.clone(), *at))
            .collect();
        let mut registry = Registry::load(&self.dir);
        let (mut live, relinked) =
            live::build(&inputs, &mut registry, Vec::new(), &last_change, false, now);
        if relinked {
            // Relink under the lock, over the freshly loaded registry. A write
            // by someone else since the check above is still a change.
            let result = registry::update(&self.dir, |locked| {
                let foreign = mtime_ms(&registry_file) != registry_mtime;
                let wrote = live::build(&inputs, locked, Vec::new(), &last_change, false, now).1;
                Ok((wrote, foreign))
            });
            match result {
                Ok((wrote, foreign)) => {
                    registry_changed |= foreign;
                    if wrote {
                        // Our own relink write is not a registry change.
                        registry_mtime = mtime_ms(&registry_file);
                    }
                }
                Err(err) => self.log(now, &format!("relink not saved: {err}")),
            }
        }
        self.state.registry_mtime_ms = registry_mtime;

        let (new_msgs, offset) = messages::since_offset(&self.dir, self.state.msg_offset);
        self.state.msg_offset = offset;
        // Right after reading; a line appended since keeps the log for a later rotation.
        if now.saturating_sub(self.last_rotate) >= ROTATE_EVERY_S {
            self.last_rotate = now;
            match messages::rotate_if_large(&self.dir, ROTATE_BYTES, offset) {
                Ok(true) => {
                    self.state.msg_offset = 0;
                    self.log(now, "rotated messages.jsonl");
                }
                Ok(false) => {}
                Err(err) => tracing::warn!("herdr+ cannot rotate the message log: {err}"),
            }
        }
        self.recent.extend(new_msgs.iter().cloned());
        while self.recent.len() > LIVE_MESSAGES {
            self.recent.pop_front();
        }

        let turn = turn::read_live(&self.dir, now);
        let request = wake_request_path(&self.dir);
        let wake_requested = request.exists();
        if wake_requested {
            if let Err(err) = std::fs::remove_file(&request) {
                tracing::warn!("herdr+ cannot remove wake.request: {err}");
            }
            self.log(now, "wake requested");
        }

        let actions = tick(
            &mut self.state,
            &live,
            &new_msgs,
            registry_changed,
            turn.as_ref(),
            wake_requested,
            &self.cfg,
            now,
        );
        let mut turn_live = turn.is_some();
        for action in actions {
            match action {
                Action::Log(line) => self.log(now, &line),
                // Both only touch the turn read above: an MCP server may
                // have started a new one since.
                Action::MarkTurnWorking => {
                    if let Some(turn) = &turn {
                        if let Err(err) = turn::mark_working_if(&self.dir, turn) {
                            tracing::warn!("herdr+ cannot update the turn marker: {err}");
                        }
                    }
                }
                Action::ClearTurn => {
                    if let Some(turn) = &turn {
                        turn::clear_if(&self.dir, turn);
                    }
                    turn_live = turn::read_live(&self.dir, now).is_some();
                }
                Action::Relaunch { resume } => self.relaunch(resume.as_deref(), now),
                Action::Wake {
                    seq,
                    digest,
                    prompt,
                    pane,
                    items,
                } => {
                    if self.wake(seq, &digest, &prompt, &pane, items, now) {
                        turn_live = true;
                    }
                }
            }
        }

        for agent in &mut live.agents {
            agent.last_change_unix = self
                .state
                .last_change
                .get(&agent.pane_id)
                .copied()
                .unwrap_or(0);
        }
        live.messages = self.recent.iter().cloned().collect();
        live.watch = self.state.summary(&self.cfg, turn_live, now);
        self.publish(live, now);
        self.persist();
        self.interval
    }

    fn relaunch(&mut self, resume: Option<&str>, now: u64) {
        let line = match coordinator_start(&self.api, &self.dir, &self.ctx, resume) {
            Ok(pane) => format!("relaunch resume={} -> {pane}", resume.unwrap_or("none")),
            Err(err) => format!("relaunch failed: {err}"),
        };
        // The start can take a while (the resume check): count the missing
        // time from now, or the next tick would launch a second coordinator
        // before this one shows up in agent.list.
        self.state.coord_missing_since = Some(now_unix().max(now));
        self.log(now, &line);
    }

    /// Deliver one wake-up; `true` when the prompt went in.
    fn wake(
        &mut self,
        seq: u64,
        digest: &str,
        prompt: &str,
        pane: &str,
        items: usize,
        now: u64,
    ) -> bool {
        // Re-check right before typing: the user may have just started a turn.
        match api::agent_get(&self.api, pane) {
            Ok(agent) if agent["agent_status"].as_str().is_some_and(is_idle) => {}
            Ok(agent) => {
                let status = agent["agent_status"].as_str().unwrap_or("unknown");
                self.log(now, &format!("held coordinator {status} (re-check)"));
                return false;
            }
            Err(err) => {
                self.state.wake_failed(now, &self.cfg);
                self.log(now, &format!("failed #{seq} {}", err.code));
                return false;
            }
        }
        let path = wake_dir(&self.dir).join(format!("{seq}.md"));
        if let Err(err) = write_atomically(&path, digest.as_bytes()) {
            self.state.wake_failed(now, &self.cfg);
            self.log(now, &format!("failed #{seq} digest not written: {err}"));
            return false;
        }
        prune_digests(&self.dir);
        let marker = Turn {
            source: "wake".into(),
            id: seq.to_string(),
            started_unix: now,
            coordinator_pane: pane.to_string(),
            seen_working: false,
        };
        // Without the marker the coordinator's write tools are not guarded
        // during this turn: no marker, no wake-up. An agent message may have
        // taken the turn since the tick read it: hold, it is not a failure.
        match turn::write_if_absent(&self.dir, &marker, now) {
            Ok(true) => {}
            Ok(false) => {
                self.log(now, "held turn live (re-check)");
                return false;
            }
            Err(err) => {
                self.state.wake_failed(now, &self.cfg);
                self.log(now, &format!("failed #{seq} turn marker: {err}"));
                return false;
            }
        }
        match api::prompt(&self.api, pane, prompt) {
            Ok(()) => {
                self.state.wake_delivered(seq, now);
                self.log(now, &format!("delivered #{seq} {items} items -> {pane}"));
                true
            }
            Err(err) => {
                turn::clear_if(&self.dir, &marker);
                self.state.wake_failed(now, &self.cfg);
                self.log(now, &format!("failed #{seq} {}", err.code));
                false
            }
        }
    }

    /// Write `live.json` when it changed (ignoring the timestamp) or every 10 s.
    fn publish(&mut self, live: LiveData, now: u64) {
        let comparable = LiveData {
            generated_unix: 0,
            ..live.clone()
        };
        if self.last_live.as_ref() == Some(&comparable)
            && now.saturating_sub(self.last_live_write) < LIVE_REFRESH_S
        {
            return;
        }
        let written = serde_json::to_vec_pretty(&live)
            .map_err(io::Error::other)
            .and_then(|json| write_atomically(&super::live_path(&self.dir), &json));
        match written {
            Ok(()) => {
                self.last_live = Some(comparable);
                self.last_live_write = now;
            }
            Err(err) => tracing::warn!("herdr+ cannot write live.json: {err}"),
        }
    }

    /// Save `watch_state.json` when it changed.
    fn persist(&mut self) {
        let Ok(json) = serde_json::to_vec_pretty(&self.state) else {
            return;
        };
        if json == self.last_state {
            return;
        }
        match self.state.save() {
            Ok(()) => self.last_state = json,
            Err(err) => tracing::warn!("herdr+ cannot write watch_state.json: {err}"),
        }
    }
}

/// Open the dashboard in herdr's browser; best effort.
fn open_dashboard(api: &impl Api, port: u16) {
    let result = api.call(Method::BrowserRun(BrowserRunParams {
        caller: None,
        profile: None,
        tab: None,
        op: BrowserOp::Open {
            url: format!("http://127.0.0.1:{port}/"),
            focus: false,
            wait: None,
        },
        timeout_ms: Some(10_000),
    }));
    if let Err(err) = result {
        tracing::info!("herdr+ dashboard not opened in the browser: {err}");
    }
}

pub fn run<A: Api>(api: A, opts: WatchOpts) -> io::Result<i32> {
    let dir = opts.dir.clone();
    let Some(_lock) = super::lock::try_exclusive(&dir, WATCH_LOCK)? else {
        println!("watcher already running ({})", dir.display());
        return Ok(0);
    };
    super::seed(&dir)?;
    launch::write_claude_mcp_config(&opts.ctx)?;
    let now = now_unix();
    if opts.serve {
        let (serve_dir, port) = (dir.clone(), opts.port);
        std::thread::spawn(move || {
            if let Err(err) = super::serve::serve(serve_dir.clone(), port) {
                tracing::warn!("herdr+ dashboard server stopped: {err}");
                log_line(
                    &serve_dir,
                    now_unix(),
                    &format!("dashboard on port {port} failed: {err}"),
                );
            }
        });
    }
    let mut cfg = WakeCfg::from_env();
    cfg.relaunch = opts.coordinator;
    let mut state = WatchState::load(&dir);
    state.rebaseline();
    // A restarted watcher retries a coordinator the relaunch cap gave up on.
    state.coordinator_down = false;
    println!(
        "herdr+ watcher: {} · dashboard http://127.0.0.1:{}/ · log {}",
        dir.display(),
        opts.port,
        wakeups_path(&dir).display()
    );
    log_line(
        &dir,
        now,
        &format!(
            "watcher started (debounce {}s, gap {}s, periodic {}s, caps {}/h {}/day)",
            cfg.debounce_s, cfg.gap_s, cfg.periodic_s, cfg.cap_hour, cfg.cap_day
        ),
    );
    let mut driver = Driver::new(api, &opts, cfg, state);
    driver.ensure_coordinator(now);
    if opts.serve {
        open_dashboard(&driver.api, opts.port);
    }
    loop {
        let wait = driver.step(now_unix());
        std::thread::sleep(wait);
    }
}

pub fn coordinator_start<A: Api>(
    api: &A,
    dir: &Path,
    ctx: &LaunchCtx,
    resume: Option<&str>,
) -> Result<String, String> {
    coordinator_start_with(api, dir, ctx, resume, &std::thread::sleep)
}

/// Whether `pane` (`{workspace}:p{n}`) belongs to `workspace_id`.
fn in_group(pane: &str, workspace_id: &str) -> bool {
    pane.strip_prefix(workspace_id)
        .is_some_and(|rest| rest.starts_with(":p"))
}

fn session_id(session: &ClaudeSession) -> &str {
    match session {
        ClaudeSession::New(id) | ClaudeSession::Resume(id) => id,
    }
}

fn start_claude(
    api: &impl Api,
    dir: &Path,
    ctx: &LaunchCtx,
    pane: &str,
    session: &ClaudeSession,
    sleep: &dyn Fn(Duration),
) -> Result<(), ApiError> {
    let kickoff = launch::coordinator_kickoff(dir);
    let args = launch::claude_args(ctx, session, true, Some(&kickoff))
        .map_err(|err| ApiError::new("mcp_config", err.to_string()))?;
    api::agent_start_with(
        api,
        COORDINATOR_NAME,
        "claude",
        pane,
        args,
        START_TIMEOUT_MS,
        sleep,
    )
    .map(|_| ())
}

/// Poll the pane for up to [`RESUME_CHECK_S`] until a live agent shows up.
fn agent_comes_up(api: &impl Api, pane: &str, sleep: &dyn Fn(Duration)) -> bool {
    for _ in 0..RESUME_CHECK_S {
        sleep(Duration::from_secs(1));
        if let Ok(agent) = api::agent_get(api, pane) {
            if matches!(
                agent["agent_status"].as_str(),
                Some("idle" | "working" | "blocked" | "done")
            ) {
                return true;
            }
        }
    }
    false
}

/// Find or create the `herdr+` group, start the coordinator Claude session
/// there (reusing its old pane when that is back at a shell), and register it.
pub(crate) fn coordinator_start_with(
    api: &impl Api,
    dir: &Path,
    ctx: &LaunchCtx,
    resume: Option<&str>,
    sleep: &dyn Fn(Duration),
) -> Result<String, String> {
    // Refuse before launching anything when the registry cannot be updated
    // afterwards: an unregistered coordinator would be started again and again.
    let known = match registry::load_strict(dir) {
        Ok(known) => known,
        Err(registry::LoadError::Corrupt(_)) => {
            return Err("managed.json is corrupt; fix or remove it".into())
        }
        Err(registry::LoadError::Io(err)) => {
            return Err(format!("cannot read managed.json: {err}"))
        }
    };
    let cwd = dir.to_string_lossy().into_owned();
    let workspaces = api::workspaces(api).map_err(|err| format!("cannot list groups: {err}"))?;
    let (workspace, fresh_root) = match api::group_by_label_or_id(&workspaces, COORDINATOR_GROUP) {
        Some(workspace) => (workspace, None),
        None => {
            let (workspace, tab, root) = api::workspace_create(api, COORDINATOR_GROUP, Some(&cwd))
                .map_err(|err| format!("cannot create the herdr+ group: {err}"))?;
            if let Err(err) = api::tab_rename(api, &tab, COORDINATOR_TAB) {
                tracing::warn!("herdr+ cannot label the coordinator tab: {err}");
            }
            (workspace, Some(root))
        }
    };
    let mut session = match resume {
        Some(id) => ClaudeSession::Resume(id.to_string()),
        None => ClaudeSession::New(launch::new_uuid()),
    };
    // Reuse the old pane only inside the herdr+ group: after a server restore
    // pane ids are remapped, and the stored id may now be a user's shell.
    let reusable = known
        .coordinator()
        .and_then(|entry| entry.pane_id.clone())
        .filter(|pane| in_group(pane, &workspace) && Some(pane) != fresh_root.as_ref());
    let mut pane = None;
    if let Some(old) = reusable {
        match start_claude(api, dir, ctx, &old, &session, sleep) {
            Ok(()) => pane = Some(old),
            Err(err)
                if matches!(
                    err.code.as_str(),
                    "agent_pane_not_found" | "agent_pane_busy" | "agent_pane_unavailable"
                ) =>
            {
                tracing::info!("herdr+ coordinator pane {old} not reusable: {err}");
            }
            Err(err) => return Err(format!("cannot start the coordinator in {old}: {err}")),
        }
    }
    let pane = match pane {
        Some(pane) => pane,
        None => {
            let pane = match fresh_root {
                Some(root) => root,
                None => {
                    api::tab_create(api, Some(&workspace), Some(&cwd), Some(COORDINATOR_TAB))
                        .map_err(|err| format!("cannot open the coordinator tab: {err}"))?
                        .1
                }
            };
            start_claude(api, dir, ctx, &pane, &session, sleep)
                .map_err(|err| format!("cannot start the coordinator in {pane}: {err}"))?;
            pane
        }
    };
    if matches!(session, ClaudeSession::Resume(_)) && !agent_comes_up(api, &pane, sleep) {
        // The resume failed (unknown or broken session): start fresh in the same pane.
        tracing::info!("herdr+ coordinator resume did not come up; starting a new session");
        session = ClaudeSession::New(launch::new_uuid());
        start_claude(api, dir, ctx, &pane, &session, sleep)
            .map_err(|err| format!("cannot restart the coordinator in {pane}: {err}"))?;
    }
    let id = session_id(&session).to_string();
    registry::update(dir, |registry| {
        match registry.coordinator_index() {
            // A fresh session differs from the stored one, so `find` would
            // refuse the pane: overwrite the keys instead.
            Some(index) => {
                registry.set_keys(index, Some(&id), Some(&pane));
            }
            None => {
                registry.manage(
                    Some(&id),
                    Some(&pane),
                    Some("claude"),
                    &ManagePatch {
                        role: Some(COORDINATOR_ROLE.into()),
                        project: Some(COORDINATOR_GROUP.into()),
                        note: None,
                    },
                )?;
            }
        }
        Ok(())
    })?;
    Ok(pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plus::live::OfflineAgent;
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn agent(pane: &str, name: &str, status: &str) -> LiveAgent {
        LiveAgent {
            name: name.into(),
            pane_id: pane.into(),
            workspace_id: pane[..2].into(),
            agent: Some("claude".into()),
            status: status.into(),
            session: Some(format!("s-{name}")),
            managed: true,
            ..LiveAgent::default()
        }
    }

    fn coord(status: &str) -> LiveAgent {
        LiveAgent {
            coordinator: true,
            role: Some(COORDINATOR_ROLE.into()),
            ..agent("w1:p1", "coordinator", status)
        }
    }

    fn live(agents: Vec<LiveAgent>) -> LiveData {
        LiveData {
            coordinator_pane: agents
                .iter()
                .find(|agent| agent.coordinator)
                .map(|agent| agent.pane_id.clone()),
            agents,
            ..LiveData::default()
        }
    }

    fn cfg() -> WakeCfg {
        WakeCfg::default()
    }

    fn quiet(state: &mut WatchState, data: &LiveData, cfg: &WakeCfg, now: u64) -> Vec<Action> {
        tick(state, data, &[], false, None, false, cfg, now)
    }

    fn wakes(actions: &[Action]) -> Vec<(u64, usize)> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Wake { seq, items, .. } => Some((*seq, *items)),
                _ => None,
            })
            .collect()
    }

    fn logs(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Log(line) => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Fire and deliver whatever wakes at `now`; the delivered seq.
    fn deliver(state: &mut WatchState, actions: &[Action], now: u64) -> Option<u64> {
        let (seq, _) = *wakes(actions).first()?;
        state.wake_delivered(seq, now);
        Some(seq)
    }

    fn message(from: &str, to: &str, outcome: &str) -> AgentMessage {
        AgentMessage {
            unix: 1,
            from_pane: Some(from.into()),
            to_pane: to.into(),
            text: "please review the branch".into(),
            outcome: outcome.into(),
            ..AgentMessage::default()
        }
    }

    #[test]
    fn the_first_tick_only_takes_a_baseline() {
        let mut state = WatchState::default();
        let data = live(vec![
            coord("idle"),
            agent("w2:p1", "a", "working"),
            agent("w2:p2", "b", "blocked"),
        ]);
        let actions = tick(
            &mut state,
            &data,
            &[message("w2:p1", "w2:p2", "sent")],
            true,
            None,
            false,
            &cfg(),
            100,
        );
        assert_eq!(actions, vec![Action::Log("baseline".into())]);
        assert!(state.pending.is_empty());
        // Unchanged afterwards: nothing to report.
        assert!(quiet(&mut state, &data, &cfg(), 102).is_empty());
        assert!(state.pending.is_empty());
    }

    #[test]
    fn the_coordinators_own_transitions_never_enqueue() {
        let mut state = WatchState::default();
        let cfg = cfg();
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 0);
        for (at, status) in [
            (10, "working"),
            (20, "idle"),
            (30, "blocked"),
            (40, "blocked"),
        ] {
            quiet(&mut state, &live(vec![coord(status)]), &cfg, at);
        }
        assert!(state.pending.is_empty());
        // Its card still shows when it last changed.
        assert_eq!(state.last_change.get("w1:p1"), Some(&30));
        let actions = quiet(&mut state, &live(vec![coord("idle")]), &cfg, 5000);
        assert!(wakes(&actions).is_empty());
    }

    #[test]
    fn two_agents_finishing_within_the_debounce_give_one_wake() {
        let mut state = WatchState::default();
        let cfg = cfg();
        let at = |a: &str, b: &str| {
            live(vec![
                coord("idle"),
                agent("w2:p1", "a", a),
                agent("w2:p2", "b", b),
            ])
        };
        quiet(&mut state, &at("working", "working"), &cfg, 0);
        assert!(quiet(&mut state, &at("idle", "working"), &cfg, 10).is_empty());
        assert!(quiet(&mut state, &at("idle", "idle"), &cfg, 20).is_empty());
        assert!(quiet(&mut state, &at("idle", "idle"), &cfg, 69).is_empty());
        let actions = quiet(&mut state, &at("idle", "idle"), &cfg, 70);
        assert_eq!(wakes(&actions), vec![(1, 2)]);
        let Some(Action::Wake {
            digest,
            prompt,
            pane,
            ..
        }) = actions.first()
        else {
            panic!("wake expected: {actions:?}");
        };
        assert_eq!(pane, "w1:p1");
        assert!(digest.starts_with("# wake 1 "), "{digest}");
        assert!(digest.contains("working \u{2192} idle"), "{digest}");
        assert!(prompt.starts_with("[herdr+ wake-up #1 "), "{prompt}");
        assert!(prompt.contains("1.md"), "{prompt}");
        assert!(prompt.contains("coordinator.md first"), "{prompt}");
        deliver(&mut state, &actions, 70);
        assert!(state.pending.is_empty());
        assert!(quiet(&mut state, &at("idle", "idle"), &cfg, 200).is_empty());
    }

    #[test]
    fn the_gap_is_respected() {
        let mut state = WatchState::default();
        let cfg = cfg();
        let one = live(vec![coord("idle"), agent("w2:p1", "a", "idle")]);
        let two = live(vec![
            coord("idle"),
            agent("w2:p1", "a", "idle"),
            agent("w2:p2", "c", "idle"),
        ]);
        quiet(&mut state, &one, &cfg, 0);
        state.wake_delivered(1, 70);
        quiet(&mut state, &two, &cfg, 80); // c appears (normal)
        let held = quiet(&mut state, &two, &cfg, 140);
        assert!(wakes(&held).is_empty());
        assert_eq!(logs(&held), vec!["held gap 50s left (pending 1)"]);
        // The countdown is logged once a minute, not every tick.
        assert!(quiet(&mut state, &two, &cfg, 189).is_empty());
        assert_eq!(wakes(&quiet(&mut state, &two, &cfg, 190)), vec![(2, 1)]);
    }

    #[test]
    fn blocked_takes_the_high_lane_once_confirmed() {
        let mut state = WatchState::default();
        let cfg = cfg();
        let with = |status: &str| live(vec![coord("idle"), agent("w2:p1", "a", status)]);
        quiet(&mut state, &with("working"), &cfg, 0);
        state.wake_delivered(1, 100);
        // One tick blocked is not confirmed yet.
        assert!(quiet(&mut state, &with("blocked"), &cfg, 110).is_empty());
        assert_eq!(state.pending[0].prio(), Some(Prio::Low));
        quiet(&mut state, &with("blocked"), &cfg, 112);
        assert_eq!(state.pending[0].prio(), Some(Prio::High));
        let held = quiet(&mut state, &with("blocked"), &cfg, 144);
        assert!(wakes(&held).is_empty(), "{held:?}");
        let actions = quiet(&mut state, &with("blocked"), &cfg, 145);
        assert_eq!(wakes(&actions), vec![(2, 1)]);
        let Some(Action::Wake { digest, .. }) = actions.first() else {
            panic!("wake expected");
        };
        assert!(digest.contains("BLOCKED since"), "{digest}");
        // A short normal gap also shortens the high lane.
        let short = WakeCfg {
            gap_s: 20,
            ..WakeCfg::default()
        };
        assert_eq!(short.gap(true), 20);
        assert_eq!(cfg.gap(true), 45);
    }

    #[test]
    fn hourly_and_daily_caps_suppress_wakes() {
        let cfg = WakeCfg {
            debounce_s: 0,
            gap_s: 0,
            cap_hour: 2,
            ..WakeCfg::default()
        };
        let mut state = WatchState::default();
        let mut agents = vec![coord("idle")];
        quiet(&mut state, &live(agents.clone()), &cfg, 0);
        let mut delivered = 0;
        for (i, now) in [100u64, 200, 300].into_iter().enumerate() {
            agents.push(agent(&format!("w2:p{i}"), &format!("n{i}"), "idle"));
            let actions = quiet(&mut state, &live(agents.clone()), &cfg, now);
            if deliver(&mut state, &actions, now).is_some() {
                delivered += 1;
            } else {
                assert_eq!(logs(&actions), vec!["suppressed capped (pending 1)"]);
            }
        }
        assert_eq!(delivered, 2);
        assert!(state.summary(&cfg, false, 300).capped);
        // An hour later the hourly cap frees up.
        assert_eq!(
            wakes(&quiet(&mut state, &live(agents.clone()), &cfg, 3800)).len(),
            1
        );

        let daily = WakeCfg {
            cap_day: 3,
            ..cfg.clone()
        };
        let mut state = WatchState::default();
        quiet(&mut state, &live(vec![coord("idle")]), &daily, 20_000);
        state.wakes = vec![10_000, 13_000, 16_000];
        state.last_wake = 16_000;
        let actions = quiet(
            &mut state,
            &live(vec![coord("idle"), agent("w2:p1", "a", "idle")]),
            &daily,
            20_010,
        );
        assert!(wakes(&actions).is_empty());
        assert!(state.capped(&daily, 20_010));
    }

    #[test]
    fn the_periodic_check_fires_only_with_something_pending() {
        let cfg = WakeCfg {
            periodic_s: 300,
            ..WakeCfg::default()
        };
        let mut state = WatchState::default();
        let data = live(vec![coord("idle")]);
        quiet(&mut state, &data, &cfg, 0);
        // Nothing pending: no periodic wake, ever.
        assert!(quiet(&mut state, &data, &cfg, 1000).is_empty());
        // A low item (a registry change) waits for the periodic check.
        let mut state = WatchState::default();
        quiet(&mut state, &data, &cfg, 0);
        tick(&mut state, &data, &[], true, None, false, &cfg, 10);
        assert_eq!(state.pending[0].prio(), Some(Prio::Low));
        assert!(quiet(&mut state, &data, &cfg, 299).is_empty());
        assert_eq!(wakes(&quiet(&mut state, &data, &cfg, 300)), vec![(1, 1)]);
    }

    #[test]
    fn no_wake_while_the_coordinator_is_busy_unsettled_or_in_a_turn() {
        let cfg = cfg();
        let request = |state: &mut WatchState, data: &LiveData, turn: Option<&Turn>, now| {
            tick(state, data, &[], false, turn, true, &cfg, now)
        };
        let mut state = WatchState::default();
        quiet(&mut state, &live(vec![coord("working")]), &cfg, 0);
        let actions = request(&mut state, &live(vec![coord("working")]), None, 10);
        assert!(wakes(&actions).is_empty());
        assert_eq!(logs(&actions), vec!["held coordinator working (pending 0)"]);
        // Idle, but not for 5 s yet.
        let actions = request(&mut state, &live(vec![coord("idle")]), None, 20);
        assert_eq!(
            logs(&actions),
            vec!["held coordinator settling (pending 0)"]
        );
        assert!(quiet(&mut state, &live(vec![coord("idle")]), &cfg, 24).is_empty());
        // A live turn holds it too.
        let turn = Turn {
            source: "message".into(),
            id: "m1".into(),
            started_unix: 20,
            coordinator_pane: "w1:p1".into(),
            seen_working: false,
        };
        let actions = request(&mut state, &live(vec![coord("idle")]), Some(&turn), 25);
        assert!(wakes(&actions).is_empty());
        assert_eq!(logs(&actions), vec!["held turn live (pending 0)"]);
        // The coordinator offline holds it as well.
        let actions = quiet(&mut state, &live(vec![]), &cfg, 26);
        assert!(wakes(&actions).is_empty());
        // The request survives all of that and fires once possible.
        assert!(state.forced);
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 30);
        assert_eq!(
            wakes(&quiet(&mut state, &live(vec![coord("idle")]), &cfg, 35)),
            vec![(1, 0)]
        );
    }

    #[test]
    fn a_turn_marker_is_advanced_with_the_coordinators_status() {
        let cfg = cfg();
        let mut state = WatchState::default();
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 0);
        let turn = Turn {
            source: "wake".into(),
            id: "1".into(),
            started_unix: 0,
            coordinator_pane: "w1:p1".into(),
            seen_working: false,
        };
        let actions = tick(
            &mut state,
            &live(vec![coord("working")]),
            &[],
            false,
            Some(&turn),
            false,
            &cfg,
            5,
        );
        assert_eq!(actions, vec![Action::MarkTurnWorking]);
        let worked = Turn {
            seen_working: true,
            ..turn
        };
        let actions = tick(
            &mut state,
            &live(vec![coord("idle")]),
            &[],
            false,
            Some(&worked),
            false,
            &cfg,
            9,
        );
        assert_eq!(actions, vec![Action::ClearTurn]);
    }

    #[test]
    fn a_requested_wake_bypasses_gap_and_caps_but_not_idle() {
        let cfg = WakeCfg {
            cap_hour: 1,
            ..WakeCfg::default()
        };
        let mut state = WatchState::default();
        quiet(&mut state, &live(vec![coord("working")]), &cfg, 0);
        state.wake_delivered(4, 1);
        assert!(state.capped(&cfg, 10));
        let actions = tick(
            &mut state,
            &live(vec![coord("working")]),
            &[],
            false,
            None,
            true,
            &cfg,
            10,
        );
        assert!(wakes(&actions).is_empty());
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 11);
        let actions = quiet(&mut state, &live(vec![coord("idle")]), &cfg, 16);
        assert_eq!(wakes(&actions), vec![(5, 0)]);
        let Some(Action::Wake { digest, .. }) = actions.first() else {
            panic!("wake expected");
        };
        assert!(digest.contains("no changes"), "{digest}");
        deliver(&mut state, &actions, 16);
        assert!(!state.forced);
        assert_eq!(state.wake_seq, 5);
    }

    #[test]
    fn a_failed_prompt_keeps_pending_and_backs_off() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let with = |status: &str| live(vec![coord("idle"), agent("w2:p1", "a", status)]);
        quiet(&mut state, &with("working"), &cfg, 0);
        quiet(&mut state, &with("idle"), &cfg, 10);
        let actions = quiet(&mut state, &with("idle"), &cfg, 70);
        assert_eq!(wakes(&actions), vec![(1, 1)]);
        state.wake_failed(70, &cfg);
        assert_eq!(state.last_wake, 0);
        assert_eq!(state.wake_seq, 0);
        assert_eq!(state.pending.len(), 1);
        let held = quiet(&mut state, &with("idle"), &cfg, 71);
        assert_eq!(logs(&held), vec!["held retry 119s left (pending 1)"]);
        // The same wake-up is retried after the gap.
        assert_eq!(
            wakes(&quiet(&mut state, &with("idle"), &cfg, 190)),
            vec![(1, 1)]
        );
    }

    #[test]
    fn a_missing_coordinator_is_relaunched_up_to_the_cap() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let mut gone = live(vec![]);
        gone.offline.push(OfflineAgent {
            pane_id: Some("w1:p1".into()),
            session: Some("cs".into()),
            agent: Some("claude".into()),
            role: Some(COORDINATOR_ROLE.into()),
            project: None,
        });
        let relaunches = |actions: &[Action]| {
            actions
                .iter()
                .filter(|action| matches!(action, Action::Relaunch { .. }))
                .count()
        };
        quiet(&mut state, &gone, &cfg, 0);
        assert_eq!(relaunches(&quiet(&mut state, &gone, &cfg, 29)), 0);
        let actions = quiet(&mut state, &gone, &cfg, 30);
        assert_eq!(
            actions,
            vec![Action::Relaunch {
                resume: Some("cs".into())
            }]
        );
        assert_eq!(relaunches(&quiet(&mut state, &gone, &cfg, 59)), 0);
        assert_eq!(relaunches(&quiet(&mut state, &gone, &cfg, 60)), 1);
        assert_eq!(relaunches(&quiet(&mut state, &gone, &cfg, 90)), 1);
        let actions = quiet(&mut state, &gone, &cfg, 120);
        assert_eq!(relaunches(&actions), 0);
        assert!(logs(&actions)[0].starts_with("coordinator down"));
        assert!(state.coordinator_down);
        assert!(state.summary(&cfg, false, 120).coordinator_down);
        assert!(
            quiet(&mut state, &gone, &cfg, 5000).is_empty(),
            "logged once, stays down"
        );
        // Back (started by hand): no longer down.
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 5010);
        assert!(!state.coordinator_down);
        // --no-coordinator never relaunches.
        let off = WakeCfg {
            relaunch: false,
            ..WakeCfg::default()
        };
        let mut state = WatchState::default();
        quiet(&mut state, &gone, &off, 0);
        assert_eq!(relaunches(&quiet(&mut state, &gone, &off, 100)), 0);
    }

    #[test]
    fn persisted_state_round_trips_without_rebaselining_counters() {
        let dir = crate::plus::test_dir("watch-state");
        let cfg = cfg();
        let mut state = WatchState::load(&dir);
        assert_eq!(state.dir, dir);
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 0);
        tick(
            &mut state,
            &live(vec![coord("idle")]),
            &[],
            true,
            None,
            false,
            &cfg,
            10,
        );
        state.wake_delivered(3, 20);
        state.msg_offset = 1234;
        tick(
            &mut state,
            &live(vec![coord("idle")]),
            &[],
            true,
            None,
            false,
            &cfg,
            30,
        );
        state.save().unwrap();
        let mut loaded = WatchState::load(&dir);
        assert_eq!(loaded.wake_seq, 3);
        assert_eq!(loaded.wakes, vec![20]);
        assert_eq!(loaded.msg_offset, 1234);
        assert_eq!(loaded.pending, state.pending);
        // A reload takes a new baseline of the agents, not of the counters.
        let actions = quiet(&mut loaded, &live(vec![coord("idle")]), &cfg, 40);
        assert_eq!(logs(&actions), vec!["baseline"]);
        assert_eq!(loaded.wake_seq, 3);
        assert_eq!(loaded.pending.len(), 1);
        // A corrupt file starts from the default.
        std::fs::write(crate::plus::watch_state_path(&dir), "{oops").unwrap();
        assert_eq!(WatchState::load(&dir).wake_seq, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flaps_that_end_where_they_started_are_dropped_below_four_changes() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let with = |status: &str| live(vec![coord("idle"), agent("w2:p1", "a", status)]);
        quiet(&mut state, &with("idle"), &cfg, 0);
        quiet(&mut state, &with("working"), &cfg, 10);
        quiet(&mut state, &with("idle"), &cfg, 20);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].prio(), None);
        assert!(quiet(&mut state, &with("idle"), &cfg, 4000).is_empty());
        quiet(&mut state, &with("working"), &cfg, 4010);
        quiet(&mut state, &with("idle"), &cfg, 4020);
        let Ev::Status {
            from, to, changes, ..
        } = &state.pending[0]
        else {
            panic!("status item expected");
        };
        assert_eq!((from.as_str(), to.as_str(), *changes), ("idle", "idle", 4));
        assert_eq!(state.pending[0].prio(), Some(Prio::Normal));
        assert_eq!(
            wakes(&quiet(&mut state, &with("idle"), &cfg, 4080)),
            vec![(1, 1)]
        );
    }

    #[test]
    fn offline_unmanaged_relinked_and_rekeyed_agents() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let a = agent("w2:p1", "a", "idle");
        let b = agent("w2:p2", "b", "idle");
        let pane_only = LiveAgent {
            session: None,
            ..agent("w2:p3", "c", "idle")
        };
        quiet(
            &mut state,
            &live(vec![coord("idle"), a.clone(), b.clone(), pane_only.clone()]),
            &cfg,
            0,
        );
        // a goes offline (still registered); b was unmanaged; c learns its
        // session id; and a moves panes when it comes back.
        let mut next = live(vec![
            coord("idle"),
            LiveAgent {
                session: Some("s-c".into()),
                ..pane_only
            },
        ]);
        next.offline.push(OfflineAgent {
            session: Some("s-a".into()),
            pane_id: Some("w2:p1".into()),
            ..OfflineAgent::default()
        });
        quiet(&mut state, &next, &cfg, 10);
        assert_eq!(state.pending.len(), 1, "{:?}", state.pending);
        assert!(matches!(&state.pending[0], Ev::Gone { key, .. } if key == "s-a"));
        assert_eq!(state.pending[0].prio(), Some(Prio::High));
        let moved = LiveAgent {
            pane_id: "w3:p1".into(),
            ..a
        };
        quiet(
            &mut state,
            &live(vec![coord("idle"), moved.clone()]),
            &cfg,
            11,
        );
        assert!(matches!(
            &state.pending[..],
            [Ev::Appeared {
                relinked: false,
                ..
            }]
        ));
        state.pending.clear();
        quiet(
            &mut state,
            &live(vec![
                coord("idle"),
                LiveAgent {
                    pane_id: "w4:p1".into(),
                    ..moved
                },
            ]),
            &cfg,
            12,
        );
        assert!(
            matches!(&state.pending[..], [Ev::Appeared { relinked: true, who, .. }] if who.pane_id == "w4:p1")
        );
    }

    #[test]
    fn messages_are_classified_by_their_relation_to_the_coordinator() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let data = live(vec![coord("idle"), agent("w2:p1", "a", "idle")]);
        quiet(&mut state, &data, &cfg, 0);
        let mut long = message("w2:p1", "w2:p2", "sent");
        long.text = "x".repeat(200);
        let msgs = [
            message("w2:p1", "w1:p1", "sent"), // typed into the coordinator: seen
            message("w2:p1", "w1:p1", "busy"),
            message("w1:p1", "w2:p1", "sent"),
            long,
            message("w2:p2", "w2:p1", "sent"),
        ];
        tick(&mut state, &data, &msgs, false, None, false, &cfg, 10);
        let scopes: Vec<(MsgScope, usize, Option<Prio>)> = state
            .pending
            .iter()
            .filter_map(|ev| match ev {
                Ev::Messages { scope, count, .. } => Some((*scope, *count, ev.prio())),
                _ => None,
            })
            .collect();
        assert_eq!(
            scopes,
            vec![
                (MsgScope::ToCoordinator, 1, Some(Prio::High)),
                (MsgScope::Between, 2, Some(Prio::Normal)),
                (MsgScope::FromCoordinator, 1, Some(Prio::Low)),
            ]
        );
        let Some(Ev::Messages { preview, .. }) = state.pending.get(1) else {
            panic!("between item expected");
        };
        assert!(preview[0].starts_with("w2:p1\u{2192}w2:p2 \""));
        assert!(preview[0].ends_with("\u{2026}\""));
        let Some(Ev::Messages { preview, .. }) = state.pending.first() else {
            panic!("to-coordinator item expected");
        };
        assert!(preview[0].contains("[busy]"), "{preview:?}");
    }

    #[test]
    fn a_sender_retrying_on_busy_wakes_the_coordinator_once_per_window() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let data = live(vec![coord("idle"), agent("w2:p1", "a", "idle")]);
        quiet(&mut state, &data, &cfg, 0);
        let busy = [message("w2:p1", "w1:p1", "busy")];
        let to_coordinator = |state: &WatchState| {
            state.pending.iter().find_map(|ev| match ev {
                Ev::Messages {
                    scope: MsgScope::ToCoordinator,
                    count,
                    ..
                } => Some(*count),
                _ => None,
            })
        };
        tick(&mut state, &data, &busy, false, None, false, &cfg, 10);
        // A repeat while the first is pending rides along with it.
        tick(&mut state, &data, &busy, false, None, false, &cfg, 15);
        assert_eq!(to_coordinator(&state), Some(2));
        // After the wake-up delivered them, repeats wait in the log...
        state.pending.clear();
        tick(&mut state, &data, &busy, false, None, false, &cfg, 60);
        assert_eq!(to_coordinator(&state), None);
        // ...but another sender, or the same one after the window, wakes it.
        let other = [message("w2:p2", "w1:p1", "busy")];
        tick(&mut state, &data, &other, false, None, false, &cfg, 61);
        assert_eq!(to_coordinator(&state), Some(1));
        state.pending.clear();
        tick(
            &mut state,
            &data,
            &busy,
            false,
            None,
            false,
            &cfg,
            10 + REFUSAL_REPEAT_S,
        );
        assert_eq!(to_coordinator(&state), Some(1));
    }

    #[test]
    fn pending_items_fold_beyond_the_cap() {
        let cfg = WakeCfg {
            debounce_s: 10_000,
            ..WakeCfg::default()
        };
        let mut state = WatchState::default();
        let mut agents = vec![coord("working")];
        quiet(&mut state, &live(agents.clone()), &cfg, 0);
        for i in 0..35u64 {
            agents.push(agent(&format!("w2:p{i}"), &format!("n{i}"), "idle"));
            quiet(&mut state, &live(agents.clone()), &cfg, 10 + i);
        }
        assert_eq!(state.pending.len(), PENDING_CAP);
        assert_eq!(state.folded, 5);
        assert_eq!(state.folded_since, 10);
        let evs = state.digest_events();
        assert!(matches!(
            evs.last(),
            Some(Ev::Earlier {
                count: 5,
                since: 10
            })
        ));
        assert_eq!(state.summary(&cfg, false, 100).pending, 35);
    }

    #[test]
    fn the_digest_caps_at_25_lines() {
        let ev = |i: u64| Ev::Appeared {
            key: format!("k{i}"),
            who: Who {
                name: format!("n{i}"),
                pane_id: format!("w2:p{i}"),
                ..Who::default()
            },
            at: 100 + i,
            relinked: false,
        };
        let many: Vec<Ev> = (0..40).map(ev).collect();
        let digest = digest_markdown(7, &many, 200);
        let lines: Vec<&str> = digest.lines().collect();
        assert_eq!(lines.len(), 25);
        assert!(lines[0].starts_with("# wake 7 "));
        assert!(lines[0].contains("40 changes since"), "{}", lines[0]);
        assert_eq!(lines[24], "+17 more");
        let fits: Vec<Ev> = (0..24).map(ev).collect();
        let digest = digest_markdown(8, &fits, 200);
        assert_eq!(digest.lines().count(), 25);
        assert!(!digest.contains("more"));
        assert_eq!(digest_markdown(9, &[], 200).lines().count(), 1);
    }

    #[test]
    fn wake_cfg_reads_its_overrides() {
        let env = HashMap::from([
            (DEBOUNCE_ENV, "10"),
            (GAP_ENV, "20"),
            (PERIODIC_ENV, "300"),
            (CAP_HOUR_ENV, "5"),
            (CAP_DAY_ENV, "junk"),
        ]);
        let cfg = WakeCfg::from_lookup(|name| env.get(name).map(|v| v.to_string()));
        assert_eq!(
            (
                cfg.debounce_s,
                cfg.gap_s,
                cfg.periodic_s,
                cfg.cap_hour,
                cfg.cap_day
            ),
            (10, 20, 300, 5, 80)
        );
        assert_eq!(cfg.gap(true), 20);
        assert_eq!(WakeCfg::from_lookup(|_| None), WakeCfg::default());
    }

    // ----- driver ------------------------------------------------------------

    type Calls = Rc<RefCell<Vec<Method>>>;

    fn recorder(
        reply: impl Fn(&Method) -> Result<Value, ApiError>,
    ) -> (impl Fn(Method) -> Result<Value, ApiError>, Calls) {
        let calls = Calls::default();
        let seen = calls.clone();
        let api = move |method: Method| {
            let out = reply(&method);
            seen.borrow_mut().push(method);
            out
        };
        (api, calls)
    }

    fn ctx(dir: &Path) -> LaunchCtx {
        LaunchCtx {
            herdr_bin: PathBuf::from("/opt/herdr"),
            dir: dir.to_path_buf(),
            port: crate::plus::DEFAULT_PORT,
        }
    }

    fn started(calls: &Calls) -> Vec<(String, Vec<String>)> {
        calls
            .borrow()
            .iter()
            .filter_map(|method| match method {
                Method::AgentStart(params) => Some((params.pane_id.clone(), params.args.clone())),
                _ => None,
            })
            .collect()
    }

    fn register_coordinator(dir: &Path, session: &str, pane: &str) {
        registry::update(dir, |registry| {
            registry
                .manage(
                    Some(session),
                    Some(pane),
                    Some("claude"),
                    &ManagePatch {
                        role: Some(COORDINATOR_ROLE.into()),
                        project: Some("herdr+".into()),
                        note: Some("keep".into()),
                    },
                )
                .map(|_| ())
        })
        .unwrap();
    }

    fn group_w1() -> Value {
        json!({ "workspaces": [{ "workspace_id": "w1", "label": "herdr+" }, { "workspace_id": "w2", "label": "app" }] })
    }

    fn no_sleep(_: Duration) {}

    #[test]
    fn coordinator_start_reuses_its_pane_and_rekeys_the_entry() {
        let dir = crate::plus::test_dir("coord-reuse");
        register_coordinator(&dir, "old", "w1:p1");
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::AgentStart(_) => Ok(json!({ "agent": { "pane_id": "w1:p1" } })),
            other => panic!("unexpected {other:?}"),
        });
        let pane = coordinator_start_with(&api, &dir, &ctx(&dir), None, &no_sleep).unwrap();
        assert_eq!(pane, "w1:p1");
        let starts = started(&calls);
        assert_eq!(starts.len(), 1);
        assert_eq!(starts[0].0, "w1:p1");
        let args = &starts[0].1;
        assert_eq!(args[0], "--session-id");
        assert!(args.iter().any(|arg| arg.starts_with("--mcp-config=")));
        assert!(args.iter().any(|arg| arg.starts_with("--allowedTools=")));
        let registry = Registry::load(&dir);
        assert_eq!(registry.agents.len(), 1);
        let entry = &registry.agents[0];
        assert_eq!(entry.session.as_deref(), Some(args[1].as_str()));
        assert_ne!(entry.session.as_deref(), Some("old"));
        assert_eq!(entry.note.as_deref(), Some("keep"));
        assert!(entry.is_coordinator());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordinator_start_falls_back_to_a_new_tab_when_the_pane_is_busy() {
        let dir = crate::plus::test_dir("coord-busy");
        register_coordinator(&dir, "old", "w1:p1");
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::AgentStart(params) if params.pane_id == "w1:p1" => {
                Err(ApiError::new("agent_pane_busy", "vim is running"))
            }
            Method::AgentStart(_) => Ok(json!({ "agent": {} })),
            Method::TabCreate(params) => {
                assert_eq!(params.workspace_id.as_deref(), Some("w1"));
                assert_eq!(params.label.as_deref(), Some(COORDINATOR_TAB));
                assert!(!params.focus);
                Ok(json!({ "tab": { "tab_id": "w1:t2" }, "root_pane": { "pane_id": "w1:p2" } }))
            }
            other => panic!("unexpected {other:?}"),
        });
        let pane = coordinator_start_with(&api, &dir, &ctx(&dir), None, &no_sleep).unwrap();
        assert_eq!(pane, "w1:p2");
        let starts = started(&calls);
        assert_eq!(starts.last().map(|(pane, _)| pane.as_str()), Some("w1:p2"));
        assert!(starts.len() > 2, "busy is retried before falling back");
        let registry = Registry::load(&dir);
        assert_eq!(registry.agents.len(), 1);
        assert_eq!(registry.agents[0].pane_id.as_deref(), Some("w1:p2"));
        assert_eq!(
            registry.agents[0].session.as_deref(),
            starts.last().map(|(_, args)| args[1].as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordinator_start_never_reuses_a_pane_outside_its_group() {
        let dir = crate::plus::test_dir("coord-stale");
        // After a restore w2:p5 may be anybody's shell.
        register_coordinator(&dir, "old", "w2:p5");
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::AgentStart(params) => {
                assert_ne!(params.pane_id, "w2:p5");
                Ok(json!({ "agent": {} }))
            }
            Method::TabCreate(_) => {
                Ok(json!({ "tab": { "tab_id": "w1:t2" }, "root_pane": { "pane_id": "w1:p2" } }))
            }
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(
            coordinator_start_with(&api, &dir, &ctx(&dir), None, &no_sleep).unwrap(),
            "w1:p2"
        );
        assert_eq!(started(&calls).len(), 1);
        assert!(in_group("w1:p2", "w1"));
        assert!(!in_group("w11:p2", "w1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordinator_start_creates_the_group_and_registers_the_coordinator() {
        let dir = crate::plus::test_dir("coord-new");
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => {
                Ok(json!({ "workspaces": [{ "workspace_id": "w1", "label": "app" }] }))
            }
            Method::WorkspaceCreate(params) => {
                assert_eq!(params.label.as_deref(), Some(COORDINATOR_GROUP));
                Ok(json!({
                    "workspace": { "workspace_id": "w3" },
                    "tab": { "tab_id": "w3:t1" },
                    "root_pane": { "pane_id": "w3:p1" }
                }))
            }
            Method::TabRename(params) => {
                assert_eq!(
                    (params.tab_id.as_str(), params.label.as_str()),
                    ("w3:t1", COORDINATOR_TAB)
                );
                Ok(json!({}))
            }
            Method::AgentStart(_) => Ok(json!({ "agent": {} })),
            other => panic!("unexpected {other:?}"),
        });
        let pane = coordinator_start_with(&api, &dir, &ctx(&dir), None, &no_sleep).unwrap();
        assert_eq!(pane, "w3:p1");
        let starts = started(&calls);
        assert_eq!(starts.len(), 1);
        let registry = Registry::load(&dir);
        let entry = registry.coordinator().unwrap();
        assert_eq!(entry.pane_id.as_deref(), Some("w3:p1"));
        assert_eq!(entry.session.as_deref(), Some(starts[0].1[1].as_str()));
        assert_eq!(entry.agent.as_deref(), Some("claude"));
        assert_eq!(entry.project.as_deref(), Some(COORDINATOR_GROUP));
        assert!(crate::plus::launch::claude_mcp_config_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resume_that_does_not_come_up_is_replaced_by_a_new_session() {
        let dir = crate::plus::test_dir("coord-resume");
        register_coordinator(&dir, "old", "w1:p1");
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::AgentStart(_) => Ok(json!({ "agent": {} })),
            Method::AgentGet(_) => Err(ApiError::new("agent_not_found", "no agent")),
            other => panic!("unexpected {other:?}"),
        });
        let slept = Rc::new(RefCell::new(0u64));
        let counter = slept.clone();
        let sleep = move |d: Duration| *counter.borrow_mut() += d.as_secs();
        coordinator_start_with(&api, &dir, &ctx(&dir), Some("old"), &sleep).unwrap();
        assert_eq!(*slept.borrow(), RESUME_CHECK_S);
        let starts = started(&calls);
        assert_eq!(starts.len(), 2);
        assert_eq!(
            starts[0].1[..2],
            ["--resume".to_string(), "old".to_string()]
        );
        assert_eq!(starts[1].1[0], "--session-id");
        let entry = Registry::load(&dir).coordinator().cloned().unwrap();
        assert_eq!(entry.session.as_deref(), Some(starts[1].1[1].as_str()));

        // A resume that comes up keeps its session.
        let (api, calls) = recorder(|method| match method {
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::AgentStart(_) => Ok(json!({ "agent": {} })),
            Method::AgentGet(_) => Ok(json!({ "agent": { "agent_status": "idle" } })),
            other => panic!("unexpected {other:?}"),
        });
        let session = entry.session.clone().unwrap();
        coordinator_start_with(&api, &dir, &ctx(&dir), Some(&session), &no_sleep).unwrap();
        assert_eq!(started(&calls).len(), 1);
        assert_eq!(
            Registry::load(&dir)
                .coordinator()
                .unwrap()
                .session
                .as_deref(),
            Some(session.as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordinator_start_refuses_a_corrupt_registry_before_launching() {
        let dir = crate::plus::test_dir("coord-corrupt");
        std::fs::write(crate::plus::registry_path(&dir), "{not json").unwrap();
        let (api, calls) = recorder(|method| panic!("unexpected {method:?}"));
        let err = coordinator_start_with(&api, &dir, &ctx(&dir), None, &no_sleep).unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        assert!(calls.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn api_agent(pane: &str, session: &str, status: &str) -> Value {
        json!({
            "terminal_id": "t", "pane_id": pane, "tab_id": format!("{}:t1", &pane[..2]),
            "workspace_id": &pane[..2], "agent": "claude", "agent_status": status,
            "agent_session": { "source": "hook", "agent": "claude", "kind": "id", "value": session },
            "focused": false, "revision": 1
        })
    }

    #[test]
    fn the_driver_publishes_live_data_and_delivers_a_requested_wake() {
        let dir = crate::plus::test_dir("watch-driver");
        crate::plus::seed(&dir).unwrap();
        register_coordinator(&dir, "cs", "w1:p1");
        for text in ["one", "two"] {
            let mut old = message("w2:p1", "w2:p2", "sent");
            old.text = text.into();
            messages::append(&dir, &old).unwrap();
        }
        let (api, calls) = recorder(|method| match method {
            Method::AgentList(_) => Ok(json!({ "agents": [
                api_agent("w1:p1", "cs", "idle"),
                api_agent("w2:p1", "unmanaged", "working"),
            ] })),
            Method::WorkspaceList(_) => Ok(group_w1()),
            Method::TabList(_) => Ok(json!({ "tabs": [] })),
            Method::AgentGet(_) => Ok(json!({ "agent": { "agent_status": "idle" } })),
            Method::AgentPrompt(_) => Ok(json!({})),
            other => panic!("unexpected {other:?}"),
        });
        let opts = WatchOpts {
            dir: dir.clone(),
            port: 7719,
            serve: false,
            coordinator: false,
            interval_ms: 2000,
            ctx: ctx(&dir),
        };
        let mut state = WatchState::load(&dir);
        state.rebaseline();
        let mut driver = Driver::new(api, &opts, WakeCfg::default(), state);
        std::fs::write(wake_request_path(&dir), "1").unwrap();
        assert_eq!(driver.step(100), Duration::from_millis(2000));
        assert!(!wake_request_path(&dir).exists(), "the request is consumed");
        let live: LiveData =
            serde_json::from_slice(&std::fs::read(crate::plus::live_path(&dir)).unwrap()).unwrap();
        assert_eq!(live.coordinator_pane.as_deref(), Some("w1:p1"));
        assert_eq!(live.unmanaged_count, 1);
        let texts: Vec<&str> = live.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"], "the log tail once, not twice");
        assert!(driver.state.forced, "waits for a settled coordinator");
        driver.step(106);
        let prompts: Vec<String> = calls
            .borrow()
            .iter()
            .filter_map(|method| match method {
                Method::AgentPrompt(params) => Some(format!("{} {}", params.target, params.text)),
                _ => None,
            })
            .collect();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].starts_with("w1:p1 [herdr+ wake-up #1 "),
            "{}",
            prompts[0]
        );
        assert!(wake_dir(&dir).join("1.md").exists());
        let turn = turn::read_live(&dir, 106).unwrap();
        assert_eq!((turn.source.as_str(), turn.id.as_str()), ("wake", "1"));
        let log = std::fs::read_to_string(wakeups_path(&dir)).unwrap();
        assert!(log.contains(" baseline\n"), "{log}");
        assert!(log.contains(" delivered #1 0 items -> w1:p1\n"), "{log}");
        let live: LiveData =
            serde_json::from_slice(&std::fs::read(crate::plus::live_path(&dir)).unwrap()).unwrap();
        assert_eq!(live.watch.wake_seq, 1);
        assert!(live.watch.turn_live);
        assert_eq!(WatchState::load(&dir).wake_seq, 1, "persisted");
        // No second wake while the turn is live.
        std::fs::write(wake_request_path(&dir), "1").unwrap();
        driver.step(150);
        let log = std::fs::read_to_string(wakeups_path(&dir)).unwrap();
        assert!(log.contains(" held turn live (pending 0)\n"), "{log}");
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|method| matches!(method, Method::AgentPrompt(_)))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_error_backs_off_and_rebaselines() {
        let dir = crate::plus::test_dir("watch-down");
        let api = |_: Method| -> Result<Value, ApiError> {
            Err(ApiError::new("server_unavailable", "down"))
        };
        let opts = WatchOpts {
            dir: dir.clone(),
            port: 7719,
            serve: false,
            coordinator: true,
            interval_ms: 2000,
            ctx: ctx(&dir),
        };
        let mut state = WatchState::load(&dir);
        state.baselined = true;
        let mut driver = Driver::new(api, &opts, WakeCfg::default(), state);
        assert_eq!(driver.step(100), ERROR_BACKOFF);
        assert!(!driver.state.baselined);
        assert!(!crate::plus::live_path(&dir).exists());
        let log = std::fs::read_to_string(wakeups_path(&dir)).unwrap();
        assert!(
            log.contains("server unavailable: server_unavailable: down"),
            "{log}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_digests_are_pruned() {
        let dir = crate::plus::test_dir("watch-prune");
        std::fs::create_dir_all(wake_dir(&dir)).unwrap();
        for seq in 1..=55 {
            std::fs::write(wake_dir(&dir).join(format!("{seq}.md")), "x").unwrap();
        }
        prune_digests(&dir);
        assert!(!wake_dir(&dir).join("5.md").exists());
        assert!(wake_dir(&dir).join("6.md").exists());
        assert_eq!(
            std::fs::read_dir(wake_dir(&dir)).unwrap().count(),
            DIGESTS_KEPT
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
