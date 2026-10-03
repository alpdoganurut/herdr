//! The watcher policy the coordinator worker ([`super::engine`]) runs: wake-up
//! bookkeeping and the pure [`tick`].
//!
//! Every pass the worker rebuilds `live.json` from herdr's agent facts, diffs
//! the agents in the wake scope (`[coordinator] wake_scope`, default: the
//! agents the coordinator opened) against what it saw last, and queues the
//! changes. Nothing outside the scope creates a pending item. A
//! wake-up (`agent.prompt` into the coordinator agent, pointing at a digest
//! file) fires only for an idle, settled coordinator with no live turn,
//! within the gap and the hourly/daily caps, and only when something is
//! worth it: a high item, a normal item older than the debounce, the slow
//! periodic check with anything pending, or `herdr coordinator wake`.
//! [`tick`] is the pure policy; the worker does the I/O around it.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::live::{LiveAgent, LiveData, WatchSummary};
use super::messages::{self, AgentMessage};
use super::turn::{self, Turn, TurnStep};
use super::{now_unix, wake_dir, wakeups_path, write_atomically};
use crate::api::schema::agents_model::{error_code, AgentActionEntry, AgentsActionOutcome};

pub const DEBOUNCE_ENV: &str = "HERDR_COORDINATOR_WAKE_DEBOUNCE_S";
pub const GAP_ENV: &str = "HERDR_COORDINATOR_WAKE_GAP_S";
pub const PERIODIC_ENV: &str = "HERDR_COORDINATOR_PERIODIC_S";
pub const CAP_HOUR_ENV: &str = "HERDR_COORDINATOR_WAKE_CAP_HOUR";
pub const CAP_DAY_ENV: &str = "HERDR_COORDINATOR_WAKE_CAP_DAY";

/// Pending items beyond this fold into one `+N earlier` line.
const PENDING_CAP: usize = 30;
const DIGEST_MAX_LINES: usize = 25;
const DIGESTS_KEPT: usize = 50;
const PREVIEWS: usize = 3;
const PREVIEW_CHARS: usize = 80;
const HELD_LOG_EVERY_S: u64 = 60;
/// The backoff after the server held a prepared wake-up back.
pub(crate) const HELD_RETRY_S: u64 = 30;
/// A sender's refused messages to the coordinator wake it at most once per
/// this window.
const REFUSAL_REPEAT_S: u64 = 600;
/// The same denial (actor, action, target, code) is queued once per window.
const DENIAL_REPEAT_S: u64 = 60;
pub(super) const LIVE_REFRESH_S: u64 = 10;
pub(super) const ROTATE_EVERY_S: u64 = 300;
pub(super) const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Messages kept in `live.json` for the dashboard's log.
pub(super) const LIVE_MESSAGES: usize = 50;
const HOUR: u64 = 3600;
const DAY: u64 = 86_400;

/// Which agents wake the coordinator (`[coordinator] wake_scope`). Every
/// agent still shows in `live.json` and the dashboard; only the scope's
/// agents (and messages among them) create wake-up items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeScope {
    /// The agents the coordinator opened (`agents_open_tab`), its own
    /// messages and refusals, and loop guards that involve it.
    #[default]
    Opened,
    /// Also every team member, messages among in-scope agents, and their
    /// denials.
    Teams,
    /// Every agent.
    All,
}

