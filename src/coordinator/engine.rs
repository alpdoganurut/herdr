//! The coordinator worker: one thread per herdr server with the coordinator
//! enabled. It owns every file under the coordinator directory it writes
//! (the message log tail and rotation, the turn marker, wake-up
//! digests, `wakeups.log`, `live.json`, `watch_state.json`, the board) and
//! the dashboard HTTP thread, so the server's event loop never blocks on a
//! flock, file I/O or a socket. It never touches App state.
//!
//! The App sends typed [`WorkerMsg`]s (a coalesced [`CoordinatorPassInput`]
//! built from scalar accessors when agents change, wake outcomes, config
//! changes); the worker answers with a
//! [`CoordinatorPassOutput`] through the sink given to [`WorkerHandle::spawn`]
//! — only when something the App reads changed or an effect is owed, so an
//! idle worker does not wake the main loop. Its own 5 s timer covers the file
//! facts (new messages and action-log lines, board edits, the periodic check
//! and the `live.json` heartbeat).
//!
//! Agents v2: the worker never writes `managed.json` (the registry is read
//! only by the one-time migration and the POC hand-over below); the
//! coordinator is the server's own record, and the wake scope decides which
//! agents wake it.
//!
//! Lifecycle: migrate the POC directory (`plus/` → `coordinator/`), take
//! [`WATCH_LOCK`] (retrying every 30 s while another server or a legacy
//! `herdr plus run` holds it), seed the directory, then start the dashboard
//! (`Ready`). Dropping the [`WorkerHandle`] stops the dashboard and releases
//! the lock.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::launch::{self, LaunchCtx};
use super::live::{
    self, CoordinatorAgentFact, FactInputs, GroupFact, LiveAgent, LiveData, TabFact,
};
use super::lock::{self, DirLock};
use super::messages::{self, AgentMessage};
use super::serve::DashboardServer;
use super::turn::{self, Turn};
use super::watch::{self, Action, ExpectedCoordinator, WakeCfg, WatchState};
use super::{
    board_path, now_unix, one_line, wake_dir, wake_request_path, write_atomically, WATCH_LOCK,
};
use crate::agents_model::actions_log::{self, AgentActionEntry};

/// The worker's own file-polling interval.
pub const TIMER: Duration = Duration::from_secs(5);
/// Retry interval while the watcher lock is held elsewhere.
pub const LOCK_RETRY_S: u64 = 30;
/// Retry interval for a dashboard port that was in use.
pub const BIND_RETRY_S: u64 = 60;
/// Action-log lines `live.json` carries for the dashboard.
const LIVE_ACTIONS: usize = 50;
/// Board text limits: the read model carries a summary only.
const BOARD_SUMMARY_CHARS: usize = 400;
const SUGGESTION_CHARS: usize = 200;
const MAX_SUGGESTIONS: usize = 30;

/// Everything the worker needs to start.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The coordinator directory.
    pub dir: PathBuf,
    /// The POC directory to migrate from (`config_dir/plus`); `None` when
    /// the directory is overridden.
    pub legacy_dir: Option<PathBuf>,
    pub wake: WakeCfg,
    /// `0` disables serving.
    pub dashboard_port: u16,
    /// For `mcp/claude.json`; `None` skips writing it (tests).
    pub launch: Option<LaunchCtx>,
    /// Suggestion hashes already notified (from `coordinator.json`).
    pub seen_suggestions: HashSet<u64>,
}

/// One pass's agent facts, built by the App only from scalar accessors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordinatorPassInput {
    pub agents: Vec<CoordinatorAgentFact>,
    pub groups: Vec<GroupFact>,
    /// Every tab (shell tabs included).
    pub tabs: Vec<TabFact>,
    pub tab_labels: HashMap<String, String>,
    /// The App gave up relaunching the coordinator (its relaunch cap).
    pub coordinator_down: bool,
    /// The coordinator the App runs (`Running`): its pane and session. Its
    /// absence from `agents` for `missing_s` triggers a relaunch.
    pub expected_coordinator: Option<ExpectedCoordinator>,
}

/// How the App's delivery of an [`Effect::Prompt`] went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The prompt was queued into the coordinator pane.
    Delivered,
    /// Not delivered and not a failure (the coordinator was busy on the
    /// re-check): the counters and the pending items are kept.
    Held(String),
    /// The prompt failed (error code): back off by the gap.
    Failed(String),
}

#[derive(Debug)]
pub enum WorkerMsg {
    /// New agent facts; runs a pass.
    Pass(Box<CoordinatorPassInput>),
    /// `coordinator.wake`: a wake-up that bypasses the gap and the caps.
    Wake,
    WakeOutcome {
        seq: u64,
        marker: Turn,
        outcome: WakeOutcome,
    },
    /// First native start: report the POC's coordinator entry's session for
    /// `--resume` (read only: v2 never writes `managed.json`).
    Migrate,
    SetWakeCfg(WakeCfg),
    SetDashboardPort(u16),
    /// Stop the loop (sent when the handle drops; a wake reporter may still
    /// hold a sender, so the channel alone would not close).
    Shutdown,
}

/// Why the coordinator cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockedReason {
    /// Another herdr server, or a legacy `herdr plus run`, holds the lock.
    LockedElsewhere,
    /// `managed.json` does not parse (agents v1). Never produced since v2
    /// stopped depending on the registry; the variant stays because the
    /// App's reason codes name it.
    #[allow(dead_code)] // Kept for the published `registry_corrupt` reason code.
    RegistryCorrupt(String),
    /// The directory cannot be created or prepared.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStatus {
    WaitingForLock,
    /// The lock is held and the directory is seeded.
    Ready,
    Blocked(BlockedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DashboardState {
    Off,
    Listening(u16),
    PortInUse(u16),
}

impl DashboardState {
    /// The dashboard's URL while it listens.
    pub fn url(self) -> Option<String> {
        match self {
            Self::Listening(port) => Some(super::serve::dashboard_url(port)),
            Self::Off | Self::PortInUse(_) => None,
        }
    }
}

/// The POC directory to migrate into `dir`: only for the default
/// coordinator directory, so a directory set elsewhere (tests, overrides)
/// never moves the user's.
pub fn legacy_dir_for(dir: &Path) -> Option<PathBuf> {
    (dir == super::coordinator_dir())
        .then(super::legacy_dir)
        .flatten()
}

/// The coordinator Claude's argv after the executable (it writes the MCP
/// config it points at) and its session id: `--resume <id>` when resuming,
/// a fresh `--session-id` otherwise, `--model` before the `--` separator.
pub fn coordinator_launch_args(
    ctx: &LaunchCtx,
    resume: Option<&str>,
    model: Option<&str>,
) -> io::Result<(String, Vec<String>)> {
    let session = match resume {
        Some(id) => launch::ClaudeSession::Resume(id.to_string()),
        None => launch::ClaudeSession::New(launch::new_uuid()),
    };
    let kickoff = launch::coordinator_kickoff(&ctx.dir);
    let args = launch::claude_args(ctx, &session, true, Some(&kickoff))?;
    let id = match session {
        launch::ClaudeSession::New(id) | launch::ClaudeSession::Resume(id) => id,
    };
    Ok((id, with_model(args, model)))
}

/// Insert `--model <model>` before the argv's `--` separator.
pub fn with_model(mut args: Vec<String>, model: Option<&str>) -> Vec<String> {
    if let Some(model) = model {
        let at = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args.splice(at..at, ["--model".to_string(), model.to_string()]);
    }
    args
}

/// What the App owes after a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Re-check the coordinator pane and queue `text` into it, then answer
    /// with [`WorkerMsg::WakeOutcome`]. The digest and the turn marker are
    /// already written.
    Prompt {
        seq: u64,
        pane: String,
        text: String,
        marker: Turn,
        items: usize,
    },
    /// The registered coordinator has been missing for `missing_s`: a
    /// trigger; the App relaunches it and counts it against its cap.
    Relaunch { resume: Option<String> },
}

