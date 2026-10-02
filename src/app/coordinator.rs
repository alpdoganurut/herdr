//! The coordinator (fork), App side: the herdr server runs the coordinator
//! agent the way it runs News. `App.coordinator` holds the `[coordinator]`
//! settings, the coordinator tab and pane, the lifecycle phase, the read
//! model (`coordinator.get`) and the notification queue; the worker thread
//! ([`crate::coordinator::engine`]) owns every file under the coordinator
//! directory and the dashboard HTTP server.
//!
//! Lifecycle ([`CoordPhase`]): enabling spawns the worker, which migrates the
//! POC directory, takes the watcher lock and reports `Ready`
//! (`WaitingForLock` until then; `Blocked` while another server holds the
//! lock or the registry is corrupt). The first native start retires the
//! POC's coordinator entry (`Migrating`). `Starting` opens the `coordinator`
//! tab in the first space (never focused) and calls `start_agent` once the
//! pane is at its shell; `Launching` follows herdr's own managed-agent launch
//! state until it is interactive (`Running`, and the worker records the
//! coordinator in the registry). A coordinator that goes missing is a
//! relaunch trigger from the worker; relaunches count against
//! `relaunch_cap_hour`, past which the coordinator is `Down` until
//! `coordinator.start`.
//!
//! The pass input is built only from scalar terminal facts (agent kind,
//! state, name, session, subagents) and public ids — no cwd and no process
//! inspection — only when an agent or pane changed (`input_dirty`), at most
//! once a second. File facts run on the worker's own timer, so an idle
//! server is not woken for the coordinator.
//!
//! What the server must remember survives in `coordinator.json` next to
//! `session.json` ([`crate::persist::coordinator`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::coordinator::{
    blocked_reason, down_reason, error_code, CoordinatorBoardInfo, CoordinatorGetInfo,
    CoordinatorManagedInfo, CoordinatorOpenDashboardParams, CoordinatorSetEnabledParams,
    CoordinatorSetModelParams, CoordinatorSetNotifyParams, CoordinatorSetWakeCapsParams,
    CoordinatorStartParams, CoordinatorStateInfo, CoordinatorTurnInfo, CoordinatorWakeInfo,
    CoordinatorWakeParams,
};
use crate::api::schema::{AgentPromptParams, AgentStartParams, AgentStatus, ResponseResult};
use crate::config::{CoordinatorConfig, QuietHours};
use crate::coordinator::engine::{
    coordinator_launch_args, legacy_dir_for, BlockedReason, CoordinatorPassInput,
    CoordinatorPassOutput, DashboardState, Effect, EngineConfig, EngineStatus, MigrationOutcome,
    WakeOutcome, WorkerHandle, WorkerMsg,
};
use crate::coordinator::launch::LaunchCtx;
use crate::coordinator::live::{CoordinatorAgentFact, GroupFact};
use crate::coordinator::watch::WakeCfg;
use crate::persist::coordinator::{
    self as store, CoordinatorNotifyRecord, CoordinatorRecord, PendingCoordinatorNotify,
    PersistedDown,
};

/// The coordinator tab's label (it identifies the tab with its stored id).
pub(crate) const COORDINATOR_TAB_LABEL: &str = "coordinator";
/// The coordinator's agent name.
pub(crate) const COORDINATOR_AGENT_NAME: &str = "coordinator";
/// The POC's live coordinator is renamed to this on migration.
pub(crate) const LEGACY_AGENT_NAME: &str = "coordinator-legacy";
/// The pane is probed for its shell prompt this often while starting.
const START_RETRY: Duration = Duration::from_millis(500);
/// A pane that never reaches its shell prompt fails the start after this.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// The managed-launch timeout handed to `start_agent`.
const LAUNCH_TIMEOUT_MS: u64 = 60_000;
/// `Launching` gives up this long after the start (the launch timeout and a margin).
const LAUNCH_GIVE_UP: Duration = Duration::from_secs(65);
/// Agent facts are posted to the worker at most this often.
const INPUT_COALESCE: Duration = Duration::from_secs(1);
const HOUR_S: u64 = 3600;
/// One-time notice after the migration from the POC.
pub(crate) const MIGRATION_NOTICE: &str =
    "the coordinator moved to its own tab; close the old herdr+ group";
/// Notification text limits.
const NOTIFY_TITLE_CHARS: usize = 120;
const NOTIFY_BODY_CHARS: usize = 240;

/// Notification kinds (delivered by `server::headless::coordinator_notify`).
/// New suggestions on the board, batched.
pub(crate) const KIND_SUGGESTIONS: &str = "suggestions";
/// The coordinator went down (relaunch cap, start failure, name taken).
pub(crate) const KIND_DOWN: &str = "down";
/// The coordinator is blocked on a prompt (its own NeedsAttention).
pub(crate) const KIND_BLOCKED: &str = "blocked";
/// Another server holds the coordinator lock.
pub(crate) const KIND_LOCKED: &str = "locked";

/// The `[coordinator]` keys the notification delivery reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoordinatorNotifyCfg {
    pub(crate) enabled: bool,
    pub(crate) daily_cap: u32,
    pub(crate) quiet: Option<QuietHours>,
}

/// The coordinator's lifecycle phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordPhase {
    Off,
    /// The worker runs and has not reported `Ready` yet.
    WaitingForLock,
    Blocked(BlockedReason),
    /// Not on this platform, or the session is not persisted.
    Unavailable(String),
    /// First native start: waiting for the worker to retire the POC entry.
    Migrating {
        asked: bool,
    },
    Starting {
        resume: Option<String>,
        next_try: Instant,
        give_up_at: Instant,
    },
    Launching {
        give_up_at: Instant,
    },
    Running,
    Down {
        since: u64,
        reason: String,
    },
}

impl CoordPhase {
    fn info(&self) -> CoordinatorStateInfo {
        match self {
            Self::Off => CoordinatorStateInfo::Off,
            Self::WaitingForLock => CoordinatorStateInfo::WaitingForLock,
            Self::Blocked(_) => CoordinatorStateInfo::Blocked,
            Self::Unavailable(_) => CoordinatorStateInfo::Unavailable,
            Self::Migrating { .. } | Self::Starting { .. } | Self::Launching { .. } => {
                CoordinatorStateInfo::Starting
            }
            Self::Running => CoordinatorStateInfo::Running,
            Self::Down { .. } => CoordinatorStateInfo::Down,
        }
    }

    /// Waiting for the worker, or not able to run at all.
    fn before_ready(&self) -> bool {
        matches!(
            self,
            Self::Off | Self::WaitingForLock | Self::Blocked(_) | Self::Unavailable(_)
        )
    }
}

fn blocked_code(reason: &BlockedReason) -> &'static str {
    match reason {
        BlockedReason::LockedElsewhere => blocked_reason::LOCKED_ELSEWHERE,
        BlockedReason::RegistryCorrupt(_) => blocked_reason::REGISTRY_CORRUPT,
        BlockedReason::Unavailable(_) => "unavailable",
    }
}

/// An error of a `coordinator.*` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl CoordinatorError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn disabled() -> Self {
        Self::new(
            error_code::DISABLED,
            "the coordinator is off ([coordinator] enabled = false)",
        )
    }
}

/// The App-side coordinator state (`App.coordinator`).
pub(crate) struct CoordinatorState {
    pub(crate) enabled: bool,
    pub(crate) cfg: WakeCfg,
    pub(crate) model: Option<String>,
    pub(crate) notify: CoordinatorNotifyCfg,
    pub(crate) relaunch_cap_hour: u32,
    pub(crate) dashboard_port: u16,
    /// The coordinator directory (read when the worker starts).
    pub(crate) dir: PathBuf,
    /// `coordinator.json`; `None` keeps everything in memory.
    pub(crate) store: Option<PathBuf>,
    /// Why the coordinator cannot run here, if it cannot.
    pub(crate) unavailable: Option<String>,
    pub(crate) phase: CoordPhase,
    worker: Option<WorkerHandle>,
    /// The worker's last reported status.
    engine: EngineStatus,
    pub(crate) dashboard: DashboardState,
    /// Agents or panes changed: post new facts on the next pass.
    pub(crate) input_dirty: bool,
    last_input_at: Option<Instant>,
    last_input: Option<CoordinatorPassInput>,
    /// The worker's latest read model pieces.
    last_output: Option<CoordinatorPassOutput>,
    // Persisted (coordinator.json).
    pub(crate) tab_id: Option<String>,
    pub(crate) pane_id: Option<String>,
    pub(crate) session: Option<String>,
    pub(crate) relaunches: Vec<u64>,
    pub(crate) notify_ledger: CoordinatorNotifyRecord,
    pub(crate) seen_suggestions: Vec<u64>,
    pub(crate) unread: u32,
    pub(crate) migrated: bool,
    pub(crate) notice: Option<String>,
    /// Opens the dashboard URL off the main loop (tests swap it).
    pub(crate) opener: fn(&str) -> Result<&'static str, String>,
    /// The coordinator pane's terminal, for the O(1) suppression check.
    terminal_id: Option<crate::terminal::TerminalId>,
    /// A delivery held back by the notification rate limit tries again then.
    pub(crate) notify_retry_at: Option<Instant>,
    /// A fresh start was asked while the coordinator was alive: release the
    /// suspended record once its exit is observed, then start.
    release_suspended: bool,
    /// Tests: the local minute of day and the day the policy goes by.
    #[cfg(test)]
    pub(crate) local_override: Option<(u16, &'static str)>,
    /// Tests: treat the coordinator pane as at a shell prompt.
    #[cfg(test)]
    pub(crate) assume_shell_ready: bool,
    /// Tests: run without a worker thread; messages are recorded instead.
    #[cfg(test)]
    pub(crate) no_worker: bool,
    #[cfg(test)]
    pub(crate) sent: Vec<WorkerMsg>,
    /// Tests: dashboard URLs the opener was asked to open.
    #[cfg(test)]
    pub(crate) opened: Vec<String>,
}

impl std::fmt::Debug for CoordinatorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinatorState")
            .field("enabled", &self.enabled)
            .field("phase", &self.phase)
            .field("engine", &self.engine)
            .field("dashboard", &self.dashboard)
            .field("tab_id", &self.tab_id)
            .field("pane_id", &self.pane_id)
            .finish_non_exhaustive()
    }
}

impl CoordinatorState {
    /// State for a server: the record loaded from `coordinator.json` next to
    /// `session.json` when the session is persisted.
    pub(crate) fn new(config: &CoordinatorConfig, persisted: bool) -> Self {
        let store = persisted.then(store::store_path);
        let mut state = Self::in_memory(config);
        state.unavailable = unavailable_reason(persisted);
        state.store = store;
        if let Some(path) = state.store.clone() {
            state.load_record(store::load(&path));
        }
        state
    }

    /// State with nothing on disk (tests, unpersisted sessions).
    pub(crate) fn in_memory(config: &CoordinatorConfig) -> Self {
        let mut state = Self {
            enabled: false,
            cfg: WakeCfg::default(),
            model: None,
            notify: CoordinatorNotifyCfg {
                enabled: true,
                daily_cap: 0,
                quiet: None,
            },
            relaunch_cap_hour: 0,
            dashboard_port: 0,
            dir: crate::coordinator::coordinator_dir(),
            store: None,
            unavailable: None,
            phase: CoordPhase::Off,
            worker: None,
            engine: EngineStatus::WaitingForLock,
            dashboard: DashboardState::Off,
            input_dirty: false,
            last_input_at: None,
            last_input: None,
            last_output: None,
            tab_id: None,
            pane_id: None,
            session: None,
            relaunches: Vec::new(),
            notify_ledger: CoordinatorNotifyRecord::default(),
            seen_suggestions: Vec::new(),
            unread: 0,
            migrated: false,
            notice: None,
            opener: open_dashboard_url,
            terminal_id: None,
            notify_retry_at: None,
            release_suspended: false,
            #[cfg(test)]
            local_override: None,
            #[cfg(test)]
            assume_shell_ready: false,
            #[cfg(test)]
            no_worker: false,
            #[cfg(test)]
            sent: Vec::new(),
            #[cfg(test)]
            opened: Vec::new(),
        };
        state.apply_config(config);
        state
    }

    fn load_record(&mut self, record: CoordinatorRecord) {
        self.tab_id = record.tab_id;
        self.pane_id = record.pane_id;
        self.session = record.session;
        self.relaunches = record.relaunches;
        self.notify_ledger = record.notify;
        self.seen_suggestions = record.seen_suggestions;
        self.unread = record.unread;
        self.migrated = record.migrated;
        self.notice = record.notice;
        if let Some(down) = record.down {
            // A restart keeps a coordinator that gave up down until
            // `coordinator.start`.
            self.phase = CoordPhase::Down {
                since: down.since,
                reason: down.reason,
            };
        }
    }

    fn record(&self) -> CoordinatorRecord {
        CoordinatorRecord {
            tab_id: self.tab_id.clone(),
            pane_id: self.pane_id.clone(),
            session: self.session.clone(),
            relaunches: self.relaunches.clone(),
            down: match &self.phase {
                CoordPhase::Down { since, reason } => Some(PersistedDown {
                    since: *since,
                    reason: reason.clone(),
                }),
                _ => None,
            },
            notify: self.notify_ledger.clone(),
            seen_suggestions: self.seen_suggestions.clone(),
            unread: self.unread,
            migrated: self.migrated,
            notice: self.notice.clone(),
        }
    }