impl WakeScope {
    pub const NAMES: [&'static str; 3] = ["opened", "teams", "all"];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "opened" => Some(Self::Opened),
            "teams" => Some(Self::Teams),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Teams => "teams",
            Self::All => "all",
        }
    }

    /// Whether an agent with these facts is in the scope (the coordinator
    /// itself is always watched, apart from the scope).
    pub fn covers(self, opened_by_coordinator: bool, member: bool) -> bool {
        match self {
            Self::Opened => opened_by_coordinator,
            Self::Teams => opened_by_coordinator || member,
            Self::All => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeCfg {
    /// Which agents wake the coordinator.
    pub scope: WakeScope,
    pub debounce_s: u64,
    pub gap_s: u64,
    pub gap_high_s: u64,
    pub settle_s: u64,
    pub periodic_s: u64,
    pub cap_hour: u32,
    pub cap_day: u32,
    /// A registered coordinator missing this long triggers a relaunch
    /// ([`Action::Relaunch`]); the herdr server counts relaunches against
    /// its own cap.
    pub missing_s: u64,
}

impl Default for WakeCfg {
    fn default() -> Self {
        Self {
            scope: WakeScope::Opened,
            debounce_s: 60,
            gap_s: 120,
            gap_high_s: 45,
            settle_s: 5,
            periodic_s: 3600,
            cap_hour: 12,
            cap_day: 80,
            missing_s: 30,
        }
    }
}

impl WakeCfg {
    /// The configured caps and periodic check (`[coordinator]`), with the
    /// `HERDR_COORDINATOR_*` overrides applied on top.
    pub fn from_config(scope: WakeScope, cap_hour: u32, cap_day: u32, periodic_s: u64) -> Self {
        Self {
            scope,
            cap_hour,
            cap_day,
            periodic_s: periodic_s.max(1),
            ..Self::default()
        }
        .with_env()
    }

    /// These values with the `HERDR_COORDINATOR_*` overrides applied.
    pub fn with_env(self) -> Self {
        self.with_lookup(|name| std::env::var(name).ok())
    }

    #[cfg(test)]
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        Self::default().with_lookup(get)
    }

    fn with_lookup(self, get: impl Fn(&str) -> Option<String>) -> Self {
        let number = |name: &str| get(name).and_then(|value| value.trim().parse::<u64>().ok());
        let mut cfg = self;
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

/// An agent in the wake scope as the digest names it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Who {
    pub name: String,
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Who {
    fn of(agent: &LiveAgent) -> Self {
        Self {
            name: agent.name.clone(),
            pane_id: agent.pane_id.clone(),
            agent: agent.agent.clone(),
            role: agent.role.clone(),
        }
    }

    /// `rev (w2:p4, codex, reviewer)`.
    fn label(&self) -> String {
        let mut parts = vec![self.pane_id.clone()];
        if let Some(agent) = &self.agent {
            parts.push(agent.clone());
        }
        if let Some(role) = &self.role {
            parts.push(role.clone());
        }
        format!("{} ({})", self.name, parts.join(", "))
    }
}

/// Which messages a [`Ev::Messages`] item counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgScope {
    /// Addressed to the coordinator but refused (busy, blocked, ...): high.
    ToCoordinator,
    /// Between other agents, and replies logged for a busy coordinator: normal.
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
    /// An agent in the wake scope stopped running (its tab closed, it
    /// exited, or it left the scope).
    Gone { key: String, who: Who, at: u64 },
    Messages {
        scope: MsgScope,
        count: usize,
        preview: Vec<String>,
        first_at: u64,
        at: u64,
    },
    /// `managed.json` changed (agents v1); never queued since v2 retired the
    /// registry, kept so a persisted `watch_state.json` still parses.
    #[allow(dead_code)] // Read back from older watch_state.json files only.
    Registry {
        first_at: u64,
        at: u64,
        changes: u32,
    },
    /// Actions herdr refused agents in the wake scope (`actions.jsonl`:
    /// `non_user_turn`, `outside_team`), under the `teams` and `all` scopes.
    Denials {
        count: usize,
        preview: Vec<String>,
        first_at: u64,
        at: u64,
    },
    /// Older items folded away by the pending cap (digest only).
    Earlier { count: u64, since: u64 },
}

pub(super) fn is_idle(status: &str) -> bool {
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
            Ev::Appeared { .. } | Ev::Gone { .. } | Ev::Denials { .. } => Prio::Normal,
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
            | Ev::Registry { first_at, .. }
            | Ev::Denials { first_at, .. } => *first_at,
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
    who: Who,
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
    /// The herdr server gave up relaunching the coordinator (its relaunch
    /// cap); set by the server before every tick, published in live.json.
    #[serde(default)]
    pub coordinator_down: bool,
    /// Byte offset into `messages.jsonl`.
    #[serde(default)]
    pub msg_offset: u64,
    /// `herdr coordinator wake` is waiting for an idle coordinator.
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
    /// When each denial (actor, action, target, code) was last queued.
    #[serde(skip)]
    denial_queued: BTreeMap<String, u64>,
    /// The coordinator herdr runs, while it should be running: its pane and
    /// session (`coordinator.json`, set by the server before every tick).
    /// Missing from the live facts for `missing_s` triggers a relaunch.
    #[serde(skip)]
    pub expected_coordinator: Option<ExpectedCoordinator>,
}

/// The coordinator the server expects to run (its own record).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpectedCoordinator {
    /// Its pane; `None` once its tab closed (the relaunch opens a new one).
    pub pane_id: Option<String>,
    /// Its Claude session, for `--resume`.
    pub session: Option<String>,
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

    /// The server held the prompt back (the coordinator was not idle, not
    /// interactive or not in its pane by its own check): keep everything
    /// pending and back off briefly, so a disagreement between the facts
    /// and the server cannot turn into a wake-up loop. A forced wake waits
    /// the backoff too.
    pub fn wake_held(&mut self, now: u64, cfg: &WakeCfg) {
        self.retry_after = now + cfg.settle_s.max(HELD_RETRY_S);
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
            wakes_last_day: self.wakes_within(DAY, now) as u64,
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

    fn add_denials(&mut self, previews: Vec<String>, now: u64) {
        let count = previews.len();
        if count == 0 {
            return;
        }
        if let Some(Ev::Denials {
            count: c,
            preview,
            at,
            ..
        }) = self
            .pending
            .iter_mut()
            .find(|ev| matches!(ev, Ev::Denials { .. }))
        {
            *c += count;
            *at = now;
            let room = PREVIEWS.saturating_sub(preview.len());
            preview.extend(previews.into_iter().take(room));
            return;
        }
        self.pending.push(Ev::Denials {
            count,
            preview: previews.into_iter().take(PREVIEWS).collect(),
            first_at: now,
            at: now,
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

/// The coordinator's pane: live, else the one the server expects it in.
fn coordinator_pane<'a>(state: &'a WatchState, live: &'a LiveData) -> Option<&'a str> {
    live.coordinator_pane.as_deref().or(state
        .expected_coordinator
        .as_ref()
        .and_then(|expected| expected.pane_id.as_deref()))
}

/// Pure core: diff + policy. No I/O.
#[allow(clippy::too_many_arguments)] // the design's fixed signature: each input is a separate fact
pub fn tick(
    state: &mut WatchState,
    live: &LiveData,
    new_msgs: &[AgentMessage],
    new_actions: &[AgentActionEntry],
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
    // Read before `liveness`, which clears it once the coordinator shows up:
    // the server says down until `coordinator.start`, and a down
    // coordinator gets no wake-ups even when its agent is back.
    let down = state.coordinator_down;
    liveness(state, coordinator, cfg, now, &mut actions);
    if !state.baselined {
        baseline(state, live, now);
        actions.push(Action::Log("baseline".into()));
        return actions;
    }
    diff_agents(state, live, now);
    queue_messages(state, live, new_msgs, cfg.scope, now);
    queue_denials(state, live, new_actions, cfg.scope, now);
    state.cap_pending();
    gate(state, coordinator, down, turn_live, cfg, now, &mut actions);
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
    let Some(expected) = state.expected_coordinator.clone() else {
        // herdr does not run a coordinator now: nothing to relaunch.
        state.coord_missing_since = None;
        return;
    };
    if state.coordinator_down {
        return;
    }
    let since = *state.coord_missing_since.get_or_insert(now);
    if now.saturating_sub(since) < cfg.missing_s {
        return;
    }
    // A trigger, repeated every `missing_s` while the coordinator stays
    // missing: the server relaunches it through herdr's agent lifecycle and
    // counts the relaunches against its own cap (then sets `coordinator_down`).
    state.coord_missing_since = Some(now);
    // The coordinator is always Claude: its recorded session resumes it.
    actions.push(Action::Relaunch {
        resume: expected.session,
    });
}

/// Agents in the wake scope other than the coordinator, with their keys.
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
        who: Who::of(agent),
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
        // An appearance not reported yet and gone again is no news.
        let unreported = state.pending.iter().any(|ev| {
            matches!(
                ev,
                Ev::Appeared {
                    relinked: false,
                    ..
                }
            ) && ev.key() == Some(&key)
        });
        if unreported {
            state.pending.retain(|ev| ev.key() != Some(key.as_str()));
            continue;
        }
        state.pending.push(Ev::Gone {
            key,
            who: seen.who,
            at: now,
        });
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
        seen.who = who.clone();
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
    let outcome = if message.outcome == messages::OUTCOME_SENT {
        String::new()
    } else {
        format!(" [{}]", message.outcome)
    };
    format!("{from}\u{2192}{to}{outcome} \"{short}\"")
}

/// The refusal code of the per-pair loop guard (`agents_send_message`).
const TEAM_LOOP_GUARD: &str = "loop_guard";

fn queue_messages(
    state: &mut WatchState,
    live: &LiveData,
    new_msgs: &[AgentMessage],
    scope: WakeScope,
    now: u64,
) {
    let coordinator = coordinator_pane(state, live).map(str::to_string);
    let coordinator = coordinator.as_deref();
    let in_scope = |pane: Option<&str>| {
        pane.is_some_and(|pane| {
            live.agents
                .iter()
                .any(|agent| agent.managed && agent.pane_id == pane)
        })
    };
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
        // An update only moves a message logged before (queued → delivered,
        // expired, dropped): nothing new to report.
        if message.is_update() {
            continue;
        }
        if coordinator == Some(message.to_pane.as_str()) {
            // Typed into the coordinator: it has seen that one already. A
            // queued one is typed in once it is free, which starts its turn.
            if message.outcome == messages::OUTCOME_QUEUED {
                continue;
            }
            if message.outcome == messages::OUTCOME_LOGGED {
                // A reply logged while the coordinator was busy (usually in
                // agents_wait_for_message, which returned it). Delivered, not a
                // failure: the normal lane, in case it was not waiting.
                between.push(preview(message));
            } else if message.outcome != messages::OUTCOME_SENT {
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
        } else if scope == WakeScope::Opened {
            // Under `opened` only the coordinator's own traffic wakes it:
            // other agents' messages (teammate chatter included) stay in the
            // log and the dashboard. Loop guards that involve it are
            // refusals to or from it (above).
            continue;
        } else if in_scope(message.from_pane.as_deref()) && in_scope(Some(&message.to_pane)) {
            // Among agents in the scope; a teammate's ordinary chatter only
            // under `all`, a tripped loop guard always.
            if message.team.is_some()
                && message.outcome != TEAM_LOOP_GUARD
                && scope != WakeScope::All
            {
                continue;
            }
            between.push(preview(message));
        }
    }
    state.add_messages(MsgScope::ToCoordinator, to_coordinator, now);
    state.add_messages(MsgScope::Between, between, now);
    state.add_messages(MsgScope::FromCoordinator, from_coordinator, now);
}

/// `lead outside_team rename_tab w2:t3`.
fn denial_preview(entry: &AgentActionEntry) -> String {
    let who = entry
        .actor_name
        .as_deref()
        .or(entry.actor_pane.as_deref())
        .unwrap_or("?");
    let target = entry
        .target_name
        .as_deref()
        .or(entry.target_tab.as_deref())
        .or(entry.target_pane.as_deref())
        .unwrap_or("-");
    format!(
        "{who} {} {} {target}",
        entry.code.as_deref().unwrap_or("denied"),
        entry.action
    )
}

/// The refusals of agents in the scope (`teams`/`all` only): one normal-lane
/// line each, the same (actor, action, target, code) once per minute. The
/// raw lines stay in `actions.jsonl` and the dashboard.
fn queue_denials(
    state: &mut WatchState,
    live: &LiveData,
    new_actions: &[AgentActionEntry],
    scope: WakeScope,
    now: u64,
) {
    if scope == WakeScope::Opened {
        return;
    }
    state
        .denial_queued
        .retain(|_, at| now.saturating_sub(*at) < DENIAL_REPEAT_S);
    let mut previews = Vec::new();
    for entry in new_actions {
        let code = entry.code.as_deref().unwrap_or("");
        if entry.outcome != AgentsActionOutcome::Denied
            || !matches!(code, error_code::NON_USER_TURN | error_code::OUTSIDE_TEAM)
        {
            continue;
        }
        let in_scope = entry.actor_pane.as_deref().is_some_and(|pane| {
            live.agents
                .iter()
                .any(|agent| agent.managed && agent.pane_id == pane)
        });
        if !in_scope {
            continue;
        }
        let key = format!(
            "{}|{}|{}|{code}",
            entry.actor_pane.as_deref().unwrap_or(""),
            entry.action,
            entry
                .target_tab
                .as_deref()
                .or(entry.target_pane.as_deref())
                .unwrap_or("")
        );
        if state.denial_queued.contains_key(&key) {
            continue;
        }
        state.denial_queued.insert(key, now);
        previews.push(denial_preview(entry));
    }
    state.add_denials(previews, now);
}

/// Why a triggered wake-up waits (`held`/`suppressed` and the reason), if it does.
fn held_reason(
    state: &WatchState,
    coordinator: Option<&LiveAgent>,
    down: bool,
    turn_live: bool,
    high: bool,
    cfg: &WakeCfg,
    now: u64,
) -> Option<(&'static str, String)> {
    if down {
        return Some(("held", "coordinator down".into()));
    }
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
    down: bool,
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
    if let Some((verb, reason)) = held_reason(state, coordinator, down, turn_live, high, cfg, now) {
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
pub(super) fn iso(unix: u64) -> String {
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
        } => format!("- {}: new agent at {}", who.label(), clock(*at)),
        Ev::Appeared { who, at, .. } => {
            format!(
                "- {}: now in pane {} ({})",
                who.label(),
                who.pane_id,
                clock(*at)
            )
        }
        Ev::Gone { who, at, .. } => format!("- {}: gone since {}", who.label(), clock(*at)),
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
        Ev::Denials { count, preview, .. } => {
            let mut line = format!("- refused agent actions: {count} new");
            for item in preview {
                line.push_str("; ");
                line.push_str(item);
            }
            line
        }
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

// ----- file helpers (used by the worker, `engine`) -------------------------

pub(super) fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_millis() as u64)
        .unwrap_or(0)
}

/// One `wakeups.log` line: `2026-10-02T03:14:05 delivered #7 5 items -> w1:p1`.
pub(super) fn log_line(dir: &Path, now: u64, line: &str) {
    tracing::info!("coordinator watcher: {line}");
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(wakeups_path(dir))
        .and_then(|mut file| file.write_all(format!("{} {line}\n", iso(now)).as_bytes()));
    if let Err(err) = result {
        tracing::warn!("coordinator: cannot write wakeups.log: {err}");
    }
}

/// Keep the newest [`DIGESTS_KEPT`] `wake/<seq>.md` files.
pub(super) fn prune_digests(dir: &Path) {
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
            tracing::warn!("coordinator: cannot prune {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        tick(state, data, &[], &[], None, false, cfg, now)
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
            &[],
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
    fn a_down_coordinator_gets_no_wake_even_when_its_agent_is_back() {
        let mut state = WatchState::default();
        let cfg = cfg();
        let at = |status: &str| live(vec![coord("idle"), agent("w2:p1", "a", status)]);
        quiet(&mut state, &at("working"), &cfg, 0);
        quiet(&mut state, &at("idle"), &cfg, 10);
        // The server says down on every pass (its phase), the facts show
        // the coordinator idle: held, not woken.
        state.coordinator_down = true;
        let actions = quiet(&mut state, &at("idle"), &cfg, 200);
        assert!(wakes(&actions).is_empty(), "{actions:?}");
        assert!(
            logs(&actions)
                .iter()
                .any(|line| line.starts_with("held coordinator down")),
            "{actions:?}"
        );
        // `coordinator.start` cleared it: the pending item goes out.
        state.coordinator_down = false;
        assert_eq!(wakes(&quiet(&mut state, &at("idle"), &cfg, 210)).len(), 1);
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
        // A low item (the coordinator's own message) waits for the periodic check.
        let mut state = WatchState::default();
        quiet(&mut state, &data, &cfg, 0);
        let own = message("w1:p1", "w2:p1", "sent");
        tick(&mut state, &data, &[own], &[], None, false, &cfg, 10);
        assert_eq!(state.pending[0].prio(), Some(Prio::Low));
        assert!(quiet(&mut state, &data, &cfg, 299).is_empty());
        assert_eq!(wakes(&quiet(&mut state, &data, &cfg, 300)), vec![(1, 1)]);
    }

    #[test]
    fn no_wake_while_the_coordinator_is_busy_unsettled_or_in_a_turn() {
        let cfg = cfg();
        let request = |state: &mut WatchState, data: &LiveData, turn: Option<&Turn>, now| {
            tick(state, data, &[], &[], turn, true, &cfg, now)
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
            &[],
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
            &[],
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
            &[],
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
    fn a_missing_coordinator_triggers_a_relaunch_every_missing_window_until_down() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let gone = live(vec![]);
        // Nothing expected (herdr does not run one): never a relaunch.
        quiet(&mut state, &gone, &cfg, 0);
        assert!(quiet(&mut state, &gone, &cfg, 100).is_empty());
        let mut state = WatchState {
            expected_coordinator: Some(ExpectedCoordinator {
                pane_id: Some("w1:p1".into()),
                session: Some("cs".into()),
            }),
            ..WatchState::default()
        };
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
        assert_eq!(
            relaunches(&quiet(&mut state, &gone, &cfg, 120)),
            1,
            "no cap here: the server counts"
        );
        // Its tab closed (no pane any more): still a relaunch.
        let mut tabless = WatchState {
            expected_coordinator: Some(ExpectedCoordinator {
                pane_id: None,
                session: None,
            }),
            ..WatchState::default()
        };
        quiet(&mut tabless, &gone, &cfg, 0);
        assert_eq!(
            quiet(&mut tabless, &gone, &cfg, 30),
            vec![Action::Relaunch { resume: None }]
        );
        // The server gave up (its relaunch cap): no more triggers.
        state.coordinator_down = true;
        assert!(state.summary(&cfg, false, 150).coordinator_down);
        assert!(quiet(&mut state, &gone, &cfg, 5000).is_empty());
        // Back (started by hand): no longer down.
        let actions = quiet(&mut state, &live(vec![coord("idle")]), &cfg, 5010);
        assert!(!state.coordinator_down);
        assert!(logs(&actions)[0].starts_with("coordinator back"));
    }

    #[test]
    fn persisted_state_round_trips_without_rebaselining_counters() {
        let dir = crate::coordinator::test_dir("watch-state");
        let cfg = cfg();
        let mut state = WatchState::load(&dir);
        assert_eq!(state.dir, dir);
        quiet(&mut state, &live(vec![coord("idle")]), &cfg, 0);
        tick(
            &mut state,
            &live(vec![coord("idle")]),
            &[message("w1:p1", "w2:p1", "sent")],
            &[],
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
            &[message("w1:p1", "w2:p1", "sent")],
            &[],
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
        std::fs::write(crate::coordinator::watch_state_path(&dir), "{oops").unwrap();
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
    fn gone_relinked_and_rekeyed_agents() {
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
        // a and b are gone (their tabs closed, or they left the scope); c
        // learns its session id; and a moves panes when it comes back.
        let c = LiveAgent {
            session: Some("s-c".into()),
            ..pane_only
        };
        let next = live(vec![coord("idle"), c.clone()]);
        quiet(&mut state, &next, &cfg, 10);
        assert_eq!(state.pending.len(), 2, "{:?}", state.pending);
        assert!(
            matches!(&state.pending[0], Ev::Gone { key, who, .. } if key == "s-a" && who.name == "a")
        );
        assert!(matches!(&state.pending[1], Ev::Gone { key, .. } if key == "s-b"));
        assert_eq!(state.pending[0].prio(), Some(Prio::Normal));
        state.pending.retain(|ev| ev.key() != Some("s-b"));
        let moved = LiveAgent {
            pane_id: "w3:p1".into(),
            ..a
        };
        quiet(
            &mut state,
            &live(vec![coord("idle"), moved.clone(), c.clone()]),
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
                c,
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
        let cfg = WakeCfg {
            scope: WakeScope::All,
            ..cfg()
        };
        let mut state = WatchState::default();
        let data = live(vec![
            coord("idle"),
            agent("w2:p1", "a", "idle"),
            agent("w2:p2", "b", "idle"),
        ]);
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
        tick(&mut state, &data, &msgs, &[], None, false, &cfg, 10);
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
    fn teammate_chatter_never_wakes_the_coordinator_but_a_loop_guard_does() {
        let cfg = WakeCfg {
            scope: WakeScope::Teams,
            ..cfg()
        };
        let mut state = WatchState::default();
        let data = live(vec![
            coord("idle"),
            agent("w3:p1", "fixer", "idle"),
            agent("w3:p2", "reviewer", "idle"),
        ]);
        quiet(&mut state, &data, &cfg, 0);
        let teammate = |outcome: &str| {
            let mut m = message("w3:p1", "w3:p2", outcome);
            m.team = Some("w3".into());
            m
        };
        let msgs = [teammate("sent"), teammate("busy"), teammate("sent")];
        tick(&mut state, &data, &msgs, &[], None, false, &cfg, 10);
        assert!(
            !state
                .pending
                .iter()
                .any(|ev| matches!(ev, Ev::Messages { .. })),
            "teammate messages queue nothing: {:?}",
            state.pending
        );
        tick(
            &mut state,
            &data,
            &[teammate(TEAM_LOOP_GUARD)],
            &[],
            None,
            false,
            &cfg,
            20,
        );
        let between: Vec<usize> = state
            .pending
            .iter()
            .filter_map(|ev| match ev {
                Ev::Messages {
                    scope: MsgScope::Between,
                    count,
                    ..
                } => Some(*count),
                _ => None,
            })
            .collect();
        assert_eq!(between, vec![1], "only the loop guard is queued");
    }

    fn denial(actor: &str, target: &str, code: &str) -> AgentActionEntry {
        AgentActionEntry {
            unix: 1,
            id: format!("a-{actor}-{target}-{code}"),
            actor: crate::api::schema::agents_model::AgentActorKind::Agent,
            actor_pane: Some(actor.into()),
            actor_name: Some("fixer".into()),
            action: "close_tab".into(),
            target_tab: Some(target.into()),
            target_pane: None,
            target_name: None,
            team: None,
            turn_origin: None,
            origin_detail: None,
            outcome: AgentsActionOutcome::Denied,
            code: Some(code.into()),
            detail: None,
            closed_ids: Vec::new(),
        }
    }

    fn denials(state: &WatchState) -> Vec<(usize, Option<Prio>)> {
        state
            .pending
            .iter()
            .filter_map(|ev| match ev {
                Ev::Denials { count, .. } => Some((*count, ev.prio())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn refusals_of_agents_in_scope_queue_one_deduped_normal_line_under_teams() {
        let data = live(vec![
            coord("idle"),
            agent("w3:p1", "fixer", "idle"),
            LiveAgent {
                managed: false,
                ..agent("w4:p1", "solo", "idle")
            },
        ]);
        let refused = [
            denial("w3:p1", "w2:t3", "outside_team"),
            denial("w3:p1", "w2:t3", "outside_team"),
            denial("w3:p1", "w3:t2", "non_user_turn"),
            denial("w3:p1", "w3:t2", "rate_limited"),
            denial("w4:p1", "w2:t3", "outside_team"),
        ];
        // Under `opened` a refusal never wakes the coordinator.
        let mut state = WatchState::default();
        quiet(&mut state, &data, &cfg(), 0);
        tick(&mut state, &data, &[], &refused, None, false, &cfg(), 10);
        assert!(denials(&state).is_empty(), "{:?}", state.pending);
        // Under `teams`: in-scope agents' non_user_turn / outside_team only,
        // the same refusal once a minute, the normal lane.
        let teams = WakeCfg {
            scope: WakeScope::Teams,
            ..cfg()
        };
        let mut state = WatchState::default();
        quiet(&mut state, &data, &teams, 0);
        tick(&mut state, &data, &[], &refused, None, false, &teams, 10);
        assert_eq!(denials(&state), vec![(2, Some(Prio::Normal))]);
        tick(
            &mut state,
            &data,
            &[],
            &refused[..1],
            None,
            false,
            &teams,
            30,
        );
        assert_eq!(denials(&state), vec![(2, Some(Prio::Normal))], "deduped");
        tick(
            &mut state,
            &data,
            &[],
            &refused[..1],
            None,
            false,
            &teams,
            71,
        );
        assert_eq!(
            denials(&state),
            vec![(3, Some(Prio::Normal))],
            "a minute later"
        );
        let digest = digest_markdown(1, &state.digest_events(), 80);
        assert!(
            digest.contains("- refused agent actions: 3 new; fixer outside_team close_tab w2:t3"),
            "{digest}"
        );
    }

    #[test]
    fn under_opened_only_the_agents_the_coordinator_opened_create_items() {
        let cfg = cfg();
        let opened = agent("w2:p1", "mine", "working");
        let other = LiveAgent {
            managed: false,
            ..agent("w3:p1", "theirs", "working")
        };
        let mut state = WatchState::default();
        quiet(
            &mut state,
            &live(vec![coord("idle"), opened.clone(), other.clone()]),
            &cfg,
            0,
        );
        let blocked = |a: &LiveAgent| LiveAgent {
            status: "blocked".into(),
            ..a.clone()
        };
        // The other agent blocks for two ticks: nothing; the opened one does.
        let data = live(vec![coord("idle"), opened.clone(), blocked(&other)]);
        quiet(&mut state, &data, &cfg, 10);
        quiet(&mut state, &data, &cfg, 15);
        assert!(state.pending.is_empty(), "{:?}", state.pending);
        let data = live(vec![coord("idle"), blocked(&opened), blocked(&other)]);
        quiet(&mut state, &data, &cfg, 20);
        quiet(&mut state, &data, &cfg, 25);
        assert!(
            matches!(
                &state.pending[..],
                [Ev::Status { key, blocked_since: Some(_), .. }] if key == "s-mine"
            ),
            "{:?}",
            state.pending
        );
    }

    #[test]
    fn a_reply_logged_for_the_coordinator_is_not_a_refusal() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let data = live(vec![coord("idle"), agent("w2:p1", "a", "idle")]);
        quiet(&mut state, &data, &cfg, 0);
        let mut reply = message("w2:p1", "w1:p1", messages::OUTCOME_LOGGED);
        reply.reply_to = Some("m1abc".into());
        tick(&mut state, &data, &[reply], &[], None, false, &cfg, 10);
        let scopes: Vec<(MsgScope, Option<Prio>)> = state
            .pending
            .iter()
            .filter_map(|ev| match ev {
                Ev::Messages { scope, .. } => Some((*scope, ev.prio())),
                _ => None,
            })
            .collect();
        assert_eq!(scopes, vec![(MsgScope::Between, Some(Prio::Normal))]);
        assert!(
            state.refusal_queued.is_empty(),
            "a logged reply is not a refusal"
        );
        let Some(Ev::Messages { preview, .. }) = state.pending.first() else {
            panic!("messages item expected");
        };
        assert!(preview[0].contains("[logged]"), "{preview:?}");
    }

    #[test]
    fn a_message_queued_for_the_coordinator_and_its_updates_wake_nothing() {
        let cfg = cfg();
        let mut state = WatchState::default();
        let data = live(vec![coord("working"), agent("w2:p1", "a", "idle")]);
        quiet(&mut state, &data, &cfg, 0);
        let queued = message("w2:p1", "w1:p1", messages::OUTCOME_QUEUED);
        let delivered = AgentMessage::update(&queued, messages::OUTCOME_DELIVERED, 12);
        tick(
            &mut state,
            &data,
            &[queued, delivered],
            &[],
            None,
            false,
            &cfg,
            10,
        );
        assert!(
            !state
                .pending
                .iter()
                .any(|ev| matches!(ev, Ev::Messages { .. })),
            "typed in once the coordinator is free: {:?}",
            state.pending
        );
        assert!(state.refusal_queued.is_empty());
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
        tick(&mut state, &data, &busy, &[], None, false, &cfg, 10);
        // A repeat while the first is pending rides along with it.
        tick(&mut state, &data, &busy, &[], None, false, &cfg, 15);
        assert_eq!(to_coordinator(&state), Some(2));
        // After the wake-up delivered them, repeats wait in the log...
        state.pending.clear();
        tick(&mut state, &data, &busy, &[], None, false, &cfg, 60);
        assert_eq!(to_coordinator(&state), None);
        // ...but another sender, or the same one after the window, wakes it.
        let other = [message("w2:p2", "w1:p1", "busy")];
        tick(&mut state, &data, &other, &[], None, false, &cfg, 61);
        assert_eq!(to_coordinator(&state), Some(1));
        state.pending.clear();
        tick(
            &mut state,
            &data,
            &busy,
            &[],
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
        // Configured values sit under the overrides.
        let configured = WakeCfg {
            cap_hour: 4,
            cap_day: 20,
            periodic_s: 600,
            ..WakeCfg::default()
        };
        let cfg = configured
            .clone()
            .with_lookup(|name| env.get(name).map(|v| v.to_string()));
        assert_eq!((cfg.cap_hour, cfg.cap_day, cfg.periodic_s), (5, 20, 300));
        assert_eq!(configured.clone().with_lookup(|_| None), configured);
    }

    #[test]
    fn old_digests_are_pruned() {
        let dir = crate::coordinator::test_dir("watch-prune");
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