/// One board suggestion, hashed for the "new suggestions" notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub hash: u64,
    pub text: String,
}

/// What the read model needs of `board.json` (its content is for the
/// dashboard page).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardSummary {
    /// The file's mtime, unix seconds (the coordinator has no clock).
    pub generated_unix: u64,
    pub summary: Option<String>,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// The POC coordinator's session, for `--resume`.
    pub legacy_session: Option<String>,
    pub legacy_pane: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorPassOutput {
    pub status: EngineStatus,
    pub dashboard: DashboardState,
    pub effects: Vec<Effect>,
    /// The wake bookkeeping (`None` before the first pass with agent facts).
    pub summary: Option<live::WatchSummary>,
    pub turn: Option<Turn>,
    pub board: Option<BoardSummary>,
    /// Suggestions not seen before, once each.
    pub new_suggestions: Vec<Suggestion>,
    /// The agents in the wake scope (the coordinator included): what
    /// `coordinator.get.managed` reports.
    pub managed: Vec<LiveAgent>,
    /// The live coordinator's session.
    pub coordinator_session: Option<String>,
    pub coordinator_pane: Option<String>,
    /// The answer to [`WorkerMsg::Migrate`].
    pub migration: Option<MigrationOutcome>,
    /// The POC directory was renamed into place on this start.
    pub dir_migrated: bool,
    /// The worker that produced it: the App's sink stamps it, and the App
    /// ignores outputs of a worker it already dropped.
    pub worker: u64,
}

impl CoordinatorPassOutput {
    fn new(status: EngineStatus, dashboard: DashboardState) -> Self {
        Self {
            status,
            dashboard,
            effects: Vec::new(),
            summary: None,
            turn: None,
            board: None,
            new_suggestions: Vec::new(),
            managed: Vec::new(),
            coordinator_session: None,
            coordinator_pane: None,
            migration: None,
            dir_migrated: false,
            worker: 0,
        }
    }

    /// Carries something the App must act on (not just a changed read model).
    fn has_events(&self) -> bool {
        !self.effects.is_empty()
            || !self.new_suggestions.is_empty()
            || self.migration.is_some()
            || self.dir_migrated
    }

    /// The read model without the one-shot events, for change detection.
    fn signature(&self) -> Self {
        Self {
            effects: Vec::new(),
            new_suggestions: Vec::new(),
            migration: None,
            dir_migrated: false,
            ..self.clone()
        }
    }
}

/// The result of moving the POC directory into place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirMigration {
    /// The coordinator directory already exists: used as is, never merged.
    UseExisting,
    /// The POC directory was renamed to the coordinator directory.
    Renamed,
    /// Neither exists yet.
    Fresh,
    /// The POC directory's watcher lock is held (a legacy `herdr plus run`).
    LegacyLocked,
}

/// Move the POC directory into place before the lock is taken in the new one.
/// Only a sibling of `dir` is ever moved (a same-parent rename).
pub fn migrate_dir(legacy: Option<&Path>, dir: &Path) -> io::Result<DirMigration> {
    if dir.exists() {
        return Ok(DirMigration::UseExisting);
    }
    let sibling = |legacy: &&Path| legacy.is_dir() && legacy.parent() == dir.parent();
    let Some(legacy) = legacy.filter(sibling) else {
        return Ok(DirMigration::Fresh);
    };
    match lock::try_exclusive(legacy, WATCH_LOCK)? {
        None => Ok(DirMigration::LegacyLocked),
        Some(held) => {
            drop(held);
            std::fs::rename(legacy, dir)?;
            Ok(DirMigration::Renamed)
        }
    }
}

/// FNV-1a over the suggestion text (stable across runs and builds).
pub fn suggestion_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Parse `board.json` leniently: every field is optional, strings are cut to
/// one line, and a suggestion is either `{ "text": ... }` or a bare string.
/// `None` when the file is not a JSON object (a half-written file).
pub fn parse_board(bytes: &[u8], generated_unix: u64) -> Option<BoardSummary> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .map(|text| one_line(text, BOARD_SUMMARY_CHARS))
        .filter(|text| !text.is_empty());
    let suggestions = object
        .get("suggestions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().or_else(|| item.get("text")?.as_str()))
                .map(|text| one_line(text, SUGGESTION_CHARS))
                .filter(|text| !text.is_empty())
                .take(MAX_SUGGESTIONS)
                .map(|text| Suggestion {
                    hash: suggestion_hash(&text),
                    text,
                })
                .collect()
        })
        .unwrap_or_default();
    Some(BoardSummary {
        generated_unix,
        summary,
        suggestions,
    })
}

/// The POC's coordinator entry (the registry's `coordinator` role): its
/// session for `--resume` and its pane. Read only: v2 never writes
/// `managed.json`, and the coordinator is the server's record, so the old
/// entry needs no retiring.
pub fn retire_legacy_coordinator(dir: &Path) -> MigrationOutcome {
    match super::registry::load_strict(dir) {
        Ok(registry) => {
            let entry = registry.coordinator();
            MigrationOutcome {
                legacy_session: entry.and_then(|entry| entry.session.clone()),
                legacy_pane: entry.and_then(|entry| entry.pane_id.clone()),
                error: None,
            }
        }
        Err(err) => MigrationOutcome {
            legacy_session: None,
            legacy_pane: None,
            error: Some(err.to_string()),
        },
    }
}

/// The worker's state; driven by [`Engine::handle`] and [`Engine::tick`]
/// (directly in tests, from the thread loop otherwise).
pub struct Engine {
    cfg: EngineConfig,
    lock: Option<DirLock>,
    status: EngineStatus,
    next_lock_try: u64,
    dir_migrated: bool,
    dashboard: Option<DashboardServer>,
    dashboard_state: DashboardState,
    next_bind_try: u64,
    port_error_logged: bool,
    state: WatchState,
    input: Option<CoordinatorPassInput>,
    wake_requested: bool,
    recent: VecDeque<AgentMessage>,
    /// The newest action-log lines, newest last (`live.json.actions`).
    actions: VecDeque<AgentActionEntry>,
    /// `actions.jsonl`'s mtime at the last read (`None`: not read yet).
    actions_mtime_ms: Option<u64>,
    last_live: Option<LiveData>,
    last_live_write: u64,
    last_state: Vec<u8>,
    last_rotate: u64,
    board_mtime_ms: Option<u64>,
    board: Option<BoardSummary>,
    seen: HashSet<u64>,
    /// The last output sent (its signature), to skip unchanged passes.
    last_sent: Option<CoordinatorPassOutput>,
    /// One-shot answers owed to the App with the next output.
    owed: CoordinatorPassOutput,
}

impl Engine {
    pub fn new(cfg: EngineConfig) -> Self {
        let seen = cfg.seen_suggestions.clone();
        Self {
            cfg,
            lock: None,
            status: EngineStatus::WaitingForLock,
            next_lock_try: 0,
            dir_migrated: false,
            dashboard: None,
            dashboard_state: DashboardState::Off,
            next_bind_try: 0,
            port_error_logged: false,
            state: WatchState::default(),
            input: None,
            wake_requested: false,
            recent: VecDeque::new(),
            actions: VecDeque::new(),
            actions_mtime_ms: None,
            last_live: None,
            last_live_write: 0,
            last_state: Vec::new(),
            last_rotate: 0,
            board_mtime_ms: None,
            board: None,
            seen,
            last_sent: None,
            owed: CoordinatorPassOutput::new(EngineStatus::WaitingForLock, DashboardState::Off),
        }
    }

    #[cfg(test)]
    fn status(&self) -> &EngineStatus {
        &self.status
    }

    fn dir(&self) -> &Path {
        &self.cfg.dir
    }