    /// Save `coordinator.json` (no-op without a store).
    pub(crate) fn persist(&self) {
        let Some(path) = self.store.as_deref() else {
            return;
        };
        if let Err(err) = store::save(path, &self.record()) {
            tracing::warn!(
                event = "coordinator.persist",
                outcome = "write_failed",
                err = %err,
                "failed to write coordinator.json"
            );
        }
    }

    /// Apply `[coordinator]` (server start and config reloads). Turning it
    /// off drops the worker (lock, wakes and dashboard stop); the
    /// coordinator's pane is left running.
    pub(crate) fn apply_config(&mut self, config: &CoordinatorConfig) {
        let cfg = config.wake_cfg();
        if cfg != self.cfg {
            self.send(WorkerMsg::SetWakeCfg(cfg.clone()));
        }
        self.cfg = cfg;
        self.model = config.model();
        self.notify = CoordinatorNotifyCfg {
            enabled: config.notify,
            daily_cap: config.notify_daily_cap,
            quiet: config.quiet_hours(),
        };
        self.relaunch_cap_hour = config.relaunch_cap_hour;
        let port = config.dashboard_port();
        if port != self.dashboard_port {
            self.send(WorkerMsg::SetDashboardPort(port));
        }
        self.dashboard_port = port;
        self.set_enabled(config.enabled);
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.stop_worker();
            self.phase = CoordPhase::Off;
        }
        tracing::info!(
            event = "coordinator.enabled",
            enabled,
            "coordinator switched"
        );
    }

    /// Stop the worker (disable, server shutdown): joins it, which stops
    /// the dashboard and releases the lock.
    pub(crate) fn stop_worker(&mut self) {
        self.worker = None;
        self.engine = EngineStatus::WaitingForLock;
        self.dashboard = DashboardState::Off;
        self.last_input = None;
        self.last_output = None;
    }

    fn send(&mut self, msg: WorkerMsg) {
        match &self.worker {
            Some(worker) => {
                if !worker.send(msg) {
                    tracing::warn!("coordinator: the worker is gone");
                }
            }
            None => self.unsent(msg),
        }
    }

    /// Without a worker a message has nowhere to go (tests record it).
    #[cfg(test)]
    fn unsent(&mut self, msg: WorkerMsg) {
        if self.no_worker {
            self.sent.push(msg);
        }
    }

    #[cfg(not(test))]
    fn unsent(&mut self, _msg: WorkerMsg) {}

    fn has_worker(&self) -> bool {
        #[cfg(test)]
        if self.no_worker {
            return true;
        }
        self.worker.is_some()
    }

    /// Mark the agent facts stale (agent transitions, pane updates).
    pub(crate) fn mark_input_dirty(&mut self) {
        if self.enabled {
            self.input_dirty = true;
        }
    }

    /// The local minute of day and `YYYY-MM-DD`.
    pub(crate) fn local_now(&self) -> (Option<u16>, String) {
        #[cfg(test)]
        if let Some((minute, day)) = self.local_override {
            return (Some(minute), day.to_string());
        }
        match crate::platform::local_datetime() {
            Some(local) => (
                Some(u16::from(local.hour()) * 60 + u16::from(local.minute())),
                format!(
                    "{:04}-{:02}-{:02}",
                    local.year(),
                    u8::from(local.month()),
                    local.day()
                ),
            ),
            None => (None, String::new()),
        }
    }

    /// The first pending notification due at `now_unix`.
    pub(crate) fn due_notification_index(&self, now_unix: u64) -> Option<usize> {
        self.notify_ledger
            .pending
            .iter()
            .position(|pending| pending.deliver_after <= now_unix)
    }

    /// Queue a notification, replacing a pending one of the same kind.
    fn queue_notification(&mut self, kind: &str, title: &str, body: Option<&str>, urgent: bool) {
        let Some(title) = super::api::sanitized_notification_text(title, NOTIFY_TITLE_CHARS) else {
            return;
        };
        let body =
            body.and_then(|body| super::api::sanitized_notification_text(body, NOTIFY_BODY_CHARS));
        self.notify_ledger
            .pending
            .retain(|pending| pending.kind != kind);
        self.notify_ledger.pending.push(PendingCoordinatorNotify {
            kind: kind.to_string(),
            title,
            body,
            urgent,
            deliver_after: 0,
            queued_at: crate::coordinator::now_unix(),
        });
        tracing::info!(
            event = "coordinator.notify",
            outcome = "queued",
            kind,
            "coordinator notification queued"
        );
    }

    /// Raise a failure alert once per streak.
    fn alert(&mut self, kind: &str, title: &str, body: Option<&str>) {
        if self
            .notify_ledger
            .alerted
            .iter()
            .any(|alerted| alerted == kind)
        {
            return;
        }
        self.notify_ledger.alerted.push(kind.to_string());
        self.queue_notification(kind, title, body, kind == KIND_DOWN);
        self.persist();
    }

    /// The condition behind an alert cleared: the next streak alerts again.
    fn clear_alert(&mut self, kind: &str) {
        let before = self.notify_ledger.alerted.len();
        self.notify_ledger.alerted.retain(|alerted| alerted != kind);
        if self.notify_ledger.alerted.len() != before {
            self.notify_ledger
                .pending
                .retain(|pending| pending.kind != kind);
            self.persist();
        }
    }

    fn go_down(&mut self, reason: &str, detail: &str) {
        let since = crate::coordinator::now_unix();
        tracing::warn!(
            event = "coordinator.down",
            reason,
            detail,
            "coordinator down"
        );
        self.phase = CoordPhase::Down {
            since,
            reason: reason.to_string(),
        };
        self.input_dirty = true;
        self.alert(
            KIND_DOWN,
            "coordinator down",
            Some(&format!(
                "{detail} — restart it from the coordinator row or `herdr coordinator start`"
            )),
        );
        self.persist();
    }

    fn dashboard_url(&self) -> Option<String> {
        self.dashboard.url()
    }

    fn dashboard_error(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        match self.dashboard {
            DashboardState::PortInUse(port) => Some(format!("port {port} in use")),
            DashboardState::Off if self.dashboard_port == 0 => {
                Some("dashboard disabled (dashboard_port = 0)".into())
            }
            DashboardState::Off if self.phase.before_ready() => {
                Some("coordinator not ready".into())
            }
            _ => None,
        }
    }
}

/// Why the coordinator cannot run on this server, if it cannot.
fn unavailable_reason(persisted: bool) -> Option<String> {
    if !crate::platform::coordinator_supported() {
        return Some("the coordinator is not supported on this platform yet".into());
    }
    // Tests run unpersisted servers: the record stays in memory there.
    if !persisted && !cfg!(test) {
        return Some("the session is not persisted".into());
    }
    None
}

/// The API status text of an agent state.
fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
        AgentStatus::Suspended => "suspended",
    }
}

/// The native session id a terminal reports: the parked record, the
/// lifecycle hook, then the persisted session (as `agent.list` does).
fn terminal_session(terminal: &crate::terminal::TerminalState) -> Option<String> {
    if let Some(record) = terminal.suspended_agent.as_ref() {
        return Some(record.session.session_ref.value.clone());
    }
    if let Some(session) = terminal
        .hook_authority
        .as_ref()
        .and_then(|authority| authority.session_ref.as_ref())
    {
        return Some(session.value.clone());
    }
    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| session.session_ref.value.clone())
}

/// The error code of an encoded API error response.
fn error_code(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_else(|| "agent_prompt_failed".into())
}

/// Open `url`: herdr's browser first (it answers `browser_disabled` when
/// `[browser] enabled` is off), the system browser when that fails.
/// Returns which one opened it.
pub(crate) fn open_with(
    url: &str,
    browser: impl FnOnce(&str) -> Result<(), String>,
    system: impl FnOnce(&str) -> Result<(), String>,
) -> Result<&'static str, String> {
    match browser(url) {
        Ok(()) => Ok("browser"),
        Err(browser_err) => {
            tracing::info!(error = %browser_err, "coordinator: dashboard not opened in herdr's browser");
            system(url).map(|()| "system")
        }
    }
}

/// The production opener: herdr's browser, else the system browser.
fn open_dashboard_url(url: &str) -> Result<&'static str, String> {
    open_with(url, open_in_herdr_browser, open_in_system_browser)
}

fn open_in_herdr_browser(url: &str) -> Result<(), String> {
    use crate::api::schema::{BrowserActor, BrowserOp, BrowserRunParams};
    crate::browser::hub()
        .run(
            &BrowserActor::External { raw: None },
            BrowserRunParams {
                caller: None,
                profile: None,
                tab: None,
                op: BrowserOp::Open {
                    url: url.to_string(),
                    focus: true,
                    wait: None,
                },
                timeout_ms: Some(10_000),
            },
        )
        .map(|_| ())
        .map_err(|err| format!("{}: {}", err.code(), err.message()))
}