    fn log(&self, now: u64, line: &str) {
        watch::log_line(&self.cfg.dir, now, line);
    }

    /// Take the lock when it is due; `true` once held.
    fn acquire(&mut self, now: u64) -> bool {
        if self.lock.is_some() {
            return true;
        }
        if now < self.next_lock_try {
            return false;
        }
        self.next_lock_try = now + LOCK_RETRY_S;
        let dir = self.cfg.dir.clone();
        match migrate_dir(self.cfg.legacy_dir.as_deref(), &dir) {
            Ok(DirMigration::LegacyLocked) => {
                tracing::info!(
                    dir = %dir.display(),
                    "coordinator: the POC directory is locked by a running watcher; retrying"
                );
                self.status = EngineStatus::Blocked(BlockedReason::LockedElsewhere);
                return false;
            }
            Ok(DirMigration::Renamed) => {
                self.dir_migrated = true;
                tracing::info!(dir = %dir.display(), "coordinator: moved the POC directory into place");
            }
            Ok(DirMigration::UseExisting | DirMigration::Fresh) => {}
            Err(err) => {
                tracing::warn!(error = %err, "coordinator: directory migration failed");
                self.status = EngineStatus::Blocked(BlockedReason::Unavailable(format!(
                    "cannot prepare {}: {err}",
                    dir.display()
                )));
                return false;
            }
        }
        let held = match lock::try_exclusive(&dir, WATCH_LOCK) {
            Ok(Some(held)) => held,
            Ok(None) => {
                self.status = EngineStatus::Blocked(BlockedReason::LockedElsewhere);
                return false;
            }
            Err(err) => {
                self.status = EngineStatus::Blocked(BlockedReason::Unavailable(format!(
                    "cannot lock {}: {err}",
                    dir.display()
                )));
                return false;
            }
        };
        if let Err(err) = super::seed(&dir) {
            self.status = EngineStatus::Blocked(BlockedReason::Unavailable(format!(
                "cannot seed {}: {err}",
                dir.display()
            )));
            return false;
        }
        self.lock = Some(held);
        if let Some(ctx) = &self.cfg.launch {
            if let Err(err) = launch::write_claude_mcp_config(ctx) {
                tracing::warn!(error = %err, "coordinator: cannot write mcp/claude.json");
            }
        }
        // `herdr coordinator wake` used to leave this file for the watcher;
        // `coordinator.wake` replaced it.
        let request = wake_request_path(&dir);
        if request.exists() {
            if let Err(err) = std::fs::remove_file(&request) {
                tracing::warn!(error = %err, "coordinator: cannot remove the stale wake.request");
            }
        }
        let mut state = WatchState::load(&dir);
        state.rebaseline();
        state.coordinator_down = false;
        // The log's tail seeds `recent`; skip what was appended while no
        // watcher ran (the baseline would ignore those lines anyway).
        let (_, offset) = messages::since_offset(&dir, state.msg_offset);
        state.msg_offset = offset;
        self.last_state = serde_json::to_vec_pretty(&state).unwrap_or_default();
        self.state = state;
        self.recent = messages::recent(&dir, watch::LIVE_MESSAGES, None).into();
        let cfg = &self.cfg.wake;
        self.log(
            now,
            &format!(
                "watcher started in herdr (debounce {}s, gap {}s, periodic {}s, caps {}/h {}/day)",
                cfg.debounce_s, cfg.gap_s, cfg.periodic_s, cfg.cap_hour, cfg.cap_day
            ),
        );
        self.status = EngineStatus::Ready;
        true
    }

    /// Start or retry the dashboard while the lock is held.
    fn ensure_dashboard(&mut self, now: u64) {
        if self.lock.is_none() || self.cfg.dashboard_port == 0 {
            self.dashboard = None;
            self.dashboard_state = DashboardState::Off;
            return;
        }
        if self.dashboard.is_some() || now < self.next_bind_try {
            return;
        }
        let port = self.cfg.dashboard_port;
        match DashboardServer::start(self.cfg.dir.clone(), port) {
            Ok(server) => {
                self.dashboard_state = DashboardState::Listening(server.port());
                self.dashboard = Some(server);
                self.port_error_logged = false;
            }
            Err(err) => {
                self.next_bind_try = now + BIND_RETRY_S;
                self.dashboard_state = DashboardState::PortInUse(port);
                if !self.port_error_logged {
                    self.port_error_logged = true;
                    tracing::warn!(port, error = %err, "coordinator: dashboard could not bind");
                    self.log(now, &format!("dashboard on port {port} failed: {err}"));
                }
            }
        }
    }

    fn set_dashboard_port(&mut self, port: u16, now: u64) {
        if port == self.cfg.dashboard_port {
            return;
        }
        self.cfg.dashboard_port = port;
        // Stop (joins the accept loop, releasing the port), then rebind.
        self.dashboard = None;
        self.dashboard_state = DashboardState::Off;
        self.next_bind_try = 0;
        self.port_error_logged = false;
        self.ensure_dashboard(now);
    }

    /// Apply one message; returns whether a pass should run now.
    pub fn handle(&mut self, msg: WorkerMsg, now: u64) -> bool {
        match msg {
            WorkerMsg::Pass(input) => {
                self.input = Some(*input);
                true
            }
            WorkerMsg::Wake => {
                self.wake_requested = true;
                if self.lock.is_some() {
                    self.log(now, "wake requested");
                }
                true
            }
            WorkerMsg::WakeOutcome {
                seq,
                marker,
                outcome,
            } => {
                self.wake_outcome(seq, &marker, outcome, now);
                true
            }
            WorkerMsg::Migrate => {
                let outcome = if self.lock.is_some() {
                    retire_legacy_coordinator(self.dir())
                } else {
                    MigrationOutcome {
                        legacy_session: None,
                        legacy_pane: None,
                        error: Some(
                            "the coordinator directory is not locked by this server".into(),
                        ),
                    }
                };
                if let Some(session) = &outcome.legacy_session {
                    self.log(
                        now,
                        &format!("found the POC coordinator's session ({session})"),
                    );
                }
                self.owed.migration = Some(outcome);
                true
            }
            WorkerMsg::SetWakeCfg(cfg) => {
                self.cfg.wake = cfg;
                false
            }
            WorkerMsg::SetDashboardPort(port) => {
                self.set_dashboard_port(port, now);
                true
            }
            WorkerMsg::Shutdown => false,
        }
    }

    fn wake_outcome(&mut self, seq: u64, marker: &Turn, outcome: WakeOutcome, now: u64) {
        if self.lock.is_none() {
            return;
        }
        match outcome {
            WakeOutcome::Delivered => {
                self.state.wake_delivered(seq, now);
                self.log(
                    now,
                    &format!("delivered #{seq} -> {}", marker.coordinator_pane),
                );
            }
            WakeOutcome::Held(status) => {
                // Not typed in: the marker must not guard a turn that never
                // started (the user may type into the coordinator next).
                turn::clear_if(self.dir(), marker);
                self.state.wake_held(now, &self.cfg.wake);
                self.log(now, &format!("held coordinator {status} (re-check)"));
            }
            WakeOutcome::Failed(code) => {
                turn::clear_if(self.dir(), marker);
                self.state.wake_failed(now, &self.cfg.wake);
                self.log(now, &format!("failed #{seq} {code}"));
            }
        }
        self.persist_state();
    }

    /// One pass: lock and dashboard upkeep, then (with the lock and agent
    /// facts) the watcher tick. Returns the output when the App should see
    /// it.
    pub fn tick(&mut self, now: u64) -> Option<CoordinatorPassOutput> {
        let had_lock = self.lock.is_some();
        self.acquire(now);
        self.ensure_dashboard(now);
        let mut out = self.owed_output();
        if self.lock.is_some() {
            if !had_lock && self.dir_migrated {
                out.dir_migrated = true;
            }
            out.status = self.status.clone();
            self.pass(now, &mut out);
        }
        self.decide(out)
    }

    fn owed_output(&mut self) -> CoordinatorPassOutput {
        let mut owed = std::mem::replace(
            &mut self.owed,
            CoordinatorPassOutput::new(EngineStatus::WaitingForLock, DashboardState::Off),
        );
        owed.status = self.status.clone();
        owed.dashboard = self.dashboard_state;
        owed
    }

    /// An output the App never received: forget it was sent (the next pass
    /// sends the read model again) and keep its one-shot answers. A dropped
    /// prompt heals on its own (its marker expires unworked, the wake stays
    /// pending); a dropped relaunch is raised again after `missing_s`.
    fn undelivered(&mut self, out: CoordinatorPassOutput) {
        self.last_sent = None;
        if self.owed.migration.is_none() {
            self.owed.migration = out.migration;
        }
        self.owed.dir_migrated |= out.dir_migrated;
        let mut suggestions = out.new_suggestions;
        suggestions.append(&mut self.owed.new_suggestions);
        self.owed.new_suggestions = suggestions;
    }

    /// Send only what changed, or what carries a one-shot event.
    fn decide(&mut self, out: CoordinatorPassOutput) -> Option<CoordinatorPassOutput> {
        let signature = out.signature();
        if !out.has_events() && self.last_sent.as_ref() == Some(&signature) {
            return None;
        }
        self.last_sent = Some(signature);
        Some(out)
    }

    fn pass(&mut self, now: u64, out: &mut CoordinatorPassOutput) {
        self.watch_board(out);
        let Some(input) = self.input.clone() else {
            // No agent facts yet: a tick now would baseline an empty list
            // and report every agent as new once the facts arrive.
            return;
        };
        let dir = self.cfg.dir.clone();
        let last_change: HashMap<String, u64> = self
            .state
            .last_change
            .iter()
            .map(|(pane, at)| (pane.clone(), *at))
            .collect();
        let inputs = FactInputs {
            agents: &input.agents,
            groups: &input.groups,
            tabs: &input.tabs,
            tab_labels: &input.tab_labels,
        };
        let mut live =
            live::build_facts(&inputs, self.cfg.wake.scope, Vec::new(), &last_change, now);
        self.state.expected_coordinator = input.expected_coordinator.clone();
        let new_actions = self.read_actions(&dir);

        let (new_msgs, offset) = messages::since_offset(&dir, self.state.msg_offset);
        self.state.msg_offset = offset;
        if now.saturating_sub(self.last_rotate) >= watch::ROTATE_EVERY_S {
            self.last_rotate = now;
            match messages::rotate_if_large(&dir, watch::ROTATE_BYTES, offset) {
                Ok(true) => {
                    self.state.msg_offset = 0;
                    self.log(now, "rotated messages.jsonl");
                }
                Ok(false) => {}
                Err(err) => tracing::warn!("coordinator: cannot rotate the message log: {err}"),
            }
        }
        for message in &new_msgs {
            messages::fold_into(&mut self.recent, message.clone());
        }
        while self.recent.len() > watch::LIVE_MESSAGES {
            self.recent.pop_front();
        }

        let turn = turn::read_live(&dir, now);
        let wake_requested = std::mem::take(&mut self.wake_requested);
        self.state.coordinator_down = input.coordinator_down;
        let actions = watch::tick(
            &mut self.state,
            &live,
            &new_msgs,
            &new_actions,
            turn.as_ref(),
            wake_requested,
            &self.cfg.wake,
            now,
        );
        let mut turn_now = turn.clone();
        for action in actions {
            match action {
                Action::Log(line) => self.log(now, &line),
                // Both only touch the turn read above: an MCP server may
                // have started a new one since.
                Action::MarkTurnWorking => {
                    if let Some(turn) = &turn {
                        if let Err(err) = turn::mark_working_if(&dir, turn) {
                            tracing::warn!("coordinator: cannot update the turn marker: {err}");
                        }
                    }
                }
                Action::ClearTurn => {
                    if let Some(turn) = &turn {
                        turn::clear_if(&dir, turn);
                    }
                    turn_now = turn::read_live(&dir, now);
                }
                Action::Relaunch { resume } => {
                    self.log(
                        now,
                        &format!(
                            "coordinator missing; relaunch requested (resume={})",
                            resume.as_deref().unwrap_or("none")
                        ),
                    );
                    out.effects.push(Effect::Relaunch { resume });
                }
                Action::Wake {
                    seq,
                    digest,
                    prompt,
                    pane,
                    items,
                } => {
                    if let Some(effect) = self.prepare_wake(seq, &digest, prompt, pane, items, now)
                    {
                        if let Effect::Prompt { marker, .. } = &effect {
                            turn_now = Some(marker.clone());
                        }
                        out.effects.push(effect);
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
        live.actions = self.actions.iter().cloned().collect();
        live.watch = self.state.summary(&self.cfg.wake, turn_now.is_some(), now);
        out.summary = Some(live.watch.clone());
        out.turn = turn_now;
        out.managed = live
            .agents
            .iter()
            .filter(|agent| agent.managed)
            .cloned()
            .collect();
        out.coordinator_pane = live.coordinator_pane.clone();
        out.coordinator_session = live
            .agents
            .iter()
            .find(|agent| agent.coordinator)
            .and_then(|agent| agent.session.clone());
        self.publish(live, now);
        self.persist_state();
    }

    /// The action-log lines appended since the last read (oldest first),
    /// read only when `actions.jsonl` changed. The first read after the
    /// lock is a baseline: it fills the dashboard's list and queues nothing.
    fn read_actions(&mut self, dir: &Path) -> Vec<AgentActionEntry> {
        let mtime = watch::mtime_ms(&actions_log::actions_path(dir));
        if self.actions_mtime_ms == Some(mtime) {
            return Vec::new();
        }
        let first = self.actions_mtime_ms.is_none();
        self.actions_mtime_ms = Some(mtime);
        // Newest first.
        let tail = actions_log::read_tail(dir, LIVE_ACTIONS);
        let last = self.actions.back().map(|entry| entry.id.clone());
        let fresh: Vec<AgentActionEntry> = match &last {
            Some(last) => tail
                .into_iter()
                .take_while(|entry| &entry.id != last)
                .collect(),
            None => tail,
        };
        let fresh: Vec<AgentActionEntry> = fresh.into_iter().rev().collect();
        self.actions.extend(fresh.iter().cloned());
        while self.actions.len() > LIVE_ACTIONS {
            self.actions.pop_front();
        }
        if first {
            Vec::new()
        } else {
            fresh
        }
    }

    /// Write the digest and take the turn marker; the App types the prompt.
    fn prepare_wake(
        &mut self,
        seq: u64,
        digest: &str,
        text: String,
        pane: String,
        items: usize,
        now: u64,
    ) -> Option<Effect> {
        let dir = self.cfg.dir.clone();
        let path = wake_dir(&dir).join(format!("{seq}.md"));
        if let Err(err) = write_atomically(&path, digest.as_bytes()) {
            self.state.wake_failed(now, &self.cfg.wake);
            self.log(now, &format!("failed #{seq} digest not written: {err}"));
            return None;
        }
        watch::prune_digests(&dir);
        let marker = Turn {
            source: "wake".into(),
            id: seq.to_string(),
            started_unix: now,
            coordinator_pane: pane.clone(),
            seen_working: false,
        };
        // Without the marker the coordinator's write tools are not guarded
        // during this turn: no marker, no wake-up. An agent message may have
        // taken the turn since the tick read it: hold, it is not a failure.
        match turn::write_if_absent(&dir, &marker, now) {
            Ok(true) => Some(Effect::Prompt {
                seq,
                pane,
                text,
                marker,
                items,
            }),
            Ok(false) => {
                self.log(now, "held turn live (re-check)");
                None
            }
            Err(err) => {
                self.state.wake_failed(now, &self.cfg.wake);
                self.log(now, &format!("failed #{seq} turn marker: {err}"));
                None
            }
        }
    }

    /// Re-read `board.json` when its mtime moved; a file that does not parse
    /// (half written) keeps the previous board.
    fn watch_board(&mut self, out: &mut CoordinatorPassOutput) {
        let path = board_path(&self.cfg.dir);
        let mtime = watch::mtime_ms(&path);
        if self.board_mtime_ms != Some(mtime) {
            let parsed = std::fs::read(&path)
                .ok()
                .and_then(|bytes| parse_board(&bytes, mtime / 1000));
            if let Some(board) = parsed {
                self.board_mtime_ms = Some(mtime);
                for suggestion in &board.suggestions {
                    if self.seen.insert(suggestion.hash) {
                        out.new_suggestions.push(suggestion.clone());
                    }
                }
                self.board = Some(board);
            }
        }
        out.board = self.board.clone();
    }

    /// Write `live.json` when it changed (ignoring the timestamp) or every
    /// 10 s (the heartbeat `turn::watcher_alive` and the page read).
    fn publish(&mut self, live: LiveData, now: u64) {
        let comparable = LiveData {
            generated_unix: 0,
            ..live.clone()
        };
        if self.last_live.as_ref() == Some(&comparable)
            && now.saturating_sub(self.last_live_write) < watch::LIVE_REFRESH_S
        {
            return;
        }
        let written = serde_json::to_vec_pretty(&live)
            .map_err(io::Error::other)
            .and_then(|json| write_atomically(&super::live_path(&self.cfg.dir), &json));
        match written {
            Ok(()) => {
                self.last_live = Some(comparable);
                self.last_live_write = now;
            }
            Err(err) => tracing::warn!("coordinator: cannot write live.json: {err}"),
        }
    }

    /// Save `watch_state.json` when it changed.
    fn persist_state(&mut self) {
        if self.lock.is_none() {
            return;
        }
        let Ok(json) = serde_json::to_vec_pretty(&self.state) else {
            return;
        };
        if json == self.last_state {
            return;
        }
        match self.state.save() {
            Ok(()) => self.last_state = json,
            Err(err) => tracing::warn!("coordinator: cannot write watch_state.json: {err}"),
        }
    }

    /// When the thread loop should wake next without a message.
    fn next_wait(&self, now: u64) -> Duration {
        if self.lock.is_none() {
            let left = self.next_lock_try.saturating_sub(now).max(1);
            return Duration::from_secs(left).min(Duration::from_secs(LOCK_RETRY_S));
        }
        TIMER
    }
}

/// The handle the App keeps; dropping it stops the worker, its dashboard
/// and releases the lock.
pub struct WorkerHandle {
    tx: Option<mpsc::Sender<WorkerMsg>>,
    handle: Option<JoinHandle<()>>,
    /// Set on drop: an output retry gives up at once, so the join (on the
    /// server's main loop, which is the one that would drain the channel)
    /// does not wait out the retries.
    stop: Arc<AtomicBool>,
}

/// Where the worker delivers its outputs (the App wraps the event sender).
/// It must not block: a full channel hands the output back (`Err`) so the
/// worker can retry briefly and otherwise resend later. Blocking here could
/// deadlock with the main loop joining the worker.
pub type OutputSink =
    Box<dyn Fn(Box<CoordinatorPassOutput>) -> Result<(), Box<CoordinatorPassOutput>> + Send>;

/// Delivery attempts for one output before it is given up (and its one-shot
/// answers kept for the next pass).
const SINK_ATTEMPTS: usize = 20;
const SINK_PAUSE: Duration = Duration::from_millis(50);

impl WorkerHandle {
    pub fn spawn(cfg: EngineConfig, sink: OutputSink) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("herdr-coordinator".into())
            .spawn(move || run(Engine::new(cfg), &rx, &sink, &stopping))?;
        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
            stop,
        })
    }

    /// `false` when the worker is gone.
    pub fn send(&self, msg: WorkerMsg) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(msg).is_ok())
    }

    /// A sender for a thread that answers later (a wake outcome). It does
    /// not keep the worker alive past the handle: dropping the handle sends
    /// [`WorkerMsg::Shutdown`].
    pub fn sender(&self) -> mpsc::Sender<WorkerMsg> {
        match &self.tx {
            Some(tx) => tx.clone(),
            None => mpsc::channel().0,
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // Shutdown ends the loop at its next receive (closing the channel
        // would too, unless a wake reporter still holds a sender).
        self.stop.store(true, Ordering::Relaxed);
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(WorkerMsg::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::warn!("coordinator: worker thread panicked");
            }
        }
    }
}