fn open_in_system_browser(url: &str) -> Result<(), String> {
    match crate::platform::open_url(url) {
        Ok(Some(mut child)) => child.wait().map(|_| ()).map_err(|err| err.to_string()),
        Ok(None) => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

/// The coordinator tab's indices.
#[derive(Debug, Clone, Copy)]
struct CoordPane {
    ws_idx: usize,
    tab_idx: usize,
    pane_id: crate::layout::PaneId,
}

impl App {
    // ----- read model ------------------------------------------------------

    /// `coordinator.get`.
    pub(crate) fn coordinator_get_info(&self) -> CoordinatorGetInfo {
        let state = &self.coordinator;
        let pane = self.existing_coordinator_pane();
        let terminal = pane.and_then(|pane| self.coordinator_terminal(pane));
        let out = state.last_output.as_ref();
        let summary = out.and_then(|out| out.summary.as_ref());
        let managed = out
            .map(|out| {
                out.managed
                    .iter()
                    .map(|agent| CoordinatorManagedInfo {
                        name: agent.name.clone(),
                        role: agent.role.clone(),
                        project: agent.project.clone(),
                        note: agent.note.clone(),
                        pane_id: Some(agent.pane_id.clone()),
                        tab_id: Some(agent.tab_id.clone()).filter(|id| !id.is_empty()),
                        agent: agent.agent.clone(),
                        status: Some(agent.status.clone()),
                        last_change_at: Some(agent.last_change_unix).filter(|at| *at > 0),
                    })
                    .chain(out.offline.iter().map(|entry| {
                        CoordinatorManagedInfo {
                            name: entry
                                .role
                                .clone()
                                .or_else(|| entry.pane_id.clone())
                                .unwrap_or_else(|| "offline".into()),
                            role: entry.role.clone(),
                            project: entry.project.clone(),
                            note: None,
                            pane_id: entry.pane_id.clone(),
                            tab_id: None,
                            agent: entry.agent.clone(),
                            status: Some("offline".into()),
                            last_change_at: None,
                        }
                    }))
                    .collect()
            })
            .unwrap_or_default();
        let wake = summary
            .map(|summary| CoordinatorWakeInfo {
                seq: summary.wake_seq,
                hour: summary.wakes_last_hour as u32,
                day: summary.wakes_last_day as u32,
                cap_hour: state.cfg.cap_hour,
                cap_day: state.cfg.cap_day,
                capped: summary.capped,
                pending: summary.pending as u32,
                last_at: Some(summary.last_wake_unix).filter(|at| *at > 0),
                next_periodic_at: (summary.pending > 0 && summary.last_wake_unix > 0)
                    .then(|| summary.last_wake_unix + state.cfg.periodic_s),
            })
            .unwrap_or(CoordinatorWakeInfo {
                cap_hour: state.cfg.cap_hour,
                cap_day: state.cfg.cap_day,
                ..CoordinatorWakeInfo::default()
            });
        let now = crate::coordinator::now_unix();
        CoordinatorGetInfo {
            enabled: state.enabled,
            state: state.phase.info(),
            blocked_reason: match &state.phase {
                CoordPhase::Blocked(reason) => Some(blocked_code(reason).to_string()),
                CoordPhase::Unavailable(reason) => Some(reason.clone()),
                _ => None,
            },
            down_reason: match &state.phase {
                CoordPhase::Down { reason, .. } => Some(reason.clone()),
                _ => None,
            },
            notice: state.notice.clone(),
            tab_id: pane.and_then(|pane| self.public_tab_id(pane.ws_idx, pane.tab_idx)),
            pane_id: pane.and_then(|pane| self.public_pane_id(pane.ws_idx, pane.pane_id)),
            coordinator_status: pane
                .and_then(|pane| self.coordinator_status(pane))
                .map(str::to_string),
            coordinator_session: terminal
                .and_then(terminal_session)
                .or_else(|| state.session.clone())
                .or_else(|| out.and_then(|out| out.coordinator_session.clone())),
            model: state.model.clone(),
            dashboard_url: state.dashboard_url(),
            dashboard_error: state.dashboard_error(),
            turn: out
                .and_then(|out| out.turn.as_ref())
                .map(|turn| CoordinatorTurnInfo {
                    source: turn.source.clone(),
                    id: turn.id.clone(),
                    started_at: turn.started_unix,
                }),
            wake,
            relaunches_hour: state
                .relaunches
                .iter()
                .filter(|at| now.saturating_sub(**at) < HOUR_S)
                .count() as u32,
            managed,
            board: out
                .and_then(|out| out.board.as_ref())
                .map(|board| CoordinatorBoardInfo {
                    generated_at: (board.generated_unix > 0)
                        .then(|| crate::app::news::iso_utc(board.generated_unix)),
                    summary: board.summary.clone(),
                    suggestion_count: board.suggestions.len() as u32,
                    unread: state.unread,
                }),
            unread_suggestions: state.unread,
            coordinator_dir: state.dir.display().to_string(),
            notify: state.notify.enabled,
            wake_queued: None,
        }
    }

    /// The coordinator tab as a notification target: space, tab and pane ids.
    pub(crate) fn coordinator_notification_target(&self) -> Option<(String, String, String)> {
        let pane = self.existing_coordinator_pane()?;
        Some((
            self.public_workspace_id(pane.ws_idx),
            self.public_tab_id(pane.ws_idx, pane.tab_idx)?,
            self.public_pane_id(pane.ws_idx, pane.pane_id)?,
        ))
    }

    /// Whether a terminal is the coordinator's. Production reads the
    /// `AppState::coordinator_terminal_id` mirror in the `suppress_completion`
    /// arm (app/actions.rs).
    #[cfg(test)]
    pub(crate) fn is_coordinator_terminal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
    ) -> bool {
        self.state.coordinator_terminal_id.as_ref() == Some(terminal_id)
    }

    /// Mark the agent facts stale (the agent-transition and pane-update hooks).
    pub(crate) fn mark_coordinator_input_dirty(&mut self) {
        self.coordinator.mark_input_dirty();
    }

    // ----- API handlers ----------------------------------------------------

    fn coordinator_reply(&self, id: String, result: Result<(), CoordinatorError>) -> String {
        match result {
            Ok(()) => encode_success(
                id,
                ResponseResult::CoordinatorGet {
                    info: self.coordinator_get_info(),
                },
            ),
            Err(err) => encode_error(id, err.code, err.message),
        }
    }

    pub(crate) fn handle_coordinator_get(&mut self, id: String) -> String {
        self.coordinator_reply(id, Ok(()))
    }

    pub(crate) fn handle_coordinator_open(&mut self, id: String) -> String {
        let result = self.open_coordinator(Instant::now());
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_open_dashboard(
        &mut self,
        id: String,
        params: CoordinatorOpenDashboardParams,
    ) -> String {
        let result = self.open_coordinator_dashboard(params.open);
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_wake(
        &mut self,
        id: String,
        params: CoordinatorWakeParams,
    ) -> String {
        match self.wake_coordinator(params.caller_pane.as_deref()) {
            Ok(queued) => {
                let mut info = self.coordinator_get_info();
                info.wake_queued = queued;
                encode_success(id, ResponseResult::CoordinatorGet { info })
            }
            Err(err) => encode_error(id, err.code, err.message),
        }
    }

    pub(crate) fn handle_coordinator_start(
        &mut self,
        id: String,
        params: CoordinatorStartParams,
    ) -> String {
        let result =
            self.start_coordinator(params.resume, params.caller_pane.as_deref(), Instant::now());
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_set_enabled(
        &mut self,
        id: String,
        params: CoordinatorSetEnabledParams,
    ) -> String {
        let result = self
            .refuse_in_coordinator_turn(params.caller_pane.as_deref())
            .and_then(|()| self.set_coordinator_enabled(params.enabled));
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_set_wake_caps(
        &mut self,
        id: String,
        params: CoordinatorSetWakeCapsParams,
    ) -> String {
        let result = self
            .refuse_in_coordinator_turn(params.caller_pane.as_deref())
            .and_then(|()| self.set_coordinator_wake_caps(params.cap_hour, params.cap_day));
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_set_model(
        &mut self,
        id: String,
        params: CoordinatorSetModelParams,
    ) -> String {
        let result = self
            .refuse_in_coordinator_turn(params.caller_pane.as_deref())
            .and_then(|()| self.set_coordinator_model(params.model.as_deref()));
        self.coordinator_reply(id, result)
    }

    pub(crate) fn handle_coordinator_set_notify(
        &mut self,
        id: String,
        params: CoordinatorSetNotifyParams,
    ) -> String {
        let result = self
            .refuse_in_coordinator_turn(params.caller_pane.as_deref())
            .and_then(|()| self.set_coordinator_notify(params.enabled));
        self.coordinator_reply(id, result)
    }

    // ----- methods ---------------------------------------------------------

    /// `in_coordinator_turn`: the caller is the coordinator pane and a
    /// herdr+ turn (a wake-up or an agent message) is live, so the request
    /// did not come from the user. Absent `caller_pane` is the user.
    pub(crate) fn refuse_in_coordinator_turn(
        &self,
        caller_pane: Option<&str>,
    ) -> Result<(), CoordinatorError> {
        let Some(caller) = caller_pane
            .map(str::trim)
            .filter(|caller| !caller.is_empty())
        else {
            return Ok(());
        };
        let Some(pane) = self.existing_coordinator_pane() else {
            return Ok(());
        };
        if self.public_pane_id(pane.ws_idx, pane.pane_id).as_deref() != Some(caller) {
            return Ok(());
        }
        // Read the marker itself: an MCP server may have set it since the
        // worker's last pass. One small file, only on this path.
        let now = crate::coordinator::now_unix();
        if crate::coordinator::turn::read_live(&self.coordinator.dir, now).is_some() {
            return Err(CoordinatorError::new(
                error_code::IN_TURN,
                "refused during a herdr+ turn of the coordinator (not the user); suggest it on the board instead",
            ));
        }
        Ok(())
    }

    /// `coordinator.open`: ensure the tab, focus it, and start the
    /// coordinator when it is enabled and not up.
    pub(crate) fn open_coordinator(&mut self, now: Instant) -> Result<(), CoordinatorError> {
        if self.coordinator.phase.before_ready() && self.coordinator.enabled {
            // The tab can open before the worker is ready; the start follows.
        }
        let pane = self
            .ensure_coordinator_tab()
            .map_err(|err| CoordinatorError::new(error_code::UNAVAILABLE, err))?;
        self.state.switch_workspace_tab(pane.ws_idx, pane.tab_idx);
        self.schedule_session_save();
        self.clear_coordinator_unread();
        if self.coordinator.enabled && matches!(self.coordinator.phase, CoordPhase::Down { .. }) {
            self.begin_coordinator_start(self.coordinator.session.clone(), now);
        }
        Ok(())
    }

    /// `coordinator.open_dashboard`: clear the unread suggestions and, with
    /// `open`, open the URL (herdr's browser, else the system browser) on a
    /// short thread.
    pub(crate) fn open_coordinator_dashboard(
        &mut self,
        open: bool,
    ) -> Result<(), CoordinatorError> {
        if !self.coordinator.enabled {
            return Err(CoordinatorError::disabled());
        }
        let Some(url) = self.coordinator.dashboard_url() else {
            return Err(CoordinatorError::new(
                error_code::DASHBOARD_UNAVAILABLE,
                format!(
                    "dashboard unavailable: {}",
                    self.coordinator
                        .dashboard_error()
                        .unwrap_or_else(|| "not listening".into())
                ),
            ));
        };
        self.clear_coordinator_unread();
        if open {
            self.spawn_dashboard_open(url);
        }
        Ok(())
    }

    fn spawn_dashboard_open(&mut self, url: String) {
        #[cfg(test)]
        self.coordinator.opened.push(url.clone());
        let opener = self.coordinator.opener;
        let spawned = std::thread::Builder::new()
            .name("herdr-coordinator-open".into())
            .spawn(move || match opener(&url) {
                Ok(via) => tracing::info!(via, "coordinator: dashboard opened"),
                Err(err) => tracing::warn!(error = %err, "coordinator: dashboard not opened"),
            });
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "coordinator: cannot spawn the dashboard opener");
        }
    }

    /// `coordinator.wake`: a wake-up that bypasses the gap and the caps (the
    /// coordinator must still be idle; the worker holds it otherwise). A
    /// coordinator that is up or coming up takes the request; it stays
    /// queued until the coordinator is idle. Returns why it waits when it
    /// cannot be delivered now.
    pub(crate) fn wake_coordinator(
        &mut self,
        caller_pane: Option<&str>,
    ) -> Result<Option<String>, CoordinatorError> {
        if !self.coordinator.enabled {
            return Err(CoordinatorError::disabled());
        }
        self.refuse_in_coordinator_turn(caller_pane)?;
        if !matches!(
            self.coordinator.phase,
            CoordPhase::Running
                | CoordPhase::Migrating { .. }
                | CoordPhase::Starting { .. }
                | CoordPhase::Launching { .. }
        ) {
            return Err(CoordinatorError::new(
                error_code::NOT_RUNNING,
                "the coordinator is not running",
            ));
        }
        self.coordinator.send(WorkerMsg::Wake);
        // The worker's pass needs fresh facts to see an idle coordinator.
        self.coordinator.input_dirty = true;
        Ok(self.coordinator_wake_hold())
    }

    /// Why a wake-up cannot be delivered now, or `None` when the
    /// coordinator is idle and interactive (the worker may still wait a
    /// few seconds for it to settle). Uses the delivery's own check.
    fn coordinator_wake_hold(&self) -> Option<String> {
        let own = self.existing_coordinator_pane();
        let status = own.and_then(|own| self.coordinator_status(own));
        if status == Some("blocked") {
            return Some("coordinator is waiting on a prompt in its tab".into());
        }
        if !matches!(self.coordinator.phase, CoordPhase::Running) {
            return Some("coordinator is starting".into());
        }
        let Some(own) = own else {
            return Some("coordinator tab is gone".into());
        };
        let reason = match status {
            Some("idle" | "done") if !self.coordinator_interactive(own) => {
                "coordinator is starting"
            }
            Some("idle" | "done") => {
                let turn_live = self
                    .coordinator
                    .last_output
                    .as_ref()
                    .is_some_and(|out| out.turn.is_some());
                if !turn_live {
                    return None;
                }
                "a coordinator turn is in progress"
            }
            Some("working") => "coordinator is working",
            Some("suspended") => "coordinator is suspended",
            Some(_) => "coordinator status unknown, check its tab",
            None => "coordinator is not running in its tab",
        };
        Some(reason.into())
    }

    /// Whether the coordinator pane takes a prompt: its managed launch
    /// settled and the agent is interactive.
    fn coordinator_interactive(&self, own: CoordPane) -> bool {
        self.coordinator_terminal(own).is_some_and(|terminal| {
            !terminal.managed_agent_launch_pending()
                && (terminal.managed_agent_kind().is_none()
                    || terminal.managed_agent_interactive_ready())
        })
    }

    /// `coordinator.start`: start a coordinator that is not up, or restart a
    /// live one — with `resume` through herdr's restart (suspend, then the
    /// native resume), without it as a new session. Clears `Down`.
    pub(crate) fn start_coordinator(
        &mut self,
        resume: bool,
        caller_pane: Option<&str>,
        now: Instant,
    ) -> Result<(), CoordinatorError> {
        if !self.coordinator.enabled {
            return Err(CoordinatorError::disabled());
        }
        self.refuse_in_coordinator_turn(caller_pane)?;
        match &self.coordinator.phase {
            CoordPhase::Blocked(reason) => {
                return Err(CoordinatorError::new(
                    error_code::BLOCKED,
                    format!("the coordinator is blocked: {}", blocked_code(reason)),
                ))
            }
            CoordPhase::Unavailable(reason) => {
                return Err(CoordinatorError::new(
                    error_code::UNAVAILABLE,
                    reason.clone(),
                ))
            }
            CoordPhase::Off | CoordPhase::WaitingForLock | CoordPhase::Migrating { .. } => {
                return Err(CoordinatorError::new(
                    error_code::UNAVAILABLE,
                    "the coordinator is not ready yet",
                ))
            }
            _ => {}
        }
        self.coordinator.relaunches.clear();
        self.coordinator.clear_alert(KIND_DOWN);
        let pane = self.existing_coordinator_pane();
        let terminal = pane.and_then(|pane| self.coordinator_terminal(pane));
        let parked = terminal.is_some_and(|terminal| terminal.suspended_agent.is_some());
        let alive = !parked && terminal.is_some_and(|terminal| terminal.is_agent_terminal());
        let target = pane.and_then(|pane| self.public_pane_id(pane.ws_idx, pane.pane_id));
        match (alive, target) {
            // Parked: herdr's own activation resumes it in place; a fresh
            // start releases the record first (the pane is not free while
            // it holds one).
            (false, Some(target)) if parked && resume => {
                self.activate_agent(&target).map_err(|err| {
                    let body = self.agent_activate_error_body(err);
                    CoordinatorError::new("coordinator_restart_failed", body.message)
                })?;
                tracing::info!(
                    event = "coordinator.start",
                    outcome = "activated",
                    "coordinator resumed from its parked record"
                );
                self.coordinator.phase = CoordPhase::Running;
            }
            (false, Some(_)) if parked => {
                self.coordinator.release_suspended = true;
                self.begin_coordinator_start(None, now);
            }
            (true, Some(target)) if resume => {
                self.restart_agent(&target).map_err(|err| {
                    let body = self.agent_restart_error_body(err);
                    CoordinatorError::new("coordinator_restart_failed", body.message)
                })?;
                tracing::info!(
                    event = "coordinator.start",
                    outcome = "restart",
                    "coordinator restarting"
                );
                self.coordinator.phase = CoordPhase::Running;
            }
            (true, Some(target)) => {
                self.suspend_agent(&target).map_err(|err| {
                    let body = self.agent_suspend_error_body(err);
                    CoordinatorError::new("coordinator_restart_failed", body.message)
                })?;
                self.coordinator.release_suspended = true;
                self.begin_coordinator_start(None, now);
                tracing::info!(
                    event = "coordinator.start",
                    outcome = "fresh",
                    "coordinator restarting with a new session"
                );
            }
            _ => {
                let resume = resume.then(|| self.coordinator.session.clone()).flatten();
                self.begin_coordinator_start(resume, now);
            }
        }
        self.coordinator.persist();
        Ok(())
    }

    /// `coordinator.set_enabled`: write `coordinator.enabled` and reload.
    pub(crate) fn set_coordinator_enabled(
        &mut self,
        enabled: bool,
    ) -> Result<(), CoordinatorError> {
        self.write_coordinator_config(crate::config::ConfigEdit::CoordinatorEnabled(enabled))?;
        if self.coordinator.enabled != enabled {
            tracing::warn!(
                event = "coordinator.set_enabled",
                outcome = "applied_directly",
                "config reload did not apply coordinator.enabled; applying it in memory"
            );
            self.coordinator.set_enabled(enabled);
        }
        Ok(())
    }

    /// `coordinator.set_wake_caps`: both caps, validated, written together.
    pub(crate) fn set_coordinator_wake_caps(
        &mut self,
        cap_hour: u32,
        cap_day: u32,
    ) -> Result<(), CoordinatorError> {
        crate::config::validate_caps(cap_hour, cap_day)
            .map_err(|err| CoordinatorError::new(error_code::INVALID_CAPS, err))?;
        self.write_coordinator_config(crate::config::ConfigEdit::CoordinatorWakeCaps {
            cap_hour,
            cap_day,
        })?;
        if (self.coordinator.cfg.cap_hour, self.coordinator.cfg.cap_day) != (cap_hour, cap_day) {
            tracing::warn!(
                event = "coordinator.set_wake_caps",
                outcome = "applied_directly",
                "config reload did not apply the wake caps; applying them in memory"
            );
            let cfg = WakeCfg {
                cap_hour,
                cap_day,
                ..self.coordinator.cfg.clone()
            };
            self.coordinator.cfg = cfg.clone();
            self.coordinator.send(WorkerMsg::SetWakeCfg(cfg));
        }
        Ok(())
    }

    /// `coordinator.set_model`: `None` (or blank) is Claude's default; applies
    /// at the next launch.
    pub(crate) fn set_coordinator_model(
        &mut self,
        model: Option<&str>,
    ) -> Result<(), CoordinatorError> {
        let model = model.map(str::trim).filter(|model| !model.is_empty());
        self.write_coordinator_config(crate::config::ConfigEdit::CoordinatorModel(model))?;
        if self.coordinator.model.as_deref() != model {
            tracing::warn!(
                event = "coordinator.set_model",
                outcome = "applied_directly",
                "config reload did not apply coordinator.model; applying it in memory"
            );
            self.coordinator.model = model.map(str::to_string);
        }
        Ok(())
    }

    /// `coordinator.set_notify`.
    pub(crate) fn set_coordinator_notify(&mut self, enabled: bool) -> Result<(), CoordinatorError> {
        self.write_coordinator_config(crate::config::ConfigEdit::CoordinatorNotify(enabled))?;
        if self.coordinator.notify.enabled != enabled {
            tracing::warn!(
                event = "coordinator.set_notify",
                outcome = "applied_directly",
                "config reload did not apply coordinator.notify; applying it in memory"
            );
            self.coordinator.notify.enabled = enabled;
        }
        Ok(())
    }

    fn write_coordinator_config(
        &mut self,
        edit: crate::config::ConfigEdit<'_>,
    ) -> Result<(), CoordinatorError> {
        crate::config::write_edit(edit)
            .map_err(|err| CoordinatorError::new(error_code::CONFIG_WRITE_FAILED, err))?;
        let report = self.reload_config();
        tracing::info!(event = "coordinator.config", status = ?report.status, "coordinator setting written");
        Ok(())
    }

    /// Apply `[coordinator]` from a (re)loaded config.
    pub(crate) fn apply_coordinator_config(&mut self, config: &CoordinatorConfig) {
        self.coordinator.apply_config(config);
    }

    // ----- the scheduler pass ----------------------------------------------

    /// The coordinator's contribution to the headless loop deadline: only
    /// timed steps (a start retry, the launch give-up, the input coalesce,
    /// a notification retry or a held one); no periodic tick.
    pub(crate) fn next_coordinator_deadline(&self, now: Instant) -> Option<Instant> {
        let state = &self.coordinator;
        if !state.enabled && state.notify_ledger.pending.is_empty() {
            return None;
        }
        let phase = match &state.phase {
            CoordPhase::Starting {
                next_try,
                give_up_at,
                ..
            } => Some(*next_try.min(give_up_at)),
            CoordPhase::Launching { give_up_at } => Some(*give_up_at),
            _ => None,
        };
        let input = (state.input_dirty && state.has_worker())
            .then(|| state.last_input_at.map_or(now, |at| at + INPUT_COALESCE));
        let held = {
            let now_unix = crate::coordinator::now_unix();
            state
                .notify_ledger
                .pending
                .iter()
                .map(|pending| pending.deliver_after)
                .filter(|at| *at > now_unix)
                .min()
                .map(|at| now + Duration::from_secs(at - now_unix))
        };
        [phase, input, state.notify_retry_at, held]
            .into_iter()
            .flatten()
            .min()
    }

    /// One pass beside `handle_news_tasks`: clear unread when the tab is
    /// focused, drive the phase, post agent facts when they changed.
    /// Returns whether the read model changed.
    pub(crate) fn handle_coordinator_tasks(&mut self, now: Instant) -> bool {
        let mut changed = self.clear_coordinator_unread_when_focused();
        changed |= self.drive_coordinator(now);
        self.post_coordinator_input(now);
        changed
    }

    /// Apply a worker output (`AppEvent::CoordinatorPassFinished`). Returns
    /// whether the read model changed.
    pub(crate) fn apply_coordinator_output(&mut self, out: Box<CoordinatorPassOutput>) -> bool {
        if !self.coordinator.enabled || !self.coordinator.has_worker() {
            return false;
        }
        let mut out = *out;
        self.coordinator.engine = out.status.clone();
        self.coordinator.dashboard = out.dashboard;
        if out.dir_migrated {
            tracing::info!(
                event = "coordinator.migrate",
                outcome = "dir_moved",
                "the POC coordinator directory moved into place"
            );
        }
        match &out.status {
            EngineStatus::Blocked(BlockedReason::LockedElsewhere) => self.coordinator.alert(
                KIND_LOCKED,
                "coordinator locked elsewhere",
                Some("another herdr server (or a legacy `herdr plus run`) runs the coordinator for this config"),
            ),
            EngineStatus::Ready => self.coordinator.clear_alert(KIND_LOCKED),
            _ => {}
        }
        if let Some(migration) = out.migration.take() {
            self.finish_coordinator_migration(migration);
        }
        if let Some(Err(err)) = out.registered.take() {
            tracing::warn!(event = "coordinator.register", error = %err, "coordinator not recorded in the registry");
        }
        let new_suggestions = std::mem::take(&mut out.new_suggestions);
        if !new_suggestions.is_empty() {
            self.note_coordinator_suggestions(&new_suggestions);
        }
        for effect in std::mem::take(&mut out.effects) {
            match effect {
                Effect::Prompt {
                    seq,
                    pane,
                    text,
                    marker,
                    ..
                } => self.deliver_coordinator_wake(seq, &pane, text, marker),
                Effect::Relaunch { resume } => self.relaunch_coordinator(resume, Instant::now()),
            }
        }
        self.coordinator.last_output = Some(out);
        true
    }

    fn note_coordinator_suggestions(
        &mut self,
        suggestions: &[crate::coordinator::engine::Suggestion],
    ) {
        let state = &mut self.coordinator;
        let mut hashes = Vec::with_capacity(suggestions.len());
        for suggestion in suggestions {
            hashes.push(suggestion.hash);
        }
        let mut record = CoordinatorRecord {
            seen_suggestions: std::mem::take(&mut state.seen_suggestions),
            ..CoordinatorRecord::default()
        };
        record.remember_suggestions(hashes);
        state.seen_suggestions = record.seen_suggestions;
        state.unread = state.unread.saturating_add(suggestions.len() as u32);
        let count = state.unread;
        let plural = if count == 1 { "" } else { "s" };
        let first = &suggestions[0].text;
        let title = format!("coordinator: {count} new suggestion{plural} — {first}");
        state.queue_notification(KIND_SUGGESTIONS, &title, Some(first), false);
        state.persist();
    }

    fn clear_coordinator_unread(&mut self) -> bool {
        if self.coordinator.unread == 0 {
            return false;
        }
        self.coordinator.unread = 0;
        self.coordinator
            .notify_ledger
            .pending
            .retain(|pending| pending.kind != KIND_SUGGESTIONS);
        self.coordinator.persist();
        true
    }

    fn clear_coordinator_unread_when_focused(&mut self) -> bool {
        if self.coordinator.unread == 0 {
            return false;
        }
        let Some(pane) = self.existing_coordinator_pane() else {
            return false;
        };
        let focused = self.state.active == Some(pane.ws_idx)
            && self
                .state
                .workspaces
                .get(pane.ws_idx)
                .is_some_and(|ws| ws.active_tab == pane.tab_idx);
        focused && self.clear_coordinator_unread()
    }

    /// Drive the lifecycle; returns whether the phase changed.
    fn drive_coordinator(&mut self, now: Instant) -> bool {
        let before = self.coordinator.phase.clone();
        self.step_coordinator(now);
        let changed = before != self.coordinator.phase;
        if changed {
            self.coordinator.input_dirty = true;
            tracing::info!(event = "coordinator.phase", phase = ?self.coordinator.phase, "coordinator phase");
        }
        changed
    }

    fn step_coordinator(&mut self, now: Instant) {
        if !self.coordinator.enabled {
            if self.coordinator.has_worker() {
                self.coordinator.stop_worker();
            }
            self.coordinator.phase = CoordPhase::Off;
            return;
        }
        if let Some(reason) = self.coordinator.unavailable.clone() {
            self.coordinator.phase = CoordPhase::Unavailable(reason);
            return;
        }
        if !self.coordinator.has_worker() {
            self.spawn_coordinator_worker();
            if !matches!(self.coordinator.phase, CoordPhase::Down { .. }) {
                self.coordinator.phase = CoordPhase::WaitingForLock;
            }
            return;
        }
        match self.coordinator.engine.clone() {
            EngineStatus::WaitingForLock => {
                if !matches!(self.coordinator.phase, CoordPhase::Down { .. }) {
                    self.coordinator.phase = CoordPhase::WaitingForLock;
                }
                return;
            }
            EngineStatus::Blocked(reason) => {
                // A coordinator that is up keeps running; it just gets no
                // wake-ups until the block clears.
                if self.coordinator.phase.before_ready()
                    || matches!(self.coordinator.phase, CoordPhase::Migrating { .. })
                    || matches!(reason, BlockedReason::LockedElsewhere)
                {
                    self.coordinator.phase = CoordPhase::Blocked(reason);
                }
                return;
            }
            EngineStatus::Ready => {}
        }
        match self.coordinator.phase.clone() {
            CoordPhase::Off
            | CoordPhase::WaitingForLock
            | CoordPhase::Blocked(_)
            | CoordPhase::Unavailable(_) => {
                if !self.coordinator.migrated {
                    self.coordinator.phase = CoordPhase::Migrating { asked: false };
                } else {
                    self.resume_or_start_coordinator(now);
                }
            }
            CoordPhase::Migrating { asked: false } => {
                self.coordinator.send(WorkerMsg::Migrate);
                self.coordinator.phase = CoordPhase::Migrating { asked: true };
            }
            CoordPhase::Migrating { asked: true } | CoordPhase::Down { .. } => {}
            CoordPhase::Starting {
                resume,
                next_try,
                give_up_at,
            } => {
                if now >= next_try {
                    self.try_start_coordinator(resume, give_up_at, now);
                }
            }
            CoordPhase::Launching { give_up_at } => self.watch_coordinator_launch(give_up_at, now),
            CoordPhase::Running => self.watch_running_coordinator(),
        }
    }

    fn spawn_coordinator_worker(&mut self) {
        #[cfg(test)]
        if self.coordinator.no_worker {
            return;
        }
        let state = &self.coordinator;
        let launch = match LaunchCtx::current(state.dir.clone(), state.dashboard_port) {
            Ok(ctx) => Some(ctx),
            Err(err) => {
                tracing::warn!(error = %err, "coordinator: cannot resolve the herdr binary for the MCP config");
                None
            }
        };
        let cfg = EngineConfig {
            dir: state.dir.clone(),
            legacy_dir: legacy_dir_for(&state.dir),
            wake: state.cfg.clone(),
            dashboard_port: state.dashboard_port,
            launch,
            seen_suggestions: state.seen_suggestions.iter().copied().collect(),
        };
        let tx = self.event_tx.clone();
        // Never block on the shared event channel: the main loop may be
        // joining this worker (disable, shutdown). A full channel hands the
        // output back for a retry.
        let sink: crate::coordinator::engine::OutputSink = Box::new(move |out| {
            use tokio::sync::mpsc::error::TrySendError;
            match tx.try_send(crate::events::AppEvent::CoordinatorPassFinished(out)) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(crate::events::AppEvent::CoordinatorPassFinished(out))) => {
                    Err(out)
                }
                Err(TrySendError::Full(_)) => Ok(()),
                Err(TrySendError::Closed(_)) => {
                    tracing::debug!("coordinator: the app is gone; output dropped");
                    Ok(())
                }
            }
        });
        match WorkerHandle::spawn(cfg, sink) {
            Ok(worker) => {
                self.coordinator.worker = Some(worker);
                self.coordinator.engine = EngineStatus::WaitingForLock;
                self.coordinator.input_dirty = true;
                self.coordinator.last_input = None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "coordinator: cannot spawn the worker");
                self.coordinator.unavailable =
                    Some(format!("cannot start the coordinator worker: {err}"));
            }
        }
    }

    /// After `Ready`: a coordinator already in its pane (alive, parked, or
    /// about to be resumed by herdr's own session restore) is `Running`;
    /// otherwise start one, resuming the recorded session.
    fn resume_or_start_coordinator(&mut self, now: Instant) {
        if matches!(self.coordinator.phase, CoordPhase::Down { .. }) {
            return;
        }
        let existing = self.existing_coordinator_pane();
        let present = existing
            .and_then(|pane| self.coordinator_terminal(pane))
            .is_some_and(|terminal| {
                terminal.is_agent_terminal()
                    || terminal.managed_agent_kind().is_some()
                    || terminal.persisted_agent_session.is_some()
            });
        if let (true, Some(pane)) = (present, existing) {
            // A restored coordinator (server restart): its completions stay
            // suppressed without recreating the tab.
            self.note_coordinator_terminal(pane);
            self.coordinator.phase = CoordPhase::Running;
            self.register_running_coordinator();
        } else {
            self.begin_coordinator_start(self.coordinator.session.clone(), now);
        }
    }

    fn begin_coordinator_start(&mut self, resume: Option<String>, now: Instant) {
        self.coordinator.phase = CoordPhase::Starting {
            resume,
            next_try: now,
            give_up_at: now + START_TIMEOUT,
        };
    }

    /// One start attempt: the tab, a shell prompt, then `start_agent`.
    fn try_start_coordinator(&mut self, resume: Option<String>, give_up_at: Instant, now: Instant) {
        let retry = |app: &mut App, resume: Option<String>| {
            if now >= give_up_at {
                app.coordinator.go_down(
                    down_reason::START_FAILED,
                    "the coordinator pane never reached its shell prompt",
                );
            } else {
                app.coordinator.phase = CoordPhase::Starting {
                    resume,
                    next_try: now + START_RETRY,
                    give_up_at,
                };
            }
        };
        let pane = match self.ensure_coordinator_tab() {
            Ok(pane) => pane,
            Err(err) => {
                self.coordinator.go_down(down_reason::START_FAILED, &err);
                return;
            }
        };
        if self.coordinator.release_suspended && !self.release_coordinator_suspended(pane) {
            retry(self, resume);
            return;
        }
        if !self.coordinator_pane_at_shell(pane) {
            retry(self, resume);
            return;
        }
        let Some(pane_id) = self.public_pane_id(pane.ws_idx, pane.pane_id) else {
            retry(self, resume);
            return;
        };
        let state = &self.coordinator;
        let launched =
            LaunchCtx::current(state.dir.clone(), state.dashboard_port).and_then(|ctx| {
                coordinator_launch_args(&ctx, resume.as_deref(), state.model.as_deref())
            });
        let (id, args) = match launched {
            Ok(launched) => launched,
            Err(err) => {
                self.coordinator.go_down(
                    down_reason::START_FAILED,
                    &format!("cannot prepare the coordinator launch: {err}"),
                );
                return;
            }
        };
        let result = self.start_agent(AgentStartParams {
            name: COORDINATOR_AGENT_NAME.into(),
            kind: "claude".into(),
            pane_id: pane_id.clone(),
            args,
            timeout_ms: Some(LAUNCH_TIMEOUT_MS),
        });
        match result {
            Ok(_) => {
                tracing::info!(event = "coordinator.start", outcome = "launched", pane = %pane_id, resume = resume.is_some(), "coordinator launched");
                self.coordinator.session = Some(id);
                self.coordinator.phase = CoordPhase::Launching {
                    give_up_at: now + LAUNCH_GIVE_UP,
                };
                self.coordinator.persist();
            }
            Err(super::agents::AgentStartError::TargetBusy(_)) => retry(self, resume),
            Err(super::agents::AgentStartError::DuplicateName { .. }) => {
                self.coordinator.go_down(
                    down_reason::NAME_TAKEN,
                    "another agent is named `coordinator` (close the old herdr+ group)",
                );
            }
            Err(err) => {
                let body = self.agent_start_error_body(err);
                self.coordinator
                    .go_down(down_reason::START_FAILED, &body.message);
            }
        }
    }

    /// A fresh start after suspending a live coordinator: drop the parked
    /// record once the exit is observed. `true` when the pane is free.
    fn release_coordinator_suspended(&mut self, pane: CoordPane) -> bool {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(pane.ws_idx)
            .and_then(|ws| ws.terminal_id(pane.pane_id))
            .cloned()
        else {
            return false;
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return false;
        };
        match terminal.suspended_agent.as_ref() {
            None => {
                self.coordinator.release_suspended = false;
                true
            }
            Some(record) if record.exit_observed() => {
                // The record and the name it kept on the pane both go: the
                // fresh session takes the name again through `start_agent`.
                terminal.take_suspended_agent();
                terminal.clear_agent_name();
                self.coordinator.release_suspended = false;
                self.state.mark_session_dirty();
                self.schedule_session_save();
                true
            }
            Some(_) => false,
        }
    }

    /// Follow herdr's managed launch: interactive means `Running` (and the
    /// worker records the coordinator); a launch that is gone or still
    /// pending at `give_up_at` means `Down`.
    fn watch_coordinator_launch(&mut self, give_up_at: Instant, now: Instant) {
        // Settle the launch the way `agent.get` does: past the settle delay
        // an idle report makes it interactive even when no further state
        // update arrives (the loop wakes at the launch's own deadline).
        let target = self
            .existing_coordinator_pane()
            .and_then(|pane| self.public_pane_id(pane.ws_idx, pane.pane_id));
        if let Some(target) = target {
            self.reconcile_managed_agent_target(&target);
        }
        let terminal = self
            .existing_coordinator_pane()
            .and_then(|pane| self.coordinator_terminal(pane));
        let (pending, ready, prompt) = terminal.map_or((false, false, false), |terminal| {
            (
                terminal.managed_agent_launch_pending(),
                terminal.managed_agent_interactive_ready(),
                terminal.state == crate::detect::AgentState::Blocked,
            )
        });
        if ready {
            self.coordinator.phase = CoordPhase::Running;
            self.coordinator.clear_alert(KIND_DOWN);
            self.coordinator.clear_alert(KIND_BLOCKED);
            self.register_running_coordinator();
        } else if !pending {
            self.coordinator
                .go_down(down_reason::START_FAILED, "the coordinator did not come up");
        } else if prompt {
            // A prompt at launch (Claude's folder trust after the directory
            // moved, a resume question): herdr's managed launch waits on the
            // user without a deadline, and so does the coordinator. The
            // give-up moves on in half-window steps so the phase (and its
            // log line) changes rarely, and an answer just before the old
            // give-up is not taken for a timeout.
            self.coordinator.alert(
                KIND_BLOCKED,
                "coordinator needs you",
                Some("the coordinator is waiting on a prompt in its tab"),
            );
            if give_up_at < now + LAUNCH_GIVE_UP / 2 {
                self.coordinator.phase = CoordPhase::Launching {
                    give_up_at: now + LAUNCH_GIVE_UP,
                };
            }
        } else if now >= give_up_at {
            self.coordinator.go_down(
                down_reason::LAUNCH_TIMEOUT,
                "the coordinator launch timed out",
            );
        }
    }

    /// Record the running coordinator in the registry (on the worker).
    fn register_running_coordinator(&mut self) {
        let Some(pane) = self.existing_coordinator_pane() else {
            return;
        };
        let Some(pane_id) = self.public_pane_id(pane.ws_idx, pane.pane_id) else {
            return;
        };
        let session = self
            .coordinator_terminal(pane)
            .and_then(terminal_session)
            .or_else(|| self.coordinator.session.clone());
        if session.is_some() && session != self.coordinator.session {
            self.coordinator.session = session.clone();
            self.coordinator.persist();
        }
        self.coordinator.send(WorkerMsg::RegisterCoordinator {
            pane: pane_id,
            session,
            agent: "claude".into(),
        });
    }

    /// While running: the coordinator blocked on a prompt raises its own
    /// alert once (the generic NeedsAttention toast is suppressed).
    fn watch_running_coordinator(&mut self) {
        let pane = self.existing_coordinator_pane();
        // Parked with no relaunch owed: a restart herdr abandoned (or the
        // user parked it). Nothing will bring it back on its own.
        let parked = pane
            .and_then(|pane| self.coordinator_terminal(pane))
            .and_then(|terminal| terminal.suspended_agent.as_ref())
            .is_some_and(|record| record.resume_pending().is_none());
        if parked {
            self.coordinator.go_down(
                down_reason::START_FAILED,
                "the coordinator is suspended (a restart was abandoned or it was parked)",
            );
            return;
        }
        let blocked = pane.and_then(|pane| self.coordinator_status(pane)) == Some("blocked");
        if blocked {
            self.coordinator.alert(
                KIND_BLOCKED,
                "coordinator needs you",
                Some("the coordinator is waiting on a prompt in its tab"),
            );
        } else {
            self.coordinator.clear_alert(KIND_BLOCKED);
        }
    }

    /// A relaunch trigger from the worker (the coordinator went missing).
    fn relaunch_coordinator(&mut self, resume: Option<String>, now: Instant) {
        if !matches!(self.coordinator.phase, CoordPhase::Running) {
            return;
        }
        let now_unix = crate::coordinator::now_unix();
        self.coordinator
            .relaunches
            .retain(|at| now_unix.saturating_sub(*at) < HOUR_S);
        if self.coordinator.relaunches.len() >= self.coordinator.relaunch_cap_hour as usize {
            let detail = format!(
                "{} relaunches in the last hour",
                self.coordinator.relaunches.len()
            );
            self.coordinator.go_down(down_reason::RELAUNCH_CAP, &detail);
            return;
        }
        self.coordinator.relaunches.push(now_unix);
        self.coordinator.persist();
        // A parked coordinator comes back through herdr's own activation
        // (the native resume command); otherwise start it with --resume.
        let parked = self.existing_coordinator_pane().and_then(|pane| {
            let terminal = self.coordinator_terminal(pane)?;
            terminal.suspended_agent.as_ref()?;
            self.public_pane_id(pane.ws_idx, pane.pane_id)
        });
        if let Some(target) = parked {
            match self.activate_agent(&target) {
                Ok(_) => {
                    tracing::info!(
                        event = "coordinator.relaunch",
                        outcome = "activated",
                        "coordinator resumed from its parked record"
                    );
                    return;
                }
                Err(err) => {
                    let body = self.agent_activate_error_body(err);
                    tracing::info!(event = "coordinator.relaunch", error = %body.message, "activation failed; starting with --resume");
                }
            }
        }
        let resume = resume.or_else(|| self.coordinator.session.clone());
        tracing::info!(
            event = "coordinator.relaunch",
            resume = resume.is_some(),
            "relaunching the coordinator"
        );
        self.begin_coordinator_start(resume, now);
    }

    /// The worker retired the POC's coordinator entry: rename a live POC
    /// coordinator out of the way, then start in the new tab with its
    /// session.
    fn finish_coordinator_migration(&mut self, migration: MigrationOutcome) {
        if !matches!(self.coordinator.phase, CoordPhase::Migrating { .. }) {
            return;
        }
        if let Some(err) = &migration.error {
            tracing::warn!(event = "coordinator.migrate", error = %err, "could not retire the POC coordinator entry");
        }
        let own = self
            .existing_coordinator_pane()
            .map(|pane| (pane.ws_idx, pane.pane_id));
        let legacy = self.agent_panes_named(COORDINATOR_AGENT_NAME, own);
        let mut renamed = false;
        for target in legacy {
            match self.rename_agent_target(&target, Some(LEGACY_AGENT_NAME.into())) {
                Ok(_) => {
                    renamed = true;
                    tracing::info!(event = "coordinator.migrate", pane = %target, "renamed the POC coordinator to coordinator-legacy");
                }
                Err(_) => {
                    tracing::warn!(event = "coordinator.migrate", pane = %target, "could not rename the POC coordinator");
                }
            }
        }
        if migration.legacy_session.is_some() || renamed {
            self.coordinator.notice = Some(MIGRATION_NOTICE.into());
        }
        self.coordinator.migrated = true;
        if self.coordinator.session.is_none() {
            self.coordinator.session = migration.legacy_session.clone();
        }
        self.coordinator.persist();
        let now = Instant::now();
        let resume = self.coordinator.session.clone();
        self.begin_coordinator_start(resume, now);
    }

    /// Public pane ids of live agents named `name`, except the given pane.
    fn agent_panes_named(
        &self,
        name: &str,
        except: Option<(usize, crate::layout::PaneId)>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for pane_id in tab.layout.pane_ids() {
                    if except == Some((ws_idx, pane_id)) {
                        continue;
                    }
                    let named = tab
                        .terminal_id(pane_id)
                        .and_then(|id| self.state.terminals.get(id))
                        .is_some_and(|terminal| terminal.agent_name.as_deref() == Some(name));
                    if named {
                        if let Some(public) = self.public_pane_id(ws_idx, pane_id) {
                            out.push(public);
                        }
                    }
                }
            }
        }
        out
    }

    /// Deliver a wake-up the worker prepared: re-check the coordinator is
    /// idle and interactive, queue the prompt, and report the outcome when
    /// the pane write finished.
    fn deliver_coordinator_wake(
        &mut self,
        seq: u64,
        pane: &str,
        text: String,
        marker: crate::coordinator::turn::Turn,
    ) {
        let reply = |state: &mut CoordinatorState, outcome: WakeOutcome| {
            state.send(WorkerMsg::WakeOutcome {
                seq,
                marker: marker.clone(),
                outcome,
            });
        };
        let ours = self
            .existing_coordinator_pane()
            .filter(|own| self.public_pane_id(own.ws_idx, own.pane_id).as_deref() == Some(pane));
        let Some(own) = ours else {
            reply(&mut self.coordinator, WakeOutcome::Held("moved".into()));
            return;
        };
        let status = self.coordinator_status(own).unwrap_or("offline");
        let interactive = self.coordinator_interactive(own);
        if !matches!(status, "idle" | "done") || !interactive {
            reply(&mut self.coordinator, WakeOutcome::Held(status.to_string()));
            return;
        }
        let queued = self.queue_agent_prompt(
            format!("coordinator:wake-{seq}"),
            AgentPromptParams {
                target: pane.to_string(),
                text,
                wait: None,
            },
        );
        match queued {
            Ok((_, _, completion)) => {
                #[cfg(test)]
                if self.coordinator.no_worker {
                    reply(&mut self.coordinator, WakeOutcome::Delivered);
                    return;
                }
                let Some(tx) = self.coordinator.worker.as_ref().map(WorkerHandle::sender) else {
                    return;
                };
                let spawned = std::thread::Builder::new()
                    .name("herdr-coordinator-wake".into())
                    .spawn(move || {
                        let outcome = match completion.recv() {
                            Ok(Ok(())) => WakeOutcome::Delivered,
                            Ok(Err(err)) => WakeOutcome::Failed(err.kind().to_string()),
                            Err(_) => WakeOutcome::Failed("pty_closed".into()),
                        };
                        let _ = tx.send(WorkerMsg::WakeOutcome {
                            seq,
                            marker,
                            outcome,
                        });
                    });
                if let Err(err) = spawned {
                    tracing::warn!(error = %err, "coordinator: cannot spawn the wake reporter");
                }
            }
            Err(response) => {
                reply(
                    &mut self.coordinator,
                    WakeOutcome::Failed(error_code(&response)),
                );
            }
        }
    }

    /// Post the agent facts to the worker when they changed (at most once
    /// per [`INPUT_COALESCE`]).
    fn post_coordinator_input(&mut self, now: Instant) {
        if !self.coordinator.input_dirty || !self.coordinator.has_worker() {
            return;
        }
        if self
            .coordinator
            .last_input_at
            .is_some_and(|at| now < at + INPUT_COALESCE)
        {
            return;
        }
        self.coordinator.input_dirty = false;
        self.coordinator.last_input_at = Some(now);
        let input = self.build_coordinator_input();
        if self.coordinator.last_input.as_ref() == Some(&input) {
            return;
        }
        self.coordinator.last_input = Some(input.clone());
        self.coordinator.send(WorkerMsg::Pass(Box::new(input)));
    }

    /// The agent facts from scalar terminal accessors and public ids only:
    /// no cwd, no process or foreground inspection, no JSON.
    pub(crate) fn build_coordinator_input(&self) -> CoordinatorPassInput {
        let mut agents = Vec::new();
        let mut groups = Vec::with_capacity(self.state.workspaces.len());
        let mut tab_labels = HashMap::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            let workspace_id = self.public_workspace_id(ws_idx);
            groups.push(GroupFact {
                workspace_id: workspace_id.clone(),
                label: ws
                    .custom_name
                    .clone()
                    .unwrap_or_else(|| ws.cached_auto_label.clone()),
                tab_count: ws.tabs.len() as u64,
            });
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                let Some(tab_id) = self.public_tab_id(ws_idx, tab_idx) else {
                    continue;
                };
                if let Some(label) = tab.custom_name.as_ref().filter(|label| !label.is_empty()) {
                    tab_labels.insert(tab_id.clone(), label.clone());
                }
                for pane_id in tab.layout.pane_ids() {
                    let Some(pane) = ws.pane_state(pane_id) else {
                        continue;
                    };
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if !terminal.is_agent_terminal() {
                        continue;
                    }
                    let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
                        continue;
                    };
                    let suspended = terminal.suspended_agent.as_ref();
                    agents.push(CoordinatorAgentFact {
                        pane_id: public,
                        tab_id: tab_id.clone(),
                        workspace_id: workspace_id.clone(),
                        name: terminal.agent_name.clone(),
                        display_agent: None,
                        agent: terminal
                            .effective_agent_label()
                            .map(str::to_string)
                            .or_else(|| suspended.map(|record| record.agent.clone())),
                        status: status_text(super::api_helpers::agent_status(
                            terminal.state,
                            pane.seen,
                            suspended.is_some(),
                        ))
                        .to_string(),
                        session: terminal_session(terminal),
                        cwd: None,
                        subagents: u64::from(terminal.active_subagent_count()),
                    });
                }
            }
        }
        CoordinatorPassInput {
            agents,
            groups,
            tab_labels,
            coordinator_down: matches!(self.coordinator.phase, CoordPhase::Down { .. }),
        }
    }

    // ----- the tab ---------------------------------------------------------

    /// The coordinator tab: the stored one while it still exists and is
    /// still labelled `coordinator`, else a new tab in the first space (not
    /// focused) whose shell starts in the coordinator directory.
    fn ensure_coordinator_tab(&mut self) -> Result<CoordPane, String> {
        if let Some(pane) = self.existing_coordinator_pane() {
            self.note_coordinator_terminal(pane);
            return Ok(pane);
        }
        if self.state.workspaces.is_empty() {
            return Err("no space to open the coordinator tab in".into());
        }
        let ws_idx = 0;
        let cwd = if self.coordinator.dir.is_dir() {
            self.coordinator.dir.clone()
        } else {
            std::env::temp_dir()
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let shell_mode = self.state.shell_mode;
        let (tab_idx, terminal, runtime) = self.state.workspaces[ws_idx]
            .create_tab(
                rows,
                cols,
                cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                crate::pane::PaneShellConfig::new(&default_shell, shell_mode),
                Vec::new(),
            )
            .map_err(|err| format!("failed to open the coordinator tab: {err}"))?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
        tab.set_custom_name(COORDINATOR_TAB_LABEL.into());
        let pane_id = tab.root_pane;
        self.state.remove_alias_shadowed_by_new_pane(pane_id);
        self.coordinator.tab_id = self.public_tab_id(ws_idx, tab_idx);
        self.coordinator.pane_id = self.public_pane_id(ws_idx, pane_id);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_tab_created_events(ws_idx, tab_idx);
        let pane = CoordPane {
            ws_idx,
            tab_idx,
            pane_id,
        };
        self.note_coordinator_terminal(pane);
        self.coordinator.persist();
        tracing::info!(
            event = "coordinator.tab",
            outcome = "created",
            tab_id = self.coordinator.tab_id.as_deref().unwrap_or(""),
            "coordinator tab created"
        );
        Ok(pane)
    }

    fn note_coordinator_terminal(&mut self, pane: CoordPane) {
        self.coordinator.terminal_id = self
            .state
            .workspaces
            .get(pane.ws_idx)
            .and_then(|ws| ws.terminal_id(pane.pane_id))
            .cloned();
        self.state.coordinator_terminal_id = self.coordinator.terminal_id.clone();
    }

    /// The stored tab, only while it is still labelled `coordinator` (a
    /// renamed tab or a user's shell is never adopted).
    fn existing_coordinator_pane(&self) -> Option<CoordPane> {
        let (ws_idx, tab_idx) = self.parse_tab_id(self.coordinator.tab_id.as_deref()?)?;
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        if tab.custom_name.as_deref() != Some(COORDINATOR_TAB_LABEL) {
            return None;
        }
        Some(CoordPane {
            ws_idx,
            tab_idx,
            pane_id: tab.root_pane,
        })
    }

    fn coordinator_terminal(&self, pane: CoordPane) -> Option<&crate::terminal::TerminalState> {
        let ws = self.state.workspaces.get(pane.ws_idx)?;
        self.state.terminals.get(ws.terminal_id(pane.pane_id)?)
    }

    /// The coordinator pane's API status text.
    fn coordinator_status(&self, pane: CoordPane) -> Option<&'static str> {
        let ws = self.state.workspaces.get(pane.ws_idx)?;
        let seen = ws.pane_state(pane.pane_id)?.seen;
        let terminal = self.coordinator_terminal(pane)?;
        if !terminal.is_agent_terminal() {
            return None;
        }
        Some(status_text(super::api_helpers::agent_status(
            terminal.state,
            seen,
            terminal.suspended_agent.is_some(),
        )))
    }

    /// Whether the coordinator pane's foreground process is its shell.
    fn coordinator_pane_at_shell(&self, pane: CoordPane) -> bool {
        #[allow(unused_mut)] // the test override below assigns it
        let mut ready = self
            .lookup_runtime_sender(pane.ws_idx, pane.pane_id)
            .is_some_and(|runtime| super::agents::available_shell_name(runtime).is_some());
        #[cfg(test)]
        {
            ready = ready || self.coordinator.assume_shell_ready;
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::coordinator::engine::{BoardSummary, Suggestion};
    use crate::coordinator::live::LiveAgent;

    fn coordinator_app(enabled: bool) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let config = Config::default();
        let mut app = App::new(
            &config,
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("bucket")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        let mut coordinator = CoordinatorState::in_memory(&CoordinatorConfig {
            enabled,
            ..CoordinatorConfig::default()
        });
        coordinator.dir = std::env::temp_dir().join(format!(
            "herdr-app-coordinator-{}-{}",
            std::process::id(),
            crate::coordinator::launch::new_uuid()
        ));
        coordinator.no_worker = true;
        coordinator.assume_shell_ready = true;
        coordinator.opener = |_| Ok("test");
        coordinator.unavailable = None;
        app.coordinator = coordinator;
        app
    }

    /// Open the coordinator tab and swap its shell for a child-less test
    /// runtime: the shell-prompt probes then answer deterministically
    /// instead of racing a real shell's startup.
    fn quiet_tab(app: &mut App) {
        let pane = app.ensure_coordinator_tab().expect("the coordinator tab");
        let id = app.state.workspaces[pane.ws_idx]
            .terminal_id(pane.pane_id)
            .expect("terminal")
            .clone();
        let (runtime, input) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(id, runtime);
        // Keep the pane's input open for the rest of the test (typed launch
        // commands and prompts must not fail on a closed channel).
        std::mem::forget(input);
    }

    fn output(status: EngineStatus) -> Box<CoordinatorPassOutput> {
        Box::new(CoordinatorPassOutput {
            status,
            dashboard: DashboardState::Listening(7799),
            effects: Vec::new(),
            summary: None,
            turn: None,
            board: None,
            new_suggestions: Vec::new(),
            managed: Vec::new(),
            offline: Vec::new(),
            coordinator_session: None,
            coordinator_pane: None,
            migration: None,
            registered: None,
            dir_migrated: false,
        })
    }

    fn ready(app: &mut App) {
        app.apply_coordinator_output(output(EngineStatus::Ready));
    }

    fn coordinator_terminal_mut(app: &mut App) -> &mut crate::terminal::TerminalState {
        let pane = app.existing_coordinator_pane().expect("tab");
        let id = app.state.workspaces[pane.ws_idx]
            .terminal_id(pane.pane_id)
            .expect("terminal")
            .clone();
        app.state.terminals.get_mut(&id).expect("terminal state")
    }

    /// Enabled, ready, migrated: one pass starts the coordinator.
    fn launching(app: &mut App) -> Instant {
        quiet_tab(app);
        let now = Instant::now();
        app.coordinator.migrated = true;
        app.handle_coordinator_tasks(now);
        ready(app);
        app.handle_coordinator_tasks(now);
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Starting { .. }),
            "{:?}",
            app.coordinator.phase
        );
        app.handle_coordinator_tasks(now);
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Launching { .. }),
            "{:?}",
            app.coordinator.phase
        );
        now
    }

    #[tokio::test]
    async fn enabling_waits_for_the_lock_then_starts_launches_and_runs() {
        let mut app = coordinator_app(true);
        quiet_tab(&mut app);
        let now = Instant::now();
        app.handle_coordinator_tasks(now);
        assert_eq!(app.coordinator.phase, CoordPhase::WaitingForLock);
        assert_eq!(
            app.coordinator_get_info().state,
            CoordinatorStateInfo::WaitingForLock
        );
        ready(&mut app);
        app.handle_coordinator_tasks(now);
        assert_eq!(
            app.coordinator.phase,
            CoordPhase::Migrating { asked: false }
        );
        app.handle_coordinator_tasks(now);
        assert!(matches!(
            app.coordinator.sent.last(),
            Some(WorkerMsg::Migrate)
        ));
        let mut out = output(EngineStatus::Ready);
        out.migration = Some(MigrationOutcome {
            legacy_session: None,
            legacy_pane: None,
            error: None,
        });
        app.apply_coordinator_output(out);
        assert!(app.coordinator.migrated);
        assert!(
            app.coordinator.notice.is_none(),
            "nothing to migrate: no notice"
        );
        assert!(matches!(app.coordinator.phase, CoordPhase::Starting { .. }));
        let now = Instant::now();
        app.handle_coordinator_tasks(now);
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Launching { .. }),
            "{:?}",
            app.coordinator.phase
        );
        let pane = app
            .existing_coordinator_pane()
            .expect("the coordinator tab");
        assert_eq!(
            app.state.workspaces[0].active_tab, 0,
            "the tab is not focused"
        );
        assert_eq!(
            app.state.workspaces[0].tabs[pane.tab_idx]
                .custom_name
                .as_deref(),
            Some(COORDINATOR_TAB_LABEL)
        );
        let terminal = coordinator_terminal_mut(&mut app);
        assert_eq!(terminal.agent_name.as_deref(), Some(COORDINATOR_AGENT_NAME));
        assert!(terminal.managed_agent_launch_pending());
        // herdr's own launch lifecycle settles it.
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        let settle = now + Duration::from_secs(5);
        terminal.reconcile_managed_agent_at(settle, false);
        assert!(terminal.managed_agent_interactive_ready(), "launch settled");
        app.handle_coordinator_tasks(settle);
        assert_eq!(app.coordinator.phase, CoordPhase::Running);
        assert!(app
            .coordinator
            .sent
            .iter()
            .any(|msg| matches!(msg, WorkerMsg::RegisterCoordinator { .. })));
        let info = app.coordinator_get_info();
        assert_eq!(info.state, CoordinatorStateInfo::Running);
        assert_eq!(
            info.dashboard_url.as_deref(),
            Some("http://127.0.0.1:7799/")
        );
        assert!(info.coordinator_session.is_some());
    }

    #[tokio::test]
    async fn a_prompt_at_launch_waits_for_the_user_instead_of_timing_out() {
        let mut app = coordinator_app(true);
        let now = launching(&mut app);
        let terminal = coordinator_terminal_mut(&mut app);
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Blocked,
        );
        terminal.reconcile_managed_agent_at(now, false);
        assert!(terminal.managed_agent_launch_pending(), "blocked at launch");
        let late = now + LAUNCH_GIVE_UP + Duration::from_secs(1);
        app.handle_coordinator_tasks(late);
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Launching { give_up_at } if give_up_at > late),
            "{:?}",
            app.coordinator.phase
        );
        assert_eq!(app.coordinator.notify_ledger.pending.len(), 1);
        assert_eq!(app.coordinator.notify_ledger.pending[0].kind, KIND_BLOCKED);
        app.handle_coordinator_tasks(late + Duration::from_secs(1));
        assert_eq!(
            app.coordinator.notify_ledger.pending.len(),
            1,
            "alerted once"
        );

        // The user answers: the launch settles and the coordinator runs.
        let terminal = coordinator_terminal_mut(&mut app);
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.reconcile_managed_agent_at(late, false);
        assert!(terminal.managed_agent_interactive_ready());
        app.handle_coordinator_tasks(late + Duration::from_secs(2));
        assert_eq!(app.coordinator.phase, CoordPhase::Running);
        assert!(app.coordinator.notify_ledger.pending.is_empty());
    }

    #[tokio::test]
    async fn a_busy_pane_retries_then_goes_down() {
        let mut app = coordinator_app(true);
        quiet_tab(&mut app);
        app.coordinator.migrated = true;
        app.coordinator.assume_shell_ready = false;
        let now = Instant::now();
        app.handle_coordinator_tasks(now);
        ready(&mut app);
        app.handle_coordinator_tasks(now);
        // A pane with a real foreground process that is not a shell: make
        // it look busy by occupying it with an agent.
        app.handle_coordinator_tasks(now);
        let pane = app.existing_coordinator_pane().expect("tab");
        let _ = pane;
        // The test runtime has no child: available_shell_name says "sh",
        // so mark the terminal as an agent to get TargetBusy instead.
        coordinator_terminal_mut(&mut app).set_agent_name("someone".into());
        app.coordinator.phase = CoordPhase::Starting {
            resume: None,
            next_try: now,
            give_up_at: now + START_TIMEOUT,
        };
        app.handle_coordinator_tasks(now);
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Starting { .. }),
            "retrying"
        );
        assert!(app
            .next_coordinator_deadline(now)
            .is_some_and(|at| at <= now + START_RETRY));
        app.handle_coordinator_tasks(now + START_TIMEOUT);
        assert!(
            matches!(&app.coordinator.phase, CoordPhase::Down { reason, .. } if reason == "start_failed"),
            "{:?}",
            app.coordinator.phase
        );
        assert_eq!(app.coordinator.notify_ledger.pending.len(), 1);
        assert_eq!(app.coordinator.notify_ledger.pending[0].kind, KIND_DOWN);
    }

    #[tokio::test]
    async fn restart_while_running_goes_through_restart_agent() {
        let mut app = coordinator_app(true);
        let now = launching(&mut app);
        let terminal = coordinator_terminal_mut(&mut app);
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        terminal.reconcile_managed_agent_at(now + Duration::from_secs(5), false);
        app.handle_coordinator_tasks(now + Duration::from_secs(5));
        assert_eq!(app.coordinator.phase, CoordPhase::Running);
        coordinator_terminal_mut(&mut app).set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Working,
        );
        // A working agent cannot restart: restart_agent's own guard answers,
        // and start_agent (which would say agent_pane_busy) is not used.
        let err = app.start_coordinator(true, None, now).unwrap_err();
        assert_eq!(err.code, "coordinator_restart_failed");
        assert_eq!(app.coordinator.phase, CoordPhase::Running);
    }

    fn park_coordinator(app: &mut App) {
        let session = crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id(
                "0b1c2d3e-0000-4000-8000-000000000001",
            )
            .expect("a valid session id"),
            transcript_path: None,
        };
        coordinator_terminal_mut(app).restore_suspended_agent(
            "claude".into(),
            Some(COORDINATOR_AGENT_NAME.into()),
            session,
        );
    }

    #[tokio::test]
    async fn a_parked_coordinator_with_no_relaunch_owed_goes_down() {
        let mut app = coordinator_app(true);
        launching(&mut app);
        app.coordinator.phase = CoordPhase::Running;
        park_coordinator(&mut app);
        app.handle_coordinator_tasks(Instant::now());
        assert!(
            matches!(&app.coordinator.phase, CoordPhase::Down { reason, .. } if reason == down_reason::START_FAILED),
            "{:?}",
            app.coordinator.phase
        );
    }

    #[tokio::test]
    async fn a_fresh_start_releases_a_parked_coordinator_then_launches() {
        let mut app = coordinator_app(true);
        launching(&mut app);
        park_coordinator(&mut app);
        app.coordinator.phase = CoordPhase::Down {
            since: 1,
            reason: down_reason::START_FAILED.into(),
        };
        app.start_coordinator(false, None, Instant::now()).unwrap();
        assert!(app.coordinator.release_suspended);
        app.handle_coordinator_tasks(Instant::now());
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Launching { .. }),
            "{:?}",
            app.coordinator.phase
        );
        let terminal = coordinator_terminal_mut(&mut app);
        assert!(terminal.suspended_agent.is_none(), "the record is released");
        assert_eq!(terminal.agent_name.as_deref(), Some(COORDINATOR_AGENT_NAME));
        assert!(terminal.managed_agent_launch_pending(), "a fresh launch");
        assert!(!app.coordinator.release_suspended);
    }

    #[tokio::test]
    async fn a_resumed_start_activates_a_parked_coordinator() {
        let mut app = coordinator_app(true);
        launching(&mut app);
        park_coordinator(&mut app);
        app.coordinator.phase = CoordPhase::Down {
            since: 1,
            reason: down_reason::START_FAILED.into(),
        };
        // Activation goes through herdr's own path, not start_agent (which
        // would refuse the parked pane as busy).
        app.start_coordinator(true, None, Instant::now())
            .expect("activation");
        assert_eq!(app.coordinator.phase, CoordPhase::Running);
        assert!(coordinator_terminal_mut(&mut app).suspended_agent.is_none());
        assert!(!app.coordinator.release_suspended);
    }

    #[test]
    fn a_relaunch_over_the_cap_goes_down_with_one_notification() {
        let mut app = coordinator_app(true);
        app.coordinator.relaunch_cap_hour = 2;
        app.coordinator.phase = CoordPhase::Running;
        app.coordinator.engine = EngineStatus::Ready;
        let now = Instant::now();
        let relaunch = |app: &mut App| {
            let mut out = output(EngineStatus::Ready);
            out.effects.push(Effect::Relaunch {
                resume: Some("s".into()),
            });
            app.apply_coordinator_output(out);
        };
        relaunch(&mut app);
        assert!(
            matches!(&app.coordinator.phase, CoordPhase::Starting { resume: Some(id), .. } if id == "s")
        );
        app.coordinator.phase = CoordPhase::Running;
        relaunch(&mut app);
        app.coordinator.phase = CoordPhase::Running;
        assert_eq!(app.coordinator.relaunches.len(), 2);
        relaunch(&mut app);
        assert!(
            matches!(&app.coordinator.phase, CoordPhase::Down { reason, .. } if reason == "relaunch_cap")
        );
        assert_eq!(app.coordinator.notify_ledger.pending.len(), 1);
        relaunch(&mut app);
        assert_eq!(
            app.coordinator.notify_ledger.pending.len(),
            1,
            "down notifies once"
        );
        assert!(app.build_coordinator_input().coordinator_down);
        assert_eq!(
            app.coordinator_get_info().down_reason.as_deref(),
            Some("relaunch_cap")
        );
        let _ = now;
    }

    #[tokio::test]
    async fn disabling_drops_the_worker_and_leaves_the_pane() {
        let mut app = coordinator_app(true);
        launching(&mut app);
        let tabs = app.state.workspaces[0].tabs.len();
        app.apply_coordinator_config(&CoordinatorConfig::default());
        app.handle_coordinator_tasks(Instant::now());
        assert_eq!(app.coordinator.phase, CoordPhase::Off);
        assert!(app.coordinator.worker.is_none());
        assert_eq!(app.state.workspaces[0].tabs.len(), tabs, "the pane stays");
        assert!(coordinator_terminal_mut(&mut app).agent_name.is_some());
        assert_eq!(app.coordinator_get_info().state, CoordinatorStateInfo::Off);
    }

    #[tokio::test]
    async fn the_tab_is_reused_only_while_its_label_matches() {
        let mut app = coordinator_app(true);
        let first = app.ensure_coordinator_tab().unwrap();
        let again = app.ensure_coordinator_tab().unwrap();
        assert_eq!(first.tab_idx, again.tab_idx);
        app.state.workspaces[0].tabs[first.tab_idx].set_custom_name("mine".into());
        assert!(
            app.existing_coordinator_pane().is_none(),
            "a renamed tab is not adopted"
        );
        let fresh = app.ensure_coordinator_tab().unwrap();
        assert_ne!(fresh.tab_idx, first.tab_idx);
        // A stored id pointing at a user's shell tab is never adopted.
        app.coordinator.tab_id = app.public_tab_id(0, 0);
        assert!(app.existing_coordinator_pane().is_none());
    }

    #[tokio::test]
    async fn migration_renames_a_live_legacy_coordinator_and_launches_with_its_session() {
        let mut app = coordinator_app(true);
        quiet_tab(&mut app);
        // The POC's coordinator, live in the first tab.
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let id = app.state.workspaces[0].terminal_id(root).unwrap().clone();
        let legacy = app.state.terminals.get_mut(&id).unwrap();
        legacy.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Idle,
        );
        legacy.set_agent_name(COORDINATOR_AGENT_NAME.into());
        let now = Instant::now();
        app.handle_coordinator_tasks(now);
        ready(&mut app);
        app.handle_coordinator_tasks(now);
        app.handle_coordinator_tasks(now);
        let mut out = output(EngineStatus::Ready);
        out.migration = Some(MigrationOutcome {
            legacy_session: Some("old-session".into()),
            legacy_pane: Some("w1:p1".into()),
            error: None,
        });
        app.apply_coordinator_output(out);
        assert_eq!(
            app.state.terminals[&id].agent_name.as_deref(),
            Some(LEGACY_AGENT_NAME)
        );
        assert_eq!(app.coordinator.notice.as_deref(), Some(MIGRATION_NOTICE));
        app.handle_coordinator_tasks(Instant::now());
        assert!(
            matches!(app.coordinator.phase, CoordPhase::Launching { .. }),
            "no agent_name_taken: {:?}",
            app.coordinator.phase
        );
        assert_eq!(app.coordinator.session.as_deref(), Some("old-session"));
    }

    #[tokio::test]
    async fn the_coordinator_cannot_change_settings_during_its_turn() {
        let mut app = coordinator_app(true);
        launching(&mut app);
        let pane = app.existing_coordinator_pane().unwrap();
        let own = app.public_pane_id(pane.ws_idx, pane.pane_id).unwrap();
        std::fs::create_dir_all(&app.coordinator.dir).unwrap();
        let now = crate::coordinator::now_unix();
        crate::coordinator::turn::write(
            &app.coordinator.dir,
            &crate::coordinator::turn::Turn {
                source: "wake".into(),
                id: "1".into(),
                started_unix: now,
                coordinator_pane: own.clone(),
                seen_working: false,
            },
        )
        .unwrap();
        let err = app.refuse_in_coordinator_turn(Some(&own)).unwrap_err();
        assert_eq!(err.code, "in_coordinator_turn");
        assert!(app.refuse_in_coordinator_turn(None).is_ok(), "the user");
        assert!(
            app.refuse_in_coordinator_turn(Some("w9:p9")).is_ok(),
            "another pane"
        );
        assert_eq!(
            app.wake_coordinator(Some(&own)).unwrap_err().code,
            "in_coordinator_turn"
        );
        crate::coordinator::turn::clear(&app.coordinator.dir);
        assert!(app.refuse_in_coordinator_turn(Some(&own)).is_ok());
        let _ = std::fs::remove_dir_all(&app.coordinator.dir);
    }

    fn suggestion(text: &str) -> Suggestion {
        Suggestion {
            hash: crate::coordinator::engine::suggestion_hash(text),
            text: text.into(),
        }
    }

    #[tokio::test]
    async fn new_suggestions_notify_and_unread_clears_on_focus_and_open_dashboard() {
        let mut app = coordinator_app(true);
        app.coordinator.phase = CoordPhase::Running;
        app.coordinator.engine = EngineStatus::Ready;
        let mut out = output(EngineStatus::Ready);
        out.new_suggestions = vec![suggestion("Ask rev?"), suggestion("Merge?")];
        out.board = Some(BoardSummary {
            generated_unix: 1,
            summary: Some("calm".into()),
            suggestions: vec![suggestion("Ask rev?"), suggestion("Merge?")],
        });
        app.apply_coordinator_output(out);
        assert_eq!(app.coordinator.unread, 2);
        let pending = &app.coordinator.notify_ledger.pending;
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0]
                .title
                .starts_with("coordinator: 2 new suggestions — Ask rev?"),
            "{}",
            pending[0].title
        );
        assert_eq!(app.coordinator.seen_suggestions.len(), 2);
        let info = app.coordinator_get_info();
        assert_eq!(info.unread_suggestions, 2);
        assert_eq!(info.board.as_ref().map(|b| b.suggestion_count), Some(2));

        // open_dashboard {open: false} answers the URL and opens nothing.
        app.open_coordinator_dashboard(false).unwrap();
        assert_eq!(app.coordinator.unread, 0);
        assert!(app.coordinator.opened.is_empty());
        assert!(
            app.coordinator.notify_ledger.pending.is_empty(),
            "the batched notice goes too"
        );
        app.open_coordinator_dashboard(true).unwrap();
        assert_eq!(
            app.coordinator.opened,
            vec!["http://127.0.0.1:7799/".to_string()]
        );

        // Focusing the coordinator tab clears it as well.
        let mut out = output(EngineStatus::Ready);
        out.new_suggestions = vec![suggestion("Close?")];
        app.apply_coordinator_output(out);
        assert_eq!(app.coordinator.unread, 1);
        let pane = app.ensure_coordinator_tab().unwrap();
        app.handle_coordinator_tasks(Instant::now());
        assert_eq!(app.coordinator.unread, 1, "not focused yet");
        app.state.switch_workspace_tab(pane.ws_idx, pane.tab_idx);
        assert!(app.handle_coordinator_tasks(Instant::now()));
        assert_eq!(app.coordinator.unread, 0);
    }

    #[test]
    fn open_dashboard_without_a_listener_is_unavailable() {
        let mut app = coordinator_app(true);
        app.coordinator.dashboard = DashboardState::PortInUse(7718);
        let err = app.open_coordinator_dashboard(true).unwrap_err();
        assert_eq!(err.code, "dashboard_unavailable");
        assert!(err.message.contains("port 7718 in use"), "{}", err.message);
        assert!(app.coordinator.opened.is_empty());
        let mut off = coordinator_app(false);
        assert_eq!(
            off.open_coordinator_dashboard(true).unwrap_err().code,
            "coordinator_disabled"
        );
    }

    #[test]
    fn the_opener_prefers_herdrs_browser_and_falls_back_to_the_system() {
        let system = |_: &str| Ok(());
        assert_eq!(open_with("u", |_| Ok(()), system), Ok("browser"));
        assert_eq!(
            open_with("u", |_| Err("browser_disabled".into()), system),
            Ok("system")
        );
        assert_eq!(
            open_with("u", |_| Err("x".into()), |_| Err("no opener".into())),
            Err("no opener".into())
        );
    }

    #[test]
    fn an_equal_output_is_applied_but_phase_passes_report_no_change() {
        let mut app = coordinator_app(true);
        app.coordinator.phase = CoordPhase::Down {
            since: 1,
            reason: "x".into(),
        };
        app.coordinator.engine = EngineStatus::Ready;
        assert!(
            !app.handle_coordinator_tasks(Instant::now()),
            "nothing moved"
        );
        assert!(!app.handle_coordinator_tasks(Instant::now()));
    }

    #[tokio::test]
    async fn the_coordinator_terminal_is_recognised_for_suppression() {
        let mut app = coordinator_app(true);
        let pane = app.ensure_coordinator_tab().unwrap();
        let own = app.state.workspaces[0]
            .terminal_id(pane.pane_id)
            .unwrap()
            .clone();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let other = app.state.workspaces[0].terminal_id(root).unwrap().clone();
        assert!(app.is_coordinator_terminal(&own));
        assert!(!app.is_coordinator_terminal(&other));
    }

    #[test]
    fn the_pass_input_carries_scalar_facts_only() {
        let mut app = coordinator_app(true);
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let id = app.state.workspaces[0].terminal_id(root).unwrap().clone();
        let terminal = app.state.terminals.get_mut(&id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Working,
        );
        terminal.set_agent_name("lead".into());
        let input = app.build_coordinator_input();
        assert_eq!(input.agents.len(), 1);
        let fact = &input.agents[0];
        assert_eq!(fact.name.as_deref(), Some("lead"));
        assert_eq!(fact.agent.as_deref(), Some("claude"));
        assert_eq!(fact.status, "working");
        assert_eq!(fact.cwd, None, "never a cwd: no process inspection");
        assert_eq!(input.groups.len(), 1);
        // The builder's accessor set stays narrow: no cwd or foreground
        // probes anywhere in it.
        let source = include_str!("coordinator.rs");
        let start = source.find("fn build_coordinator_input").unwrap();
        let end = start + source[start..].find("\n    }\n").unwrap();
        let body = &source[start..end];
        for forbidden in [
            "cwd_for_pane",
            "foreground_cwd",
            "foreground_job",
            "pane_info",
            "agent_info",
            "collect_agent_infos",
            "child_pid",
            "display_name_from",
            "serde_json",
        ] {
            assert!(
                !body.contains(forbidden),
                "{forbidden} in build_coordinator_input"
            );
        }
    }

    #[test]
    fn input_is_posted_on_change_and_coalesced() {
        let mut app = coordinator_app(true);
        app.coordinator.engine = EngineStatus::Ready;
        app.coordinator.phase = CoordPhase::Running;
        let now = Instant::now();
        app.coordinator.input_dirty = true;
        app.post_coordinator_input(now);
        let passes = |app: &App| {
            app.coordinator
                .sent
                .iter()
                .filter(|msg| matches!(msg, WorkerMsg::Pass(_)))
                .count()
        };
        assert_eq!(passes(&app), 1);
        app.mark_coordinator_input_dirty();
        app.post_coordinator_input(now + Duration::from_millis(200));
        assert_eq!(passes(&app), 1, "coalesced");
        assert_eq!(
            app.next_coordinator_deadline(now + Duration::from_millis(200)),
            Some(now + INPUT_COALESCE)
        );
        app.post_coordinator_input(now + INPUT_COALESCE);
        assert_eq!(passes(&app), 1, "same facts: not resent");
        assert!(!app.coordinator.input_dirty);
        assert_eq!(
            app.next_coordinator_deadline(now + INPUT_COALESCE),
            None,
            "no periodic tick"
        );
    }

    #[tokio::test]
    async fn a_held_wake_is_reported_when_the_coordinator_is_busy() {
        let mut app = coordinator_app(true);
        let now = launching(&mut app);
        let pane = app.existing_coordinator_pane().unwrap();
        let own = app.public_pane_id(pane.ws_idx, pane.pane_id).unwrap();
        let terminal = coordinator_terminal_mut(&mut app);
        terminal.set_detected_state(
            Some(crate::detect::Agent::Claude),
            crate::detect::AgentState::Working,
        );
        terminal.reconcile_managed_agent_at(now + Duration::from_secs(5), false);
        let marker = crate::coordinator::turn::Turn {
            source: "wake".into(),
            id: "1".into(),
            started_unix: 1,
            coordinator_pane: own.clone(),
            seen_working: false,
        };
        let mut out = output(EngineStatus::Ready);
        out.effects.push(Effect::Prompt {
            seq: 1,
            pane: own,
            text: "wake".into(),
            marker,
            items: 1,
        });
        app.apply_coordinator_output(out);
        assert!(app.coordinator.sent.iter().any(|msg| matches!(
            msg,
            WorkerMsg::WakeOutcome {
                outcome: WakeOutcome::Held(status),
                ..
            } if status == "working"
        )));
    }

    #[tokio::test]
    async fn a_wake_that_cannot_be_delivered_says_why_and_stays_queued() {
        let mut app = coordinator_app(true);
        let now = launching(&mut app);
        let wakes = |app: &App| {
            app.coordinator
                .sent
                .iter()
                .filter(|msg| matches!(msg, WorkerMsg::Wake))
                .count()
        };
        let set = |app: &mut App, state: crate::detect::AgentState, at: Instant| {
            let terminal = coordinator_terminal_mut(app);
            terminal.set_detected_state(Some(crate::detect::Agent::Claude), state);
            terminal.reconcile_managed_agent_at(at, false);
        };

        // Launching, on a prompt (Claude's folder trust): queued, not refused.
        set(&mut app, crate::detect::AgentState::Blocked, now);
        assert_eq!(
            app.wake_coordinator(None).unwrap().as_deref(),
            Some("coordinator is waiting on a prompt in its tab")
        );
        assert_eq!(wakes(&app), 1, "the wake is queued with the worker");

        let settle = now + Duration::from_secs(5);
        set(&mut app, crate::detect::AgentState::Idle, settle);
        app.handle_coordinator_tasks(settle);
        assert_eq!(app.coordinator.phase, CoordPhase::Running);

        for (state, reason) in [
            (crate::detect::AgentState::Working, "coordinator is working"),
            (
                crate::detect::AgentState::Unknown,
                "coordinator status unknown, check its tab",
            ),
            (
                crate::detect::AgentState::Blocked,
                "coordinator is waiting on a prompt in its tab",
            ),
        ] {
            set(&mut app, state, settle);
            assert_eq!(
                app.wake_coordinator(None).unwrap().as_deref(),
                Some(reason),
                "{state:?}"
            );
        }
        set(&mut app, crate::detect::AgentState::Idle, settle);
        assert_eq!(app.wake_coordinator(None).unwrap(), None, "deliverable now");
        assert_eq!(wakes(&app), 5);

        // The reply carries it; `coordinator.get` does not.
        set(&mut app, crate::detect::AgentState::Working, settle);
        let reply = app.handle_coordinator_wake("w".into(), CoordinatorWakeParams::default());
        assert!(
            reply.contains(r#""wake_queued":"coordinator is working""#),
            "{reply}"
        );
        assert!(!app
            .handle_coordinator_get("g".into())
            .contains("wake_queued"));

        // Down still refuses.
        app.coordinator.go_down(down_reason::START_FAILED, "test");
        assert_eq!(
            app.wake_coordinator(None).unwrap_err().code,
            error_code::NOT_RUNNING
        );
    }

    #[tokio::test]
    async fn an_agent_status_change_refreshes_the_coordinator_facts() {
        let mut app = coordinator_app(true);
        let now = launching(&mut app);
        let pane = app.existing_coordinator_pane().unwrap();
        let transition = |app: &mut App, state: crate::detect::AgentState| {
            app.coordinator.input_dirty = false;
            let update = app
                .state
                .update_terminal_state(pane.pane_id, |terminal| {
                    let change =
                        terminal.set_detected_state(Some(crate::detect::Agent::Claude), state);
                    terminal.reconcile_managed_agent_at(now, false);
                    Some(crate::terminal::TerminalStateMutation {
                        effective_state_change: change,
                        session_ref_changed: false,
                        agent_released: false,
                    })
                })
                .expect("a state update");
            let relabelled = update.previous_agent_label != update.agent_label;
            app.emit_pane_state_update(&update);
            (relabelled, app.coordinator.input_dirty)
        };
        // The first one names the agent; the next ones only change status.
        assert_eq!(
            transition(&mut app, crate::detect::AgentState::Working),
            (true, true)
        );
        assert_eq!(
            transition(&mut app, crate::detect::AgentState::Idle),
            (false, true),
            "a status change alone marks the facts stale"
        );
        assert_eq!(
            transition(&mut app, crate::detect::AgentState::Working),
            (false, true)
        );
    }

    #[test]
    fn the_managed_read_model_lists_live_and_offline_agents() {
        let mut app = coordinator_app(true);
        app.coordinator.engine = EngineStatus::Ready;
        let mut out = output(EngineStatus::Ready);
        out.managed = vec![LiveAgent {
            name: "lead".into(),
            pane_id: "w1:p2".into(),
            tab_id: "w1:t2".into(),
            status: "working".into(),
            managed: true,
            role: Some("fixer".into()),
            last_change_unix: 5,
            ..LiveAgent::default()
        }];
        out.offline = vec![crate::coordinator::live::OfflineAgent {
            pane_id: Some("w1:p3".into()),
            role: Some("reviewer".into()),
            ..Default::default()
        }];
        app.apply_coordinator_output(out);
        let info = app.coordinator_get_info();
        assert_eq!(info.managed.len(), 2);
        assert_eq!(info.managed[0].status.as_deref(), Some("working"));
        assert_eq!(info.managed[0].last_change_at, Some(5));
        assert_eq!(info.managed[1].status.as_deref(), Some("offline"));
    }
}