/// Hand `out` to the sink, retrying a full channel a few times; `false` when
/// it was given up, in which case the next pass sends the read model again
/// and the one-shot answers ride along with it. `stop` (the handle is being
/// dropped) gives up at once.
fn deliver(
    engine: &mut Engine,
    sink: &OutputSink,
    out: CoordinatorPassOutput,
    attempts: usize,
    pause: Duration,
    stop: &AtomicBool,
) -> bool {
    let mut out = Box::new(out);
    for attempt in 0..attempts.max(1) {
        match sink(out) {
            Ok(()) => return true,
            Err(back) => out = back,
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if attempt + 1 < attempts {
            std::thread::sleep(pause);
        }
    }
    tracing::warn!("coordinator: the app is not taking worker outputs; resending later");
    engine.undelivered(*out);
    false
}

fn run(mut engine: Engine, rx: &mpsc::Receiver<WorkerMsg>, sink: &OutputSink, stop: &AtomicBool) {
    let mut deadline = Instant::now();
    loop {
        let wait = deadline.saturating_duration_since(Instant::now());
        let mut due = match rx.recv_timeout(wait) {
            Ok(WorkerMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(msg) => engine.handle(msg, now_unix()),
            Err(RecvTimeoutError::Timeout) => true,
        };
        // Coalesce whatever else is queued into one pass.
        loop {
            match rx.try_recv() {
                Ok(WorkerMsg::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    tracing::info!("coordinator: worker stopped");
                    return;
                }
                Ok(msg) => due |= engine.handle(msg, now_unix()),
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if Instant::now() >= deadline {
            due = true;
        }
        if !due {
            continue;
        }
        let now = now_unix();
        if let Some(out) = engine.tick(now) {
            deliver(&mut engine, sink, out, SINK_ATTEMPTS, SINK_PAUSE, stop);
        }
        deadline = Instant::now() + engine.next_wait(now);
    }
    tracing::info!("coordinator: worker stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_model::OpenedBy;
    use crate::coordinator::{live_path, registry_path, test_dir, turn_path};

    fn config(dir: &Path) -> EngineConfig {
        EngineConfig {
            dir: dir.to_path_buf(),
            legacy_dir: None,
            wake: WakeCfg::default(),
            dashboard_port: 0,
            launch: None,
            seen_suggestions: HashSet::new(),
        }
    }

    fn fact(pane: &str, name: &str, session: &str, status: &str) -> CoordinatorAgentFact {
        CoordinatorAgentFact {
            pane_id: pane.into(),
            tab_id: format!("{}:t1", &pane[..2]),
            workspace_id: pane[..2].into(),
            name: Some(name.into()),
            agent: Some("claude".into()),
            status: status.into(),
            session: Some(session.into()),
            ..CoordinatorAgentFact::default()
        }
    }

    /// The coordinator herdr runs (w1:p1, session cs).
    fn expected() -> Option<ExpectedCoordinator> {
        Some(ExpectedCoordinator {
            pane_id: Some("w1:p1".into()),
            session: Some("cs".into()),
        })
    }

    fn input(agents: Vec<CoordinatorAgentFact>) -> WorkerMsg {
        WorkerMsg::Pass(Box::new(CoordinatorPassInput {
            agents,
            expected_coordinator: expected(),
            ..CoordinatorPassInput::default()
        }))
    }

    /// A ready engine; [`agents`] gives the coordinator (w1:p1, session cs)
    /// and one agent it opened (w2:p1, session a).
    fn ready(name: &str) -> (PathBuf, Engine) {
        let root = test_dir(name);
        let dir = root.join("coordinator");
        let mut engine = Engine::new(config(&dir));
        engine.tick(0);
        assert_eq!(engine.status(), &EngineStatus::Ready);
        (root, engine)
    }

    fn agents(status: &str) -> Vec<CoordinatorAgentFact> {
        let mut coordinator = fact("w1:p1", "coordinator", "cs", "idle");
        coordinator.coordinator = true;
        let mut lead = fact("w2:p1", "lead", "a", status);
        lead.opened_by = Some(OpenedBy::Coordinator);
        vec![coordinator, lead]
    }

    fn prompts(out: &CoordinatorPassOutput) -> Vec<&Effect> {
        out.effects
            .iter()
            .filter(|effect| matches!(effect, Effect::Prompt { .. }))
            .collect()
    }

    /// Baseline, settle and finish one agent: the debounce passes at 70.
    fn drive_to_a_wake(engine: &mut Engine) -> CoordinatorPassOutput {
        engine.handle(input(agents("working")), 0);
        engine.tick(1);
        engine.handle(input(agents("idle")), 10);
        engine.tick(10);
        engine.tick(70).expect("a wake is owed")
    }

    #[test]
    fn a_finished_agent_produces_a_prompt_effect_and_a_marker() {
        let (root, mut engine) = ready("engine-prompt");
        let dir = root.join("coordinator");
        let out = drive_to_a_wake(&mut engine);
        let prompts = prompts(&out);
        assert_eq!(prompts.len(), 1, "{out:?}");
        let Effect::Prompt {
            seq, pane, marker, ..
        } = prompts[0]
        else {
            unreachable!()
        };
        assert_eq!((*seq, pane.as_str()), (1, "w1:p1"));
        assert!(
            wake_dir(&dir).join("1.md").exists(),
            "the digest is written"
        );
        let on_disk = turn::read_live(&dir, 70).expect("marker on disk");
        assert_eq!(&on_disk, marker);
        assert!(out.turn.is_some(), "the read model shows the turn");

        engine.handle(
            WorkerMsg::WakeOutcome {
                seq: 1,
                marker: marker.clone(),
                outcome: WakeOutcome::Delivered,
            },
            71,
        );
        assert_eq!(engine.state.wake_seq, 1);
        assert!(engine.state.pending.is_empty());
        assert!(
            turn_path(&dir).exists(),
            "a delivered turn keeps its marker"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_wake_clears_the_marker_and_held_keeps_the_counters() {
        let (root, mut engine) = ready("engine-failed");
        let dir = root.join("coordinator");
        let out = drive_to_a_wake(&mut engine);
        let Some(Effect::Prompt { seq, marker, .. }) = prompts(&out).first().copied().cloned()
        else {
            panic!("no prompt: {out:?}")
        };
        let pending = engine.state.pending.len();
        engine.handle(
            WorkerMsg::WakeOutcome {
                seq,
                marker: marker.clone(),
                outcome: WakeOutcome::Held("working".into()),
            },
            71,
        );
        assert!(!turn_path(&dir).exists(), "a held wake releases its marker");
        assert_eq!(engine.state.wake_seq, 0, "held keeps the counters");
        assert_eq!(engine.state.pending.len(), pending);
        let held_until = 71 + watch::HELD_RETRY_S.max(WakeCfg::default().settle_s);
        assert_eq!(engine.state.retry_after, held_until, "held backs off");
        // The facts still say idle: the next pass must not prepare the same
        // wake-up again at once (the App would hold it again, in a loop).
        let again = engine.tick(71);
        assert!(
            again.as_ref().is_none_or(|out| prompts(out).is_empty()),
            "{again:?}"
        );
        let later = engine.tick(held_until).expect("a wake is owed again");
        assert_eq!(prompts(&later).len(), 1, "{later:?}");
        let Some(Effect::Prompt { marker, .. }) = prompts(&later).first().copied().cloned() else {
            unreachable!()
        };

        turn::write(&dir, &marker).unwrap();
        engine.handle(
            WorkerMsg::WakeOutcome {
                seq,
                marker,
                outcome: WakeOutcome::Failed("agent_prompt_failed".into()),
            },
            72,
        );
        assert!(!turn_path(&dir).exists(), "a failed wake clears the marker");
        assert_eq!(engine.state.wake_seq, 0);
        assert_eq!(engine.state.pending.len(), pending, "still pending");
        assert_eq!(engine.state.retry_after, 72 + WakeCfg::default().gap_s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_worker_on_the_same_dir_is_locked_out_and_never_serves() {
        let (root, mut first) = ready("engine-locked");
        let dir = root.join("coordinator");
        let mut cfg = config(&dir);
        cfg.dashboard_port = free_port();
        let mut second = Engine::new(cfg);
        let out = second.tick(0).expect("status");
        assert_eq!(
            out.status,
            EngineStatus::Blocked(BlockedReason::LockedElsewhere)
        );
        assert_eq!(out.dashboard, DashboardState::Off);
        assert!(second.dashboard.is_none());
        assert!(second.tick(10).is_none(), "nothing new before the retry");
        // The first one goes away: the retry takes over.
        first.lock = None;
        drop(first);
        let out = second.tick(LOCK_RETRY_S).expect("ready");
        assert_eq!(out.status, EngineStatus::Ready);
        assert!(matches!(out.dashboard, DashboardState::Listening(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    #[test]
    fn the_dashboard_binds_after_ready_and_a_port_in_use_is_reported() {
        let root = test_dir("engine-port");
        let dir = root.join("coordinator");
        let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        let mut cfg = config(&dir);
        cfg.dashboard_port = port;
        let mut engine = Engine::new(cfg);
        let out = engine.tick(0).expect("first");
        assert_eq!(out.status, EngineStatus::Ready);
        assert_eq!(out.dashboard, DashboardState::PortInUse(port));
        engine.handle(input(agents("idle")), 1);
        assert!(engine.tick(1).is_some(), "passes keep running");
        drop(busy);
        assert!(engine.tick(30).is_none(), "the bind retry waits");
        let out = engine.tick(BIND_RETRY_S).expect("bound");
        assert_eq!(out.dashboard, DashboardState::Listening(port));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dropping_the_worker_releases_the_port_and_the_lock() {
        let root = test_dir("engine-drop");
        let dir = root.join("coordinator");
        let port = free_port();
        let mut cfg = config(&dir);
        cfg.dashboard_port = port;
        let (out_tx, out_rx) = mpsc::channel();
        let worker = WorkerHandle::spawn(
            cfg,
            Box::new(move |out| {
                let _ = out_tx.send(out);
                Ok(())
            }),
        )
        .unwrap();
        // A wake reporter may still hold a sender when the handle drops.
        let _reporter = worker.sender();
        let out = out_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first output");
        assert_eq!(out.status, EngineStatus::Ready);
        assert_eq!(out.dashboard, DashboardState::Listening(port));
        let response = {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /board.json HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
                .unwrap();
            let mut text = String::new();
            stream.read_to_string(&mut text).unwrap();
            text
        };
        assert!(response.starts_with("HTTP/1.0 200 OK"), "{response}");
        assert!(lock::try_exclusive(&dir, WATCH_LOCK).unwrap().is_none());
        drop(worker);
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        assert!(lock::try_exclusive(&dir, WATCH_LOCK).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_board_suggestions_are_reported_once_and_a_corrupt_board_is_ignored() {
        let (root, mut engine) = ready("engine-board");
        let dir = root.join("coordinator");
        let board = board_path(&dir);
        std::fs::write(
            &board,
            r#"{"summary":"all calm","suggestions":[{"text":"Ask rev?","why":"x"},"Merge lead?"]}"#,
        )
        .unwrap();
        bump_mtime(&board, 1);
        let out = engine.tick(5).expect("board");
        let texts: Vec<&str> = out
            .new_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(texts, vec!["Ask rev?", "Merge lead?"]);
        assert_eq!(
            out.board.as_ref().and_then(|b| b.summary.as_deref()),
            Some("all calm")
        );
        assert!(engine.tick(6).is_none(), "nothing new");

        std::fs::write(
            &board,
            r#"{"suggestions":[{"text":"Ask rev?"},{"text":"Close the old tab?"}]}"#,
        )
        .unwrap();
        bump_mtime(&board, 2);
        let out = engine.tick(7).expect("changed board");
        let texts: Vec<&str> = out
            .new_suggestions
            .iter()
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(texts, vec!["Close the old tab?"], "only the new one");

        std::fs::write(&board, r#"{"suggestions":[{"te"#).unwrap();
        bump_mtime(&board, 3);
        let out = engine.tick(8);
        assert!(out.is_none(), "a half-written board changes nothing");
        assert_eq!(engine.board.as_ref().map(|b| b.suggestions.len()), Some(2));

        // Already seen (from coordinator.json): not new.
        let mut cfg = config(&dir);
        cfg.seen_suggestions = HashSet::from([suggestion_hash("Ask rev?")]);
        engine.lock = None;
        drop(engine);
        std::fs::write(&board, r#"{"suggestions":["Ask rev?"]}"#).unwrap();
        let mut fresh = Engine::new(cfg);
        let out = fresh.tick(9).expect("first");
        assert!(out.new_suggestions.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Move a file's mtime by whole seconds (filesystem granularity).
    fn bump_mtime(path: &Path, step: u64) {
        let at = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + step);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(at).unwrap();
    }

    #[test]
    fn live_json_is_rewritten_on_change_or_by_the_heartbeat() {
        let (root, mut engine) = ready("engine-live");
        let dir = root.join("coordinator");
        let generated = || {
            let live: LiveData =
                serde_json::from_slice(&std::fs::read(live_path(&dir)).unwrap()).unwrap();
            live.generated_unix
        };
        engine.handle(input(agents("idle")), 100);
        engine.tick(100);
        assert_eq!(generated(), 100);
        engine.tick(103);
        assert_eq!(generated(), 100, "unchanged: not rewritten");
        engine.handle(input(agents("working")), 104);
        engine.tick(104);
        assert_eq!(generated(), 104, "a change is written");
        engine.tick(113);
        assert_eq!(generated(), 104);
        engine.tick(114);
        assert_eq!(generated(), 114, "the 10 s heartbeat");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_tick_runs_before_the_first_agent_facts() {
        let (root, mut engine) = ready("engine-noinput");
        let dir = root.join("coordinator");
        engine.tick(1);
        assert!(!live_path(&dir).exists());
        engine.handle(input(agents("idle")), 2);
        engine.tick(2);
        assert!(live_path(&dir).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_coordinator_yields_a_relaunch_effect_unless_down() {
        let (root, mut engine) = ready("engine-relaunch");
        let gone = vec![agents("idle")[1].clone()];
        engine.handle(input(gone.clone()), 0);
        engine.tick(0);
        let out = engine.tick(30).expect("relaunch");
        assert_eq!(
            out.effects,
            vec![Effect::Relaunch {
                resume: Some("cs".into())
            }]
        );
        engine.handle(
            WorkerMsg::Pass(Box::new(CoordinatorPassInput {
                agents: gone.clone(),
                coordinator_down: true,
                expected_coordinator: expected(),
                ..CoordinatorPassInput::default()
            })),
            31,
        );
        let out = engine.tick(61).expect("down shows in the summary");
        assert!(out.effects.is_empty());
        assert!(out.summary.is_some_and(|summary| summary.coordinator_down));
        // A coordinator herdr does not run (off, starting) is never relaunched.
        let (root2, mut idle) = ready("engine-relaunch-off");
        idle.handle(
            WorkerMsg::Pass(Box::new(CoordinatorPassInput {
                agents: gone,
                ..CoordinatorPassInput::default()
            })),
            0,
        );
        idle.tick(0);
        assert!(idle.tick(90).is_none_or(|out| out.effects.is_empty()));
        let _ = std::fs::remove_dir_all(&root2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_requested_wake_prompts_an_idle_coordinator() {
        let (root, mut engine) = ready("engine-wake");
        engine.handle(input(agents("idle")), 0);
        engine.tick(0);
        engine.tick(10);
        engine.handle(WorkerMsg::Wake, 11);
        let out = engine.tick(11).expect("wake");
        assert_eq!(prompts(&out).len(), 1, "{out:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_migration_renames_never_merges_and_respects_a_held_lock() {
        let root = test_dir("engine-dirs");
        let legacy = root.join("plus");
        let dir = root.join("coordinator");
        assert_eq!(
            migrate_dir(Some(&legacy), &dir).unwrap(),
            DirMigration::Fresh
        );
        // Never across directories.
        let elsewhere = test_dir("engine-dirs-elsewhere").join("plus");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert_eq!(
            migrate_dir(Some(&elsewhere), &dir).unwrap(),
            DirMigration::Fresh
        );
        assert!(elsewhere.is_dir());
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("managed.json"), "{}").unwrap();
        // A legacy watcher holds its lock: no rename.
        let held = lock::try_exclusive(&legacy, WATCH_LOCK).unwrap().unwrap();
        assert_eq!(
            migrate_dir(Some(&legacy), &dir).unwrap(),
            DirMigration::LegacyLocked
        );
        assert!(!dir.exists());
        let mut cfg = config(&dir);
        cfg.legacy_dir = Some(legacy.clone());
        let mut engine = Engine::new(cfg.clone());
        let out = engine.tick(0).expect("blocked");
        assert_eq!(
            out.status,
            EngineStatus::Blocked(BlockedReason::LockedElsewhere)
        );
        drop(held);
        let out = engine.tick(LOCK_RETRY_S).expect("ready");
        assert_eq!(out.status, EngineStatus::Ready);
        assert!(out.dir_migrated);
        assert!(!legacy.exists());
        assert!(dir.join("managed.json").exists(), "renamed into place");
        engine.lock = None;
        drop(engine);
        // Both exist: the new one is used as is.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("stale"), "x").unwrap();
        assert_eq!(
            migrate_dir(Some(&legacy), &dir).unwrap(),
            DirMigration::UseExisting
        );
        assert!(!dir.join("stale").exists(), "never merged");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_reads_the_poc_coordinator_entry_and_never_writes_the_registry() {
        let root = test_dir("engine-retire");
        let dir = root.join("coordinator");
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = br#"{"agents":[{"session":"old","pane_id":"w9:p1","agent":"claude","role":"coordinator","project":"herdr+","added_unix":1}]}"#;
        std::fs::write(registry_path(&dir), v1).unwrap();
        std::fs::write(wake_request_path(&dir), "1").unwrap();
        let mut engine = Engine::new(config(&dir));
        engine.tick(0);
        assert!(
            !wake_request_path(&dir).exists(),
            "the stale request is gone"
        );
        engine.handle(WorkerMsg::Migrate, 1);
        let out = engine.tick(1).expect("migration answer");
        let migration = out.migration.expect("answered");
        assert_eq!(migration.legacy_session.as_deref(), Some("old"));
        assert_eq!(migration.legacy_pane.as_deref(), Some("w9:p1"));
        assert_eq!(
            std::fs::read(registry_path(&dir)).unwrap(),
            v1,
            "managed.json is the rollback source: byte for byte"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_registry_no_longer_blocks_the_coordinator() {
        let (root, mut engine) = ready("engine-corrupt");
        let dir = root.join("coordinator");
        std::fs::write(registry_path(&dir), "{oops").unwrap();
        engine.handle(input(agents("idle")), 1);
        let out = engine.tick(1).expect("a pass");
        assert_eq!(out.status, EngineStatus::Ready);
        assert_eq!(std::fs::read(registry_path(&dir)).unwrap(), b"{oops");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_wake_scope_decides_what_managed_lists_and_what_wakes() {
        let (root, mut engine) = ready("engine-scope");
        let dir = root.join("coordinator");
        let mut facts = agents("working");
        let mut member = fact("w3:p1", "fixer", "f", "working");
        member.member = true;
        let plain = fact("w4:p1", "solo", "p", "working");
        facts.extend([member, plain]);
        engine.handle(input(facts.clone()), 0);
        let out = engine.tick(0).expect("first pass");
        let managed: Vec<&str> = out.managed.iter().map(|a| a.pane_id.as_str()).collect();
        assert_eq!(
            managed,
            ["w1:p1", "w2:p1"],
            "opened: the coordinator and its own"
        );
        let live: LiveData =
            serde_json::from_slice(&std::fs::read(live_path(&dir)).unwrap()).unwrap();
        assert_eq!(live.agents.len(), 4, "the dashboard lists every agent");
        assert_eq!(live.wake_scope, "opened");
        // A member and a plain agent finishing create nothing under `opened`.
        for fact in facts.iter_mut().skip(2) {
            fact.status = "idle".into();
        }
        engine.handle(input(facts.clone()), 5);
        engine.tick(5);
        assert!(
            engine.state.pending.is_empty(),
            "{:?}",
            engine.state.pending
        );
        // Under `teams` the member's next edge is news, the plain agent's not.
        engine.handle(
            WorkerMsg::SetWakeCfg(WakeCfg {
                scope: watch::WakeScope::Teams,
                ..WakeCfg::default()
            }),
            6,
        );
        engine.handle(input(facts.clone()), 6);
        let out = engine.tick(6).expect("scope changed");
        assert_eq!(out.managed.len(), 3);
        for fact in facts.iter_mut().skip(2) {
            fact.status = "working".into();
        }
        engine.handle(input(facts), 7);
        engine.tick(7);
        let keys: Vec<String> = engine
            .state
            .pending
            .iter()
            .filter_map(|ev| match ev {
                watch::Ev::Status { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(keys, ["f"], "{:?}", engine.state.pending);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn new_action_log_lines_reach_live_json_after_a_baseline() {
        let (root, mut engine) = ready("engine-actions");
        let dir = root.join("coordinator");
        let entry = |id: &str| AgentActionEntry {
            unix: 1,
            id: id.into(),
            actor: crate::api::schema::agents_model::AgentActorKind::Agent,
            actor_pane: Some("w2:p1".into()),
            actor_name: Some("lead".into()),
            action: "rename_tab".into(),
            target_tab: Some("w3:t1".into()),
            target_pane: None,
            target_name: None,
            team: None,
            turn_origin: None,
            origin_detail: None,
            outcome: crate::api::schema::agents_model::AgentsActionOutcome::Denied,
            code: Some("outside_team".into()),
            detail: None,
            closed_ids: Vec::new(),
        };
        actions_log::append(&dir, &entry("a1")).unwrap();
        assert!(
            engine.read_actions(&dir).is_empty(),
            "the first read is a baseline"
        );
        assert_eq!(engine.actions.len(), 1);
        // The mtime check is coarse: force a re-read.
        engine.actions_mtime_ms = Some(0);
        actions_log::append(&dir, &entry("a2")).unwrap();
        actions_log::append(&dir, &entry("a3")).unwrap();
        let fresh: Vec<String> = engine
            .read_actions(&dir)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(fresh, ["a2", "a3"], "oldest first");
        assert!(
            engine.read_actions(&dir).is_empty(),
            "unchanged: not read again"
        );
        engine.handle(input(agents("idle")), 2);
        engine.tick(2);
        let live: LiveData =
            serde_json::from_slice(&std::fs::read(live_path(&dir)).unwrap()).unwrap();
        let ids: Vec<&str> = live.actions.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a1", "a2", "a3"], "newest last");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launch_args_resume_or_mint_a_session_and_carry_the_model() {
        let root = test_dir("engine-launch");
        let ctx = LaunchCtx {
            herdr_bin: PathBuf::from("/bin/herdr"),
            dir: root.clone(),
            port: crate::coordinator::DEFAULT_PORT,
        };
        let (id, args) = coordinator_launch_args(&ctx, Some("old"), Some("opus")).unwrap();
        assert_eq!(id, "old");
        assert_eq!(&args[..2], ["--resume", "old"]);
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(&args[separator - 2..separator], ["--model", "opus"]);
        assert!(
            args[separator + 1].contains("coordinator agent"),
            "{args:?}"
        );
        let (fresh, args) = coordinator_launch_args(&ctx, None, None).unwrap();
        assert_eq!(&args[..2], ["--session-id", fresh.as_str()]);
        assert!(!args.iter().any(|arg| arg == "--model"));
        let plain = vec!["--resume".to_string(), "x".into(), "--".into(), "go".into()];
        assert_eq!(with_model(plain.clone(), None), plain);
        assert_eq!(
            DashboardState::Listening(7718).url().as_deref(),
            Some("http://127.0.0.1:7718/")
        );
        assert_eq!(DashboardState::PortInUse(7718).url(), None);
        assert_eq!(legacy_dir_for(&root), None, "not the default dir");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_undelivered_output_is_sent_again_with_its_answers() {
        let (root, mut engine) = ready("engine-undelivered");
        engine.handle(WorkerMsg::Migrate, 1);
        let out = engine.tick(1).expect("migration answer");
        assert!(out.migration.is_some());
        let full: OutputSink = Box::new(Err);
        let running = AtomicBool::new(false);
        assert!(!deliver(
            &mut engine,
            &full,
            out,
            2,
            Duration::ZERO,
            &running
        ));
        let again = engine.tick(2).expect("resent though unchanged");
        assert!(again.migration.is_some(), "the one-shot answer rides along");
        let taken: OutputSink = Box::new(|_| Ok(()));
        assert!(deliver(
            &mut engine,
            &taken,
            again,
            2,
            Duration::ZERO,
            &running
        ));
        assert!(engine.tick(3).is_none(), "delivered: nothing new");
        // A stopping worker gives up after one try instead of sleeping
        // through its retries (the main loop is joining it).
        engine.handle(WorkerMsg::Migrate, 4);
        let out = engine.tick(4).expect("migration answer");
        let tries = std::cell::Cell::new(0);
        let counting: OutputSink = Box::new(move |out| {
            tries.set(tries.get() + 1);
            assert_eq!(tries.get(), 1, "one try only");
            Err(out)
        });
        let stopping = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!deliver(
            &mut engine,
            &counting,
            out,
            20,
            Duration::from_secs(1),
            &stopping
        ));
        assert!(started.elapsed() < Duration::from_secs(1), "no retry pause");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn board_parsing_is_lenient_and_clipped() {
        assert_eq!(parse_board(b"[1]", 0), None);
        assert_eq!(parse_board(b"{oops", 0), None);
        let long = "x".repeat(500);
        let text = format!(
            r#"{{"summary":"a\nb","suggestions":[{{"text":"{long}"}},{{"why":"no text"}},7,""]}}"#
        );
        let board = parse_board(text.as_bytes(), 9).unwrap();
        assert_eq!(board.summary.as_deref(), Some("a b"));
        assert_eq!(board.suggestions.len(), 1);
        assert_eq!(board.suggestions[0].text.chars().count(), SUGGESTION_CHARS);
        assert_eq!(board.generated_unix, 9);
        assert_eq!(suggestion_hash("a"), suggestion_hash("a"));
        assert_ne!(suggestion_hash("a"), suggestion_hash("b"));
    }
}
