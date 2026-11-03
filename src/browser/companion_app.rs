//! The companion launcher (fork, companion launcher): herdr starts, watches,
//! restarts and stops the companion browser it installed
//! (`companion_install`).
//!
//! Health is the companion lease (`companion_host`): a registration whose
//! `launch_token` is the launcher's current token makes the lease
//! *managed*. A companion started by hand registers without one
//! (`external`): herdr routes to it but never stops or restarts it. What
//! the launcher adds is the process: the child's exit (a waiter thread), an
//! adopted pid that went away (polled), no registration within 60 s, a
//! lease lost for 30 s, and the crash backoff (1/2/4/8/16 s, five crashes
//! in five minutes and it gives up).
//!
//! [`Core`] is the state machine with explicit clocks (tested without
//! processes); [`Launcher`] holds it with the spawner; the `BrowserHub`
//! methods at the end drive it: the on-demand start of `[browser] host`
//! routing, explicit start / stop / restart / focus, adoption by a new
//! server after a live handoff (`run.json`), the stop at server exit and one
//! driver thread for the timers.
//!
//! Lock order: the core's mutex is a leaf. It is never held across a spawn,
//! a signal, a sidecar request or another hub lock, and every wait releases
//! it.
//!
//! A pid from a run record (an adopted companion, another session's) is
//! only trusted, and only ever signalled, while it is still the process
//! that record names: the start token recorded at the launch tells it
//! apart from a later process that reuses the pid.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::companion_install::{self, InstallProblem, InstallRecord};
use super::hub::{BrowserHub, COMPANION_PROFILE};
use super::{unix_now, BrowserError};
use crate::api::schema::BrowserCompanionAppInfo;
use crate::config::{BrowserConfig, BrowserHostMode};

/// The run record's file under the companion home.
pub const RUN_FILE: &str = "run.json";
/// Crashes within [`CRASH_WINDOW`] before herdr stops restarting it.
pub const CRASH_LIMIT: usize = 5;
pub const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
/// How the give-up reads in messages.
pub const GIVE_UP_TEXT: &str = "5 crashes in 5 min";
/// The wait before restarting after the n-th crash in the window.
pub const BACKOFF_SECS: [u64; 5] = [1, 2, 4, 8, 16];
/// How long an `open` routed to the companion waits for it to register.
pub const REGISTER_WAIT: Duration = Duration::from_secs(45);
/// A spawned companion that has not registered by then is stopped (a crash).
pub const REGISTER_DEADLINE: Duration = Duration::from_secs(60);
/// A managed lease gone this long while its process lives: stopped (a crash).
pub const LEASE_LOST_LIMIT: Duration = Duration::from_secs(30);
/// An adopted companion gets this long to re-register before the rule above runs.
pub const ADOPT_GRACE: Duration = Duration::from_secs(15);
/// The stop: the control page's quit, then the wait for the exit, SIGTERM, SIGKILL.
pub const QUIT_WAIT: Duration = Duration::from_secs(3);
pub const EXIT_WAIT: Duration = Duration::from_secs(5);
pub const TERM_WAIT: Duration = Duration::from_secs(5);
pub const KILL_WAIT: Duration = Duration::from_secs(2);
/// A process without a live lease of its own (still starting, or its lease
/// lost) gets no quit: this grace, then SIGTERM.
pub const UNATTACHED_WAIT: Duration = Duration::from_millis(500);
/// The whole stop at server exit.
pub const SERVER_EXIT_BUDGET: Duration = Duration::from_secs(8);
/// SIGKILL's exit is always given this long to be seen, even when the
/// stop's deadline has come.
const KILL_FLOOR: Duration = Duration::from_millis(250);
/// A run record's `started_at` (stamped right after the spawn) against the
/// process's own start time, for a record without a start token.
const START_SLACK_SECS: u64 = 15;
/// `companion.log` moves to `companion.log.1` at a launch beyond this size.
pub const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// What [`log_tail`] reads from the end of the log.
const LOG_TAIL_BYTES: u64 = 64 * 1024;
/// The variables a managed companion never inherits from the server.
pub const ENV_REMOVED: [&str; 7] = [
    "HERDR_SOCKET_PATH",
    "HERDR_CLIENT_SOCKET_PATH",
    "HERDR_PANE_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "ELECTRON_RUN_AS_NODE",
    "NODE_OPTIONS",
];
/// The driver's pace while a process is watched, and while nothing is.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);
const IDLE_INTERVAL: Duration = Duration::from_secs(30);
/// `browser.companion status`'s state while the config keeps the companion
/// off and nothing of it runs; `browser.get` leaves `companion_app` out then.
pub const DISABLED_STATE: &str = "disabled";

/// Why the browser helper (the sidecar a companion registers through)
/// cannot start now ([`BrowserHub::sidecar_problem`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarProblem {
    /// Not installed (`herdr browser setup` never ran, or it was removed).
    Missing,
    /// Its node_modules predate this herdr's pinned playwright-core.
    Outdated { installed: String },
}

/// Why this config keeps the companion off: the browser is disabled, or
/// the herdr+ window is on and chosen (`host = "window"`: every
/// registration is refused). `None`: it can run. `user`: the user's own
/// call (an agent is told to ask the user).
pub fn companion_off_by_config(config: &BrowserConfig, user: bool) -> Option<BrowserError> {
    if !config.enabled {
        return Some(BrowserError::disabled());
    }
    if config.window && config.host_mode() == BrowserHostMode::Window {
        return Some(BrowserError::new(
            "host_disabled",
            format!(
                "the companion browser is off: this herdr uses only its own browser window ([browser] host = \"window\"); {} [browser] host = \"companion\" to use it",
                if user { "set" } else { "ask the user to set" }
            ),
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// The run record

/// `run.json`: the managed process, written right after the spawn and
/// removed after it exits. A new server (live handoff, restart) adopts the
/// process from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompanionRunRecord {
    pub pid: u32,
    pub launch_token: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub app_sha256: String,
    #[serde(default)]
    pub app_dir: String,
    #[serde(default)]
    pub electron: String,
    #[serde(default)]
    pub bundle: String,
    #[serde(default)]
    pub started_at: u64,
    /// The process's start token at the launch
    /// (`platform::process_start_token`): what tells it apart from a later
    /// process that reuses its pid (0: not recorded).
    #[serde(default)]
    pub start_token: u64,
    #[serde(default)]
    pub server_pid: u32,
    /// The API socket the process was told to use.
    #[serde(default)]
    pub socket: String,
    /// Started for an agent, by autostart or a crash restart (no window).
    #[serde(default)]
    pub background: bool,
}

pub fn run_record_path(home: &Path) -> PathBuf {
    home.join(RUN_FILE)
}

pub fn read_run_record(home: &Path) -> Option<CompanionRunRecord> {
    let text = std::fs::read_to_string(run_record_path(home)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_run_record(home: &Path, record: &CompanionRunRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let path = run_record_path(home);
    let tmp = home.join(format!(".{RUN_FILE}.tmp-{}", std::process::id()));
    let json = serde_json::to_string_pretty(record).map_err(std::io::Error::other)?;
    let result = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Remove the run record when it still names `token` (a newer launch's stays).
pub fn clear_run_record(home: &Path, token: Option<&str>) {
    let matches = match (read_run_record(home), token) {
        (Some(record), Some(token)) => record.launch_token == token,
        (Some(_), None) => true,
        (None, _) => false,
    };
    if matches {
        let _ = std::fs::remove_file(run_record_path(home));
    }
}

/// The app directory a live managed companion runs from (setup keeps it).
pub fn running_app_dir(home: &Path) -> Option<PathBuf> {
    read_run_record(home)
        .filter(|record| record.pid != 0 && crate::platform::process_exists(record.pid))
        .map(|record| PathBuf::from(record.app_dir))
        .filter(|dir| !dir.as_os_str().is_empty())
}

// ---------------------------------------------------------------------------
// The state machine

/// The launcher's view of its process. `not_installed` and `external` are
/// not phases: [`BrowserHub::companion_app_info`] derives them from the
/// install and the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Reconnecting,
    Stopping,
    Crashed,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Reconnecting => "reconnecting",
            Self::Stopping => "stopping",
            Self::Crashed => "crashed",
            Self::Failed => "failed",
        }
    }
}

/// Why a launch happens: everything but the user's own start runs in the
/// background (the window is never shown and the app never activated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartReason {
    /// The TUI, or the CLI from a plain terminal.
    User,
    /// The CLI (or the API) from a herdr pane: an agent, or the user in a pane.
    Agent,
    /// An `open` / `show` routed to the companion.
    OnDemand,
    /// `[browser] companion_autostart`.
    Autostart,
    CrashRestart,
}

impl StartReason {
    pub fn background(self) -> bool {
        !matches!(self, Self::User)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    Term,
    Kill,
}

/// How a process ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// The status is known (our own child); an adopted process's is not.
    pub known: bool,
}

impl ExitInfo {
    pub fn from_status(status: std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: status.code(),
            signal,
            known: true,
        }
    }

    pub fn unknown() -> Self {
        Self::default()
    }

    pub fn describe(&self) -> String {
        match (self.known, self.code, self.signal) {
            (false, _, _) => "exited (status unknown)".into(),
            (true, Some(code), _) => format!("exited with status {code}"),
            (true, None, Some(signal)) => format!("killed by signal {signal}"),
            (true, None, None) => "exited".into(),
        }
    }
}

/// A launch to run: its generation and token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnTicket {
    pub generation: u64,
    pub token: String,
    pub background: bool,
}

/// What a stop finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopTarget {
    /// This process: the caller stops it.
    Process {
        pid: u32,
        generation: u64,
        has_child: bool,
    },
    /// A launch is in flight: it runs the stop on what it spawns, or spawns
    /// nothing.
    Launch { generation: u64 },
    /// No process lives (it is `stopped`, or still `failed`).
    Nothing,
}

/// A stop that came while a launch was in flight: its waits and its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingStop {
    ladder: Ladder,
    deadline: Instant,
}

/// What the driver must do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A crash restart is due.
    Respawn,
    Signal {
        pid: u32,
        generation: u64,
        signal: StopSignal,
    },
    /// An adopted process: is it still there?
    Poll { pid: u32, generation: u64 },
}

#[derive(Debug, Clone)]
pub struct Core {
    pub phase: Phase,
    pub since_unix: u64,
    /// Bumped by every launch and adoption: late reports of an older process are ignored.
    pub generation: u64,
    /// The current process's launch token.
    pub token: Option<String>,
    pub pid: Option<u32>,
    pub started_unix: Option<u64>,
    spawned_at: Option<Instant>,
    adopted_at: Option<Instant>,
    /// The current token registered.
    pub registered: bool,
    /// The current process unregistered (it is quitting on its own).
    unregistered: bool,
    lease_lost_since: Option<Instant>,
    crashes: VecDeque<Instant>,
    next_retry: Option<Instant>,
    pub next_retry_unix: Option<u64>,
    pub last_error: Option<String>,
    /// herdr is stopping the process for this reason (a crash when it goes).
    kill_reason: Option<String>,
    term_sent_at: Option<Instant>,
    kill_sent: bool,
    pub background: bool,
    pub run: Option<CompanionRunRecord>,
    /// A launch is under way (single flight).
    pub spawning: bool,
    /// A waiter thread reports the exit (else the driver polls the pid).
    has_child: bool,
    /// A stop that came while the launch was in flight: the launch runs it
    /// on what it spawns, or spawns nothing.
    stop_pending: Option<PendingStop>,
    /// The server is exiting: nothing launches from now on.
    pub shutting_down: bool,
}

impl Default for Core {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Core {
    pub fn new(now_unix: u64) -> Self {
        Self {
            phase: Phase::Stopped,
            since_unix: now_unix,
            generation: 0,
            token: None,
            pid: None,
            started_unix: None,
            spawned_at: None,
            adopted_at: None,
            registered: false,
            unregistered: false,
            lease_lost_since: None,
            crashes: VecDeque::new(),
            next_retry: None,
            next_retry_unix: None,
            last_error: None,
            kill_reason: None,
            term_sent_at: None,
            kill_sent: false,
            background: false,
            run: None,
            spawning: false,
            has_child: false,
            stop_pending: None,
            shutting_down: false,
        }
    }

    fn set_phase(&mut self, phase: Phase, now_unix: u64) {
        if self.phase != phase {
            self.phase = phase;
            self.since_unix = now_unix;
        }
    }

    /// The process lives or is being started or stopped.
    pub fn busy(&self) -> bool {
        self.spawning
            || self.pid.is_some()
            || matches!(
                self.phase,
                Phase::Starting | Phase::Running | Phase::Reconnecting | Phase::Stopping
            )
    }

    /// Crashes within the window ending at `now`.
    pub fn crashes_in_window(&self, now: Instant) -> u32 {
        self.crashes
            .iter()
            .filter(|at| now.duration_since(**at) < CRASH_WINDOW)
            .count() as u32
    }

    /// An explicit start or restart: the backoff counter goes, `failed` and
    /// a scheduled restart are cleared.
    pub fn reset_backoff(&mut self, now_unix: u64) {
        self.crashes.clear();
        self.next_retry = None;
        self.next_retry_unix = None;
        self.last_error = None;
        if matches!(self.phase, Phase::Failed | Phase::Crashed) {
            self.set_phase(Phase::Stopped, now_unix);
        }
    }

    /// Begin a launch; `None` when one is under way, the process lives, it
    /// gave up (`failed`) or the server is exiting.
    pub fn begin_spawn(
        &mut self,
        reason: StartReason,
        now: Instant,
        now_unix: u64,
    ) -> Option<SpawnTicket> {
        if self.busy() || self.phase == Phase::Failed || self.shutting_down {
            return None;
        }
        // a crash restart the driver found due loses to a stop (or a start)
        // that came in between
        if reason == StartReason::CrashRestart && self.phase != Phase::Crashed {
            return None;
        }
        self.generation += 1;
        let token = super::session::random_hex(32);
        self.token = Some(token.clone());
        self.spawning = true;
        self.registered = false;
        self.unregistered = false;
        self.kill_reason = None;
        self.term_sent_at = None;
        self.kill_sent = false;
        self.lease_lost_since = None;
        self.adopted_at = None;
        self.next_retry = None;
        self.next_retry_unix = None;
        self.background = reason.background();
        self.spawned_at = Some(now);
        self.started_unix = None;
        self.run = None;
        self.set_phase(Phase::Starting, now_unix);
        Some(SpawnTicket {
            generation: self.generation,
            token,
            background: self.background,
        })
    }

    /// The launch ran: its pid and record.
    pub fn spawned(
        &mut self,
        generation: u64,
        pid: u32,
        record: CompanionRunRecord,
        has_child: bool,
        now_unix: u64,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.spawning = false;
        self.pid = Some(pid);
        self.started_unix = Some(now_unix);
        self.run = Some(record);
        self.has_child = has_child;
        true
    }

    /// Right before the spawn: whether this launch still goes ahead. A stop
    /// that came while it got ready, or the server's exit, calls it off:
    /// nothing is spawned and it is `stopped`.
    pub fn launch_goes_ahead(&mut self, generation: u64, now_unix: u64) -> bool {
        if generation != self.generation || !self.spawning {
            return false;
        }
        if self.stop_pending.is_some() || self.shutting_down {
            self.spawning = false;
            self.stop_pending = None;
            self.set_phase(Phase::Stopped, now_unix);
            return false;
        }
        true
    }

    /// The launch itself failed (no executable, …): a crash with that error
    /// (a stop that waited for it, or the server's exit: stopped).
    pub fn spawn_failed(&mut self, generation: u64, error: String, now: Instant, now_unix: u64) {
        if generation != self.generation {
            return;
        }
        self.spawning = false;
        if self.stop_pending.take().is_some() || self.shutting_down {
            self.last_error = Some(error);
            self.set_phase(Phase::Stopped, now_unix);
            return;
        }
        self.crash(error, now, now_unix);
    }

    /// A new server takes over a live companion from its run record.
    pub fn adopt(&mut self, record: CompanionRunRecord, now: Instant, now_unix: u64) {
        self.generation += 1;
        self.token = Some(record.launch_token.clone());
        self.pid = Some(record.pid);
        self.started_unix = Some(record.started_at).filter(|at| *at > 0);
        self.background = record.background;
        self.run = Some(record);
        self.spawning = false;
        self.has_child = false;
        self.registered = false;
        self.unregistered = false;
        self.kill_reason = None;
        self.term_sent_at = None;
        self.kill_sent = false;
        self.lease_lost_since = None;
        self.spawned_at = None;
        self.adopted_at = Some(now);
        self.set_phase(Phase::Reconnecting, now_unix);
    }

    /// A registration landed: `true` when it carried the current token
    /// (the lease is managed).
    pub fn registered(&mut self, token: Option<&str>, now_unix: u64) -> bool {
        let current = token.is_some() && token == self.token.as_deref();
        if current {
            self.registered = true;
            self.unregistered = false;
            self.lease_lost_since = None;
            if matches!(self.phase, Phase::Starting | Phase::Reconnecting) {
                self.set_phase(Phase::Running, now_unix);
            }
        }
        current
    }

    /// The current process unregistered: it is quitting on its own.
    pub fn unregistered(&mut self, token: Option<&str>) {
        if token.is_some() && token == self.token.as_deref() {
            self.unregistered = true;
            self.registered = false;
        }
    }

    /// The process of `generation` is gone. `tail` gives its last log lines
    /// for a crash. Answers whether it was the current process (its run
    /// record goes).
    pub fn exited(
        &mut self,
        generation: u64,
        exit: &ExitInfo,
        tail: &[String],
        now: Instant,
        now_unix: u64,
    ) -> bool {
        if generation != self.generation || (self.pid.is_none() && !self.spawning) {
            return false;
        }
        self.pid = None;
        self.spawning = false;
        self.stop_pending = None;
        self.has_child = false;
        self.run = None;
        self.registered = false;
        self.started_unix = None;
        let stopping = self.phase == Phase::Stopping;
        let killed = self.kill_reason.take();
        self.term_sent_at = None;
        self.kill_sent = false;
        if stopping {
            self.set_phase(Phase::Stopped, now_unix);
            return true;
        }
        if killed.is_none() && (self.unregistered || exit.code == Some(0)) {
            // it quit on its own (the user closed it): not a crash
            self.set_phase(Phase::Stopped, now_unix);
            return true;
        }
        let mut error = killed.unwrap_or_else(|| exit.describe());
        if !tail.is_empty() {
            error.push_str(" — ");
            error.push_str(&tail.join(" | ").chars().take(400).collect::<String>());
        }
        self.crash(error, now, now_unix);
        true
    }

    fn crash(&mut self, error: String, now: Instant, now_unix: u64) {
        self.kill_reason = None;
        while self
            .crashes
            .front()
            .is_some_and(|at| now.duration_since(*at) >= CRASH_WINDOW)
        {
            self.crashes.pop_front();
        }
        self.crashes.push_back(now);
        self.last_error = Some(error);
        if self.crashes.len() >= CRASH_LIMIT {
            self.next_retry = None;
            self.next_retry_unix = None;
            self.set_phase(Phase::Failed, now_unix);
        } else {
            let delay = BACKOFF_SECS[(self.crashes.len() - 1).min(BACKOFF_SECS.len() - 1)];
            self.next_retry = Some(now + Duration::from_secs(delay));
            self.next_retry_unix = Some(now_unix + delay);
            self.set_phase(Phase::Crashed, now_unix);
        }
    }

    /// The user (or the server's exit) stops it, on `ladder` by `deadline`.
    /// A scheduled restart goes either way. A launch in flight takes the
    /// stop over: it runs it on what it spawns, or spawns nothing (and no
    /// start is accepted until then).
    pub fn begin_stop(&mut self, ladder: Ladder, deadline: Instant, now_unix: u64) -> StopTarget {
        self.next_retry = None;
        self.next_retry_unix = None;
        if let Some(pid) = self.pid {
            self.set_phase(Phase::Stopping, now_unix);
            return StopTarget::Process {
                pid,
                generation: self.generation,
                has_child: self.has_child,
            };
        }
        if self.spawning {
            self.stop_pending = Some(PendingStop { ladder, deadline });
            self.set_phase(Phase::Stopping, now_unix);
            return StopTarget::Launch {
                generation: self.generation,
            };
        }
        if self.phase != Phase::Failed {
            self.set_phase(Phase::Stopped, now_unix);
        }
        StopTarget::Nothing
    }

    /// The stop could not end the process: let it go as stopped, with why.
    pub fn abandon(&mut self, generation: u64, why: String, now_unix: u64) {
        if generation != self.generation {
            return;
        }
        self.pid = None;
        self.stop_pending = None;
        self.run = None;
        self.has_child = false;
        self.last_error = Some(why);
        self.set_phase(Phase::Stopped, now_unix);
    }

    fn begin_kill(&mut self, reason: &str, now: Instant, out: &mut Vec<Action>) {
        self.kill_reason = Some(reason.to_string());
        self.term_sent_at = Some(now);
        self.kill_sent = false;
        if let Some(pid) = self.pid {
            out.push(Action::Signal {
                pid,
                generation: self.generation,
                signal: StopSignal::Term,
            });
        }
    }

    /// One driver pass at `now`; `lease_live` is whether a lease lives.
    pub fn due(&mut self, now: Instant, lease_live: bool) -> Vec<Action> {
        let mut out = Vec::new();
        match self.phase {
            Phase::Crashed => {
                if self.next_retry.is_some_and(|at| at <= now) {
                    out.push(Action::Respawn);
                }
            }
            Phase::Starting => {
                if !self.registered
                    && self.kill_reason.is_none()
                    && self.pid.is_some()
                    && self
                        .spawned_at
                        .is_some_and(|at| now.duration_since(at) >= REGISTER_DEADLINE)
                {
                    self.begin_kill(
                        &format!("did not register within {} s", REGISTER_DEADLINE.as_secs()),
                        now,
                        &mut out,
                    );
                }
            }
            Phase::Running | Phase::Reconnecting => {
                if lease_live {
                    self.lease_lost_since = None;
                } else if self.kill_reason.is_none() {
                    let since = *self.lease_lost_since.get_or_insert(now);
                    let graced = self
                        .adopted_at
                        .is_none_or(|at| now.duration_since(at) >= ADOPT_GRACE);
                    if graced && now.duration_since(since) >= LEASE_LOST_LIMIT {
                        self.begin_kill("stopped heartbeating", now, &mut out);
                    }
                }
            }
            _ => {}
        }
        if self.kill_reason.is_some()
            && !self.kill_sent
            && self
                .term_sent_at
                .is_some_and(|at| now.duration_since(at) >= TERM_WAIT)
        {
            if let Some(pid) = self.pid {
                self.kill_sent = true;
                out.push(Action::Signal {
                    pid,
                    generation: self.generation,
                    signal: StopSignal::Kill,
                });
            }
        }
        if !self.has_child {
            if let Some(pid) = self.pid {
                out.push(Action::Poll {
                    pid,
                    generation: self.generation,
                });
            }
        }
        out
    }

    /// How long the driver may sleep after a pass at `now`.
    pub fn next_wake(&self, now: Instant) -> Duration {
        if self.busy() {
            return WATCH_INTERVAL;
        }
        match self.next_retry {
            Some(at) => at.saturating_duration_since(now).min(WATCH_INTERVAL),
            None => IDLE_INTERVAL,
        }
    }
}

// ---------------------------------------------------------------------------
// Processes

/// What a launch runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub executable: PathBuf,
    pub app_dir: PathBuf,
    pub args: Vec<String>,
    pub log: PathBuf,
}

/// A launched process: its pid, and the child when this process owns it.
pub struct Spawned {
    pub pid: u32,
    pub child: Option<std::process::Child>,
}

/// What tells a live process apart from a later one that reuses its pid
/// (each `None` where this platform cannot read it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessFacts {
    /// `platform::process_start_token`.
    pub start_token: Option<u64>,
    /// `platform::process_start_unix_ms`.
    pub started_ms: Option<u64>,
}

/// What a run record's pid is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedProcess {
    /// The process the record names (its start token matches, else its
    /// start time).
    Same,
    /// No process has that pid, or another one does (a reused pid).
    Gone,
    /// A live pid nothing tells apart (no start token or time to compare):
    /// never adopted, never signalled.
    Unknown,
}

/// What `record`'s pid is now, from `facts` about it (`None`: no process
/// has that pid).
pub fn match_record(record: &CompanionRunRecord, facts: Option<&ProcessFacts>) -> RecordedProcess {
    let Some(facts) = facts.filter(|_| record.pid != 0) else {
        return RecordedProcess::Gone;
    };
    let same = |yes: bool| {
        if yes {
            RecordedProcess::Same
        } else {
            RecordedProcess::Gone
        }
    };
    if let (true, Some(token)) = (record.start_token != 0, facts.start_token) {
        return same(token == record.start_token);
    }
    if let (true, Some(ms)) = (record.started_at != 0, facts.started_ms) {
        return same((ms / 1000).abs_diff(record.started_at) <= START_SLACK_SECS);
    }
    RecordedProcess::Unknown
}

/// Starting, signalling and probing companion processes (a fake in tests).
pub trait Spawner: Send + Sync {
    fn spawn(&self, spec: &LaunchSpec) -> std::io::Result<Spawned>;
    fn signal(&self, pid: u32, signal: StopSignal);
    /// What tells a live `pid` apart from a later process that reuses it;
    /// `None` when no process has that pid.
    fn facts(&self, pid: u32) -> Option<ProcessFacts>;
}

/// The real thing: the inner binary, executed directly (never `open`:
/// LaunchServices would activate it), its own session, stdio to the log
/// file (never a pipe: the process outlives the server that spawned it).
pub struct ProcessSpawner;

impl Spawner for ProcessSpawner {
    fn spawn(&self, spec: &LaunchSpec) -> std::io::Result<Spawned> {
        rotate_log(&spec.log);
        if let Some(parent) = spec.log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.log)?;
        let mut command = std::process::Command::new(&spec.executable);
        command
            .arg(&spec.app_dir)
            .args(&spec.args)
            .current_dir(&spec.app_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log.try_clone()?))
            .stderr(std::process::Stdio::from(log));
        for key in ENV_REMOVED {
            command.env_remove(key);
        }
        command.env("HERDR_COMPANION_MANAGED", "1");
        crate::platform::detach_background_command(&mut command);
        let child = command.spawn()?;
        Ok(Spawned {
            pid: child.id(),
            child: Some(child),
        })
    }

    fn signal(&self, pid: u32, signal: StopSignal) {
        match signal {
            StopSignal::Term => super::launch::terminate(pid),
            StopSignal::Kill => super::launch::kill_hard(pid),
        }
    }

    fn facts(&self, pid: u32) -> Option<ProcessFacts> {
        (pid != 0 && crate::platform::process_exists(pid)).then(|| ProcessFacts {
            start_token: crate::platform::process_start_token(pid),
            started_ms: crate::platform::process_start_unix_ms(pid),
        })
    }
}

fn rotate_log(log: &Path) {
    if std::fs::metadata(log).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        let _ = std::fs::rename(log, PathBuf::from(rotated));
    }
}

/// `45s`, `3m 5s`, `2h 3m`, `1d 2h`: an uptime or a countdown.
pub fn duration_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => {
            let (m, s) = (secs / 60, secs % 60);
            if s == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {s}s")
            }
        }
        3600..=86_399 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// The last `n` non-empty lines of the log (a crash's error), from its
/// last [`LOG_TAIL_BYTES`] only.
pub fn log_tail(log: &Path, n: usize) -> Vec<String> {
    let Ok(mut file) = std::fs::File::open(log) else {
        return Vec::new();
    };
    let start = file
        .metadata()
        .map_or(0, |meta| meta.len().saturating_sub(LOG_TAIL_BYTES));
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err()
        || file.take(LOG_TAIL_BYTES).read_to_end(&mut bytes).is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // a read that starts mid-file starts mid-line: that piece is no line
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.chars().take(200).collect())
        .collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

// ---------------------------------------------------------------------------
// The launcher

/// The launcher a hub owns: the core, the condition every transition
/// notifies, the spawner.
pub struct Launcher {
    core: Mutex<Core>,
    changed: Condvar,
    driver: AtomicBool,
    spawner: Mutex<Arc<dyn Spawner>>,
    /// Tests: the API socket the launcher passes and adoption compares.
    socket: Mutex<Option<PathBuf>>,
    /// install.json, re-read when its mtime moves (`browser.get` is polled).
    install: Mutex<Option<(Option<std::time::SystemTime>, Option<InstallRecord>)>>,
}

impl Default for Launcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Launcher {
    pub fn new() -> Self {
        Self {
            core: Mutex::new(Core::new(unix_now())),
            changed: Condvar::new(),
            driver: AtomicBool::new(false),
            spawner: Mutex::new(Arc::new(ProcessSpawner)),
            socket: Mutex::new(None),
            install: Mutex::new(None),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Core> {
        self.core
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn notify(&self) {
        self.changed.notify_all();
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, Core>, timeout: Duration) -> MutexGuard<'a, Core> {
        match self.changed.wait_timeout(guard, timeout) {
            Ok((guard, _)) => guard,
            Err(poisoned) => poisoned.into_inner().0,
        }
    }

    pub fn spawner(&self) -> Arc<dyn Spawner> {
        self.spawner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Tests: a fake spawner.
    #[cfg(test)]
    pub fn set_spawner(&self, spawner: Arc<dyn Spawner>) {
        *self
            .spawner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = spawner;
    }

    /// The server's API socket (what the companion connects to).
    pub fn socket(&self) -> PathBuf {
        self.socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .unwrap_or_else(crate::api::socket_path)
    }

    /// Tests: the socket a launch passes and adoption compares.
    #[cfg(test)]
    pub fn set_socket(&self, socket: PathBuf) {
        *self
            .socket
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(socket);
    }

    /// install.json, cached until its mtime moves.
    pub fn install_record(&self, home: &Path) -> Option<InstallRecord> {
        let path = home.join(companion_install::INSTALL_FILE);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let mut cache = self
            .install
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((seen, record)) = cache.as_ref() {
            if *seen == mtime {
                return record.clone();
            }
        }
        let record = companion_install::read_install(home);
        *cache = Some((mtime, record.clone()));
        record
    }
}

// ---------------------------------------------------------------------------
// Errors (exact texts: the design's §2.2). `user`: the user's own call (the
// TUI, the CLI from a plain terminal) is told what to run; an agent is told
// to ask the user.

fn run_it(user: bool) -> &'static str {
    if user {
        "run"
    } else {
        "ask the user to run"
    }
}

fn install_error(problem: InstallProblem, user: bool) -> BrowserError {
    let run = run_it(user);
    BrowserError::new(
        "host_unavailable",
        match problem {
            InstallProblem::NotInstalled => format!(
                "the companion browser is not installed; {run} \"herdr browser setup --companion\" (downloads Electron, {})",
                companion_install::DOWNLOAD_SIZE
            ),
            InstallProblem::NoElectron => format!("the companion browser is installed without Electron; {run} \"herdr browser setup --companion\" in a terminal"),
            InstallProblem::Unsupported(why) => format!("{why}; set [browser] window = true to use the herdr+ window"),
        },
    )
}

/// The browser helper is missing or outdated: a companion started now could
/// never register (it registers through it).
fn sidecar_error(problem: &SidecarProblem, user: bool) -> BrowserError {
    let run = run_it(user);
    BrowserError::new(
        "host_unavailable",
        match problem {
            SidecarProblem::Missing => format!(
                "the browser helper the companion browser registers through is not installed; {run} \"herdr browser setup\" in a terminal"
            ),
            SidecarProblem::Outdated { installed } => format!(
                "the browser helper is outdated (playwright-core {installed}, this herdr needs {}); {run} \"herdr browser setup\" in a terminal",
                crate::integration::browser_assets::PLAYWRIGHT_CORE_VERSION
            ),
        },
    )
}

/// Neither the companion nor the browser helper is installed: both commands.
fn nothing_installed_error(user: bool) -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        format!(
            "the companion browser and the browser helper are not installed; {} \"herdr browser setup\" and \"herdr browser setup --companion\" (downloads Electron, {})",
            run_it(user),
            companion_install::DOWNLOAD_SIZE
        ),
    )
}

fn failed_error(last: Option<&str>, user: bool) -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        format!(
            "the companion browser crashed {CRASH_LIMIT} times in 5 minutes and herdr stopped restarting it (last error: {}); {} \"herdr browser companion restart\"",
            last.unwrap_or("unknown"),
            run_it(user)
        ),
    )
}

fn timeout_error(log: &Path, waited: Duration, user: bool) -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        format!(
            "the companion browser did not start within {} s (log: {}); retry once, then {} \"herdr browser companion status\"",
            waited.as_secs(),
            log.display(),
            run_it(user)
        ),
    )
}

fn exited_error(log: &Path, user: bool) -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        format!(
            "the companion browser exited before it registered (log: {}); retry once, then {} \"herdr browser companion status\"",
            log.display(),
            run_it(user)
        ),
    )
}

fn other_session_error(socket: &str) -> BrowserError {
    BrowserError::new(
        "host_unavailable",
        format!("the companion runs for another herdr session ({socket}); stop it there first"),
    )
}

/// What `browser.companion stop` / `restart` answer for a companion started by hand.
pub const EXTERNAL_NOTE: &str = "this companion was not started by herdr; quit it from its window";

// ---------------------------------------------------------------------------
// The hub's side

/// The companion lease as the launcher needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeaseFacts {
    pub live: bool,
    pub launch_token: Option<String>,
    pub version: String,
    pub pid: Option<u32>,
}

/// A stop's waits (the server's exit is shorter). Every wait also ends at
/// the stop's one deadline, `total()` after it began; the control page's
/// quit is sent without waiting for its reply (the exit is the answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ladder {
    /// The exit after the control page's quit.
    quit: Duration,
    /// No live lease of its own (no quit sent): the grace before SIGTERM.
    unattached: Duration,
    /// The exit after SIGTERM.
    term: Duration,
    /// The exit after SIGKILL.
    kill: Duration,
}

impl Ladder {
    /// The whole stop (an unattached grace is never longer than the quit's wait).
    const fn total(&self) -> Duration {
        Duration::from_millis(
            (self.quit.as_millis() + self.term.as_millis() + self.kill.as_millis()) as u64,
        )
    }
}

const USER_LADDER: Ladder = Ladder {
    quit: Duration::from_secs(QUIT_WAIT.as_secs() + EXIT_WAIT.as_secs()),
    unattached: UNATTACHED_WAIT,
    term: TERM_WAIT,
    kill: KILL_WAIT,
};

/// At most [`SERVER_EXIT_BUDGET`] in all.
const SERVER_LADDER: Ladder = Ladder {
    quit: Duration::from_secs(4),
    unattached: UNATTACHED_WAIT,
    term: Duration::from_secs(3),
    kill: Duration::from_secs(1),
};
const _: () = assert!(
    SERVER_LADDER.total().as_millis() <= SERVER_EXIT_BUDGET.as_millis()
        && SERVER_LADDER.unattached.as_millis() <= SERVER_LADDER.quit.as_millis()
        && USER_LADDER.unattached.as_millis() <= USER_LADDER.quit.as_millis()
);

impl BrowserHub {
    /// The companion's home (`<browser home>/companion-app`).
    pub fn companion_home(&self) -> PathBuf {
        companion_install::companion_home(self.home_dir())
    }

    /// `browser.get` must re-pull: the launcher changed.
    fn companion_changed(&self) {
        self.launcher().notify();
        self.note_session_change();
    }

    /// What a launch needs in place, checked before it begins (never under
    /// the core's lock): the companion's install and the browser helper it
    /// registers through. Without the helper a launched companion never
    /// registers: the agent's `open` would wait out its whole wait and the
    /// launcher kill it as a crash, again and again.
    fn companion_launchable(&self, home: &Path, user: bool) -> Result<(), BrowserError> {
        let helper = self.sidecar_problem();
        match (companion_install::resolve(home), helper) {
            (Err(InstallProblem::NotInstalled), Some(SidecarProblem::Missing)) => {
                Err(nothing_installed_error(user))
            }
            (Err(problem), _) => Err(install_error(problem, user)),
            (Ok(_), Some(problem)) => Err(sidecar_error(&problem, user)),
            (Ok(_), None) => Ok(()),
        }
    }

    /// `browser.companion status`'s note while the config keeps the
    /// companion off (it says why, and how to turn it on).
    pub fn companion_off_note(&self) -> Option<String> {
        companion_off_by_config(&self.config(), true).map(|err| err.message)
    }

    /// A live run record of another herdr session (its socket).
    fn companion_other_session(&self, home: &Path) -> Option<String> {
        let ours = self.launcher().socket();
        let record = read_run_record(home)?;
        (record.socket != ours.display().to_string()
            && self.record_process(&record) == RecordedProcess::Same)
            .then_some(record.socket)
    }

    /// What a run record's pid is now: its process, gone, or another one.
    fn record_process(&self, record: &CompanionRunRecord) -> RecordedProcess {
        let facts = (record.pid != 0)
            .then(|| self.launcher().spawner().facts(record.pid))
            .flatten();
        match_record(record, facts.as_ref())
    }

    /// The process of `generation`, which herdr has no child of (adopted):
    /// what its run record's pid is now (`Gone` once it is not current).
    fn companion_adopted_process(&self, generation: u64, pid: u32) -> RecordedProcess {
        let record = {
            let core = self.launcher().lock();
            (core.generation == generation)
                .then(|| core.run.clone())
                .flatten()
        };
        match record {
            Some(record) if record.pid == pid => self.record_process(&record),
            _ => RecordedProcess::Gone,
        }
    }

    /// `[browser] host` routed an `open` / `show` to the companion and no
    /// lease lives: start it (one launch for every caller) and wait for its
    /// registration, at most `wait`. On the caller's connection thread;
    /// never with a hub, profile or host lock held. `user`: the user's own
    /// call (its errors say what to run; an agent's say to ask the user).
    pub fn ensure_companion(&self, wait: Duration, user: bool) -> Result<(), BrowserError> {
        if self.companion_live() {
            return Ok(());
        }
        // a companion this config refuses to register is never started
        if let Some(err) = companion_off_by_config(&self.config(), user) {
            return Err(err);
        }
        let home = self.companion_home();
        let launcher = self.launcher();
        let deadline = Instant::now() + wait;
        let mut launched: Option<u64> = None;
        loop {
            if self.companion_live() {
                return Ok(());
            }
            let (phase, spawning, generation, last_error, shutting_down) = {
                let core = launcher.lock();
                (
                    core.phase,
                    core.spawning,
                    core.generation,
                    core.last_error.clone(),
                    core.shutting_down,
                )
            };
            if shutting_down {
                return Err(BrowserError::new(
                    "host_unavailable",
                    "herdr is shutting down; the companion browser is not started now",
                ));
            }
            match phase {
                Phase::Failed => return Err(failed_error(last_error.as_deref(), user)),
                Phase::Stopped if !spawning => {
                    if launched == Some(generation) {
                        return Err(exited_error(&companion_install::log_path(&home), user));
                    }
                    // checked before each launch, never under the core's lock
                    if let Some(socket) = self.companion_other_session(&home) {
                        return Err(other_session_error(&socket));
                    }
                    self.companion_launchable(&home, user)?;
                    let ticket = launcher.lock().begin_spawn(
                        StartReason::OnDemand,
                        Instant::now(),
                        unix_now(),
                    );
                    if let Some(ticket) = ticket {
                        launched = Some(ticket.generation);
                        self.companion_changed();
                        self.companion_launch(ticket);
                        continue;
                    }
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(timeout_error(
                    &companion_install::log_path(&home),
                    wait,
                    user,
                ));
            }
            let core = launcher.lock();
            if core.generation == generation && core.phase == phase {
                drop(launcher.wait(core, (deadline - now).min(Duration::from_millis(250))));
            }
        }
    }

    /// An explicit start (the TUI, the CLI, `browser.start companion`):
    /// answers at once; the launch runs on its own thread. The backoff
    /// counter and `failed` are reset.
    pub fn companion_start(&self, reason: StartReason) -> Result<(), BrowserError> {
        let user = reason == StartReason::User;
        if let Some(err) = companion_off_by_config(&self.config(), user) {
            return Err(err);
        }
        let facts = self.companion_lease_facts();
        if facts.live && !self.lease_is_managed(&facts) {
            return Ok(()); // a companion started by hand runs: nothing to start
        }
        let home = self.companion_home();
        if let Some(socket) = self.companion_other_session(&home) {
            return Err(other_session_error(&socket));
        }
        self.companion_launchable(&home, user)?;
        let ticket = {
            let mut core = self.launcher().lock();
            core.reset_backoff(unix_now());
            core.begin_spawn(reason, Instant::now(), unix_now())
        };
        self.companion_changed();
        if let Some(ticket) = ticket {
            let hub = self.clone();
            std::thread::Builder::new()
                .name("herdr-companion-start".into())
                .spawn(move || hub.companion_launch(ticket))
                .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        }
        Ok(())
    }

    /// Stop the companion: the managed one quits (control page, SIGTERM,
    /// SIGKILL) on its own thread; one started by hand is only detached
    /// (it re-registers) and the answer says so.
    pub fn companion_stop(&self) -> Result<Option<String>, BrowserError> {
        let facts = self.companion_lease_facts();
        if facts.live && !self.lease_is_managed(&facts) {
            self.detach_external_companion();
            return Ok(Some(EXTERNAL_NOTE.into()));
        }
        let hub = self.clone();
        std::thread::Builder::new()
            .name("herdr-companion-stop".into())
            .spawn(move || hub.companion_stop_blocking(USER_LADDER))
            .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        Ok(None)
    }

    /// Stop, then start again (backoff reset; the installed app files: an
    /// update applies), on its own thread.
    pub fn companion_restart(&self, reason: StartReason) -> Result<Option<String>, BrowserError> {
        let user = reason == StartReason::User;
        // nothing is stopped for a start that cannot follow
        if let Some(err) = companion_off_by_config(&self.config(), user) {
            return Err(err);
        }
        let facts = self.companion_lease_facts();
        if facts.live && !self.lease_is_managed(&facts) {
            return Ok(Some(EXTERNAL_NOTE.into()));
        }
        let home = self.companion_home();
        self.companion_launchable(&home, user)?;
        let hub = self.clone();
        std::thread::Builder::new()
            .name("herdr-companion-restart".into())
            .spawn(move || {
                hub.companion_stop_blocking(USER_LADDER);
                if let Err(err) = hub.companion_start(reason) {
                    tracing::warn!(event = "browser.companion.restart", code = %err.code, message = %err.message, "companion restart: the start failed");
                }
            })
            .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        Ok(None)
    }

    /// Bring the companion's window up: `user` activates it (keyboard
    /// focus), an agent's only orders it front. A user focus of a stopped
    /// companion starts it (its window shows).
    pub fn companion_focus(&self, user: bool) -> Result<(), BrowserError> {
        if !self.companion_live() {
            if user {
                return self.companion_start(StartReason::User);
            }
            return Err(BrowserError::new(
                "host_unavailable",
                "the companion browser is not running; `herdr browser companion start` starts it",
            ));
        }
        let hub = self.clone();
        std::thread::Builder::new()
            .name("herdr-companion-focus".into())
            .spawn(move || {
                if let Err(err) = hub.request(
                    "control_raise",
                    Some(COMPANION_PROFILE),
                    None,
                    json!({ "user": user }),
                    Duration::from_secs(3),
                ) {
                    tracing::debug!(event = "browser.companion.focus", code = %err.code, "companion raise failed");
                }
            })
            .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        Ok(())
    }

    /// Whether the live lease is the launcher's own process.
    fn lease_is_managed(&self, facts: &LeaseFacts) -> bool {
        facts.launch_token.is_some() && facts.launch_token == self.launcher().lock().token
    }

    /// A registration landed (`host_register`, after its lease was
    /// minted): the launcher's own token makes it managed.
    pub(crate) fn companion_registered(&self, token: Option<&str>) -> bool {
        let managed = self.launcher().lock().registered(token, unix_now());
        self.companion_changed();
        managed
    }

    /// The lease's process unregistered (it quits on its own).
    pub(crate) fn companion_unregistered(&self, token: Option<&str>) {
        self.launcher().lock().unregistered(token);
        self.companion_changed();
    }

    /// The installed version a running managed companion is not on yet.
    pub(crate) fn companion_update_ready(&self) -> Option<String> {
        let install = self.launcher().install_record(&self.companion_home())?;
        let core = self.launcher().lock();
        let run = core.run.as_ref()?;
        (core.pid.is_some() && run.app_sha256 != install.app_sha256).then_some(install.app)
    }

    /// Run a launch: resolve the install (this herdr's app files first),
    /// spawn, record, watch. On the calling thread. A stop that came
    /// meanwhile calls it off before the spawn, or runs here right after it.
    fn companion_launch(&self, ticket: SpawnTicket) {
        let home = self.companion_home();
        let generation = ticket.generation;
        let fail = |hub: &Self, error: String| {
            tracing::warn!(event = "browser.companion.launch", %error, "companion launch failed");
            hub.launcher()
                .lock()
                .spawn_failed(generation, error, Instant::now(), unix_now());
            hub.companion_changed();
            hub.ensure_companion_driver();
        };
        // the directory a companion still runs from is kept
        if let Some(Err(err)) =
            companion_install::refresh_app_files(&home, running_app_dir(&home).as_deref())
        {
            tracing::warn!(event = "browser.companion.refresh", error = %err, "companion app files not refreshed");
        }
        let install = match companion_install::resolve(&home) {
            Ok(install) => install,
            Err(problem) => return fail(self, install_error(problem, !ticket.background).message),
        };
        let socket = self.launcher().socket();
        let mut args = vec![
            "--socket".to_string(),
            socket.display().to_string(),
            "--profile-dir".to_string(),
            companion_install::profile_dir(&home).display().to_string(),
            "--managed".to_string(),
            "--launch-token".to_string(),
            ticket.token.clone(),
        ];
        if ticket.background {
            args.push("--background".into());
        }
        let spec = LaunchSpec {
            executable: install.executable.clone(),
            app_dir: install.app_dir.clone(),
            args,
            log: companion_install::log_path(&home),
        };
        // a stop (or the server's exit) that came while this got ready: no process
        if !self
            .launcher()
            .lock()
            .launch_goes_ahead(generation, unix_now())
        {
            tracing::info!(
                event = "browser.companion.launch",
                "companion launch called off by a stop"
            );
            self.companion_changed();
            return;
        }
        let spawner = self.launcher().spawner();
        let spawned = match spawner.spawn(&spec) {
            Ok(spawned) => spawned,
            Err(err) => {
                return fail(
                    self,
                    format!("could not start {}: {err}", spec.executable.display()),
                )
            }
        };
        let pid = spawned.pid;
        let record = CompanionRunRecord {
            pid,
            launch_token: ticket.token.clone(),
            app: install.record.app.clone(),
            app_sha256: install.record.app_sha256.clone(),
            app_dir: install.app_dir.display().to_string(),
            electron: install.record.electron.clone(),
            bundle: install.record.bundle.clone(),
            started_at: unix_now(),
            start_token: spawner
                .facts(pid)
                .and_then(|facts| facts.start_token)
                .unwrap_or(0),
            server_pid: std::process::id(),
            socket: socket.display().to_string(),
            background: ticket.background,
        };
        if let Err(err) = write_run_record(&home, &record) {
            tracing::warn!(event = "browser.companion.run_record", error = %err, "companion run record not written; a live handoff cannot adopt it");
        }
        let has_child = spawned.child.is_some();
        let (current, pending) = {
            let mut core = self.launcher().lock();
            let current = core.spawned(generation, pid, record, has_child, unix_now());
            (current, current.then(|| core.stop_pending.take()).flatten())
        };
        tracing::info!(
            event = "browser.companion.spawn",
            pid,
            background = ticket.background,
            current,
            "companion started"
        );
        if let Some(mut child) = spawned.child {
            let hub = self.clone();
            let _ = std::thread::Builder::new()
                .name("herdr-companion-wait".into())
                .spawn(move || {
                    let exit = child
                        .wait()
                        .map(ExitInfo::from_status)
                        .unwrap_or_else(|_| ExitInfo::unknown());
                    hub.companion_exited(generation, exit);
                });
        }
        self.companion_changed();
        self.ensure_companion_driver();
        // a stop came while it was being spawned: it runs now, on its deadline
        if let Some(stop) = pending {
            self.companion_stop_process(pid, generation, has_child, stop.ladder, stop.deadline);
        }
    }

    /// The process of `generation` is gone. Its run record goes before the
    /// core hears of it: whoever then sees the process gone (a stop, a
    /// restart) finds no record left behind.
    fn companion_exited(&self, generation: u64, exit: ExitInfo) {
        let home = self.companion_home();
        let tail = log_tail(&companion_install::log_path(&home), 3);
        let live_token = {
            let core = self.launcher().lock();
            (core.generation == generation && (core.pid.is_some() || core.spawning))
                .then(|| core.token.clone())
                .flatten()
        };
        // the process is current until `exited` (no launch can begin before
        // it): the record on disk is still this token's
        if let Some(token) = live_token.as_deref() {
            clear_run_record(&home, Some(token));
        }
        let (current, token, phase) = {
            let mut core = self.launcher().lock();
            let token = core.token.clone();
            let current = core.exited(generation, &exit, &tail, Instant::now(), unix_now());
            (current, token, core.phase)
        };
        if current {
            if let Some(token) = token.as_deref() {
                self.drop_companion_lease_for(
                    token,
                    matches!(phase, Phase::Crashed | Phase::Failed),
                );
            }
            tracing::info!(event = "browser.companion.exit", status = %exit.describe(), phase = phase.as_str(), "companion process exited");
        }
        self.companion_changed();
    }

    /// The stop, within `ladder.total()` from now (one deadline for every
    /// step). A launch in flight stops what it spawns on that deadline, or
    /// spawns nothing; this waits for it.
    fn companion_stop_blocking(&self, ladder: Ladder) {
        let launcher = self.launcher();
        let deadline = Instant::now() + ladder.total();
        let target = launcher.lock().begin_stop(ladder, deadline, unix_now());
        self.companion_changed();
        match target {
            StopTarget::Process {
                pid,
                generation,
                has_child,
            } => self.companion_stop_process(pid, generation, has_child, ladder, deadline),
            StopTarget::Launch { generation } => {
                let until = deadline + KILL_FLOOR + Duration::from_millis(100);
                let mut core = launcher.lock();
                while core.generation == generation && (core.spawning || core.pid.is_some()) {
                    let now = Instant::now();
                    if now >= until {
                        break;
                    }
                    core = launcher.wait(core, (until - now).min(Duration::from_millis(100)));
                }
            }
            StopTarget::Nothing => {}
        }
    }

    /// Stop the process of `generation`: the control page's quit when it
    /// holds the live lease (sent, never waited for: its exit is the
    /// answer), else a short grace; then SIGTERM, then SIGKILL. Every wait
    /// ends at `deadline` too (SIGKILL's gets at least [`KILL_FLOOR`]).
    fn companion_stop_process(
        &self,
        pid: u32,
        generation: u64,
        has_child: bool,
        ladder: Ladder,
        deadline: Instant,
    ) {
        let launcher = self.launcher();
        let left = |step: Duration| step.min(deadline.saturating_duration_since(Instant::now()));
        let gone = |within: Duration| self.companion_wait_gone(generation, pid, has_child, within);
        let token = {
            let core = launcher.lock();
            (core.generation == generation)
                .then(|| core.token.clone())
                .flatten()
        };
        let facts = self.companion_lease_facts();
        let attached = facts.live && token.is_some() && facts.launch_token == token;
        if attached {
            let hub = self.clone();
            let wait = ladder.quit;
            let _ = std::thread::Builder::new()
                .name("herdr-companion-quit".into())
                .spawn(move || {
                    if let Err(err) = hub.request(
                        "control_quit",
                        Some(COMPANION_PROFILE),
                        None,
                        serde_json::Value::Null,
                        wait,
                    ) {
                        tracing::debug!(event = "browser.companion.quit", code = %err.code, "companion control_quit failed");
                    }
                });
        }
        if gone(left(if attached {
            ladder.quit
        } else {
            ladder.unattached
        })) {
            return;
        }
        if !self.companion_signal(pid, generation, has_child, StopSignal::Term)
            || gone(left(ladder.term))
        {
            return;
        }
        if !self.companion_signal(pid, generation, has_child, StopSignal::Kill)
            || gone(left(ladder.kill).max(KILL_FLOOR))
        {
            return;
        }
        tracing::warn!(
            event = "browser.companion.stop",
            pid,
            "companion did not exit after SIGKILL"
        );
        launcher.lock().abandon(
            generation,
            format!("pid {pid} did not exit after SIGKILL"),
            unix_now(),
        );
        // a process SIGKILL cannot end is nothing a next server should adopt
        clear_run_record(&self.companion_home(), token.as_deref());
        if let Some(token) = token.as_deref() {
            self.drop_companion_lease_for(token, false);
        }
        self.companion_changed();
    }

    /// Signal the process of `generation`; one herdr has no child of
    /// (adopted) only while its pid is still the process its run record
    /// names. `false`: it is gone (its exit is reported here).
    fn companion_signal(
        &self,
        pid: u32,
        generation: u64,
        has_child: bool,
        signal: StopSignal,
    ) -> bool {
        if !has_child {
            match self.companion_adopted_process(generation, pid) {
                RecordedProcess::Same => {}
                RecordedProcess::Gone => {
                    self.companion_exited(generation, ExitInfo::unknown());
                    return false;
                }
                RecordedProcess::Unknown => {
                    tracing::warn!(
                        event = "browser.companion.signal",
                        pid,
                        ?signal,
                        "the companion's pid cannot be told from a reused one; not signalled"
                    );
                    return true;
                }
            }
        }
        self.launcher().spawner().signal(pid, signal);
        true
    }

    /// Wait until the process of `generation` is gone (its waiter reported,
    /// or — adopted — its pid went away or is another process now), at most
    /// `within`.
    fn companion_wait_gone(
        &self,
        generation: u64,
        pid: u32,
        has_child: bool,
        within: Duration,
    ) -> bool {
        let launcher = self.launcher();
        let deadline = Instant::now() + within;
        let gone = |core: &Core| core.generation != generation || core.pid.is_none();
        loop {
            if gone(&launcher.lock()) {
                return true;
            }
            // an adopted process has no waiter: its recorded pid is the fact
            if !has_child
                && self.companion_adopted_process(generation, pid) == RecordedProcess::Gone
            {
                self.companion_exited(generation, ExitInfo::unknown());
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let core = launcher.lock();
            if gone(&core) {
                return true;
            }
            drop(launcher.wait(core, (deadline - now).min(Duration::from_millis(100))));
        }
    }

    /// A new server (live handoff, restart) takes over the companion the
    /// last one started: its pid lives, is still the process its run record
    /// names (the start token) and was told this server's socket. It
    /// re-registers with the same token.
    pub fn adopt_companion(&self) {
        let home = self.companion_home();
        let Some(record) = read_run_record(&home) else {
            return;
        };
        match self.record_process(&record) {
            // its process is gone, or its pid is another process now
            RecordedProcess::Gone => {
                clear_run_record(&home, Some(&record.launch_token));
                return;
            }
            // nothing tells it from a reused pid: left alone
            RecordedProcess::Unknown => return,
            RecordedProcess::Same => {}
        }
        let launcher = self.launcher();
        if record.socket != launcher.socket().display().to_string() {
            // another session's: never touched (routing names it)
            return;
        }
        let token = record.launch_token.clone();
        let pid = record.pid;
        {
            let mut core = launcher.lock();
            if core.busy() {
                return;
            }
            core.adopt(record, Instant::now(), unix_now());
        }
        // it may have re-registered before this ran
        let facts = self.companion_lease_facts();
        if facts.live && facts.launch_token.as_deref() == Some(token.as_str()) {
            launcher.lock().registered(Some(&token), unix_now());
        }
        tracing::info!(
            event = "browser.companion.adopt",
            pid,
            "adopted the running companion"
        );
        self.companion_changed();
        self.ensure_companion_driver();
    }

    /// `[browser] companion_autostart`: start it with the server (in the
    /// background) when it is installed and nothing was adopted.
    pub fn companion_autostart(&self) {
        let config = self.config();
        if !config.companion_autostart
            || companion_off_by_config(&config, false).is_some()
            || self.launcher().lock().busy()
        {
            return;
        }
        if companion_install::resolve(&self.companion_home()).is_err() {
            return;
        }
        if let Err(err) = self.companion_start(StartReason::Autostart) {
            tracing::warn!(event = "browser.companion.autostart", code = %err.code, message = %err.message, "companion autostart failed");
        }
    }

    /// Server exit (never a live handoff): nothing launches from now on (a
    /// scheduled crash restart goes too) and the managed companion quits
    /// with the server, within [`SERVER_EXIT_BUDGET`]. A companion started
    /// by hand stays.
    pub fn stop_companion_with_server(&self) {
        self.launcher().lock().shutting_down = true;
        self.companion_stop_blocking(SERVER_LADDER);
    }

    /// The managed companion, as the API shows it.
    pub fn companion_app_info(&self) -> BrowserCompanionAppInfo {
        let home = self.companion_home();
        let facts = self.companion_lease_facts();
        let launcher = self.launcher();
        let install = launcher.install_record(&home);
        // cheap on every `browser.get`: the cached record and one stat
        let installed = install.as_ref().is_some_and(|record| {
            !record.bundle.is_empty()
                && companion_install::bundle_executable(Path::new(&record.bundle)).is_file()
        });
        let config = self.config();
        let core = launcher.lock().clone();
        let managed = facts.live
            && facts.launch_token.is_some()
            && facts.launch_token.as_deref() == core.token.as_deref();
        let external = facts.live && !managed;
        let state = if external {
            "external".to_string()
        } else if !core.busy() && !facts.live && companion_off_by_config(&config, true).is_some() {
            // nothing of it runs and nothing will start it: one still
            // running after a config change keeps its state (and its Stop)
            DISABLED_STATE.to_string()
        } else if core.phase == Phase::Stopped && !installed && !core.busy() {
            "not_installed".to_string()
        } else {
            core.phase.as_str().to_string()
        };
        let update_ready = !external
            && core.pid.is_some()
            && install
                .as_ref()
                .zip(core.run.as_ref())
                .is_some_and(|(install, run)| install.app_sha256 != run.app_sha256);
        BrowserCompanionAppInfo {
            state,
            managed,
            pid: if external { facts.pid } else { core.pid },
            version: if facts.live && !facts.version.is_empty() {
                Some(facts.version.clone())
            } else {
                core.run
                    .as_ref()
                    .map(|run| run.app.clone())
                    .filter(|v| !v.is_empty())
            },
            installed: install.as_ref().map(|i| i.app.clone()),
            electron: install.as_ref().map(|i| i.electron.clone()),
            update_ready,
            started_unix: if external { None } else { core.started_unix },
            since_unix: Some(core.since_unix),
            restarts: core.crashes_in_window(Instant::now()),
            next_retry_unix: core
                .next_retry_unix
                .filter(|_| core.phase == Phase::Crashed),
            last_error: core.last_error.clone(),
            autostart: config.companion_autostart,
            log: Some(companion_install::log_path(&home).display().to_string()),
        }
    }

    /// The driver: crash restarts, the registration deadline, the lease
    /// rule, adopted pids. One thread per hub, started with the first
    /// launch or adoption; idle it sleeps until a transition wakes it.
    fn ensure_companion_driver(&self) {
        if self.launcher().driver.swap(true, Ordering::AcqRel) {
            return;
        }
        let hub = self.clone();
        if std::thread::Builder::new()
            .name("herdr-companion-launcher".into())
            .spawn(move || loop {
                hub.companion_drive_once();
            })
            .is_err()
        {
            self.launcher().driver.store(false, Ordering::Release);
        }
    }

    fn companion_drive_once(&self) {
        let launcher = self.launcher();
        let lease_live = self.companion_live();
        let actions = launcher.lock().due(Instant::now(), lease_live);
        for action in actions {
            match action {
                Action::Respawn => {
                    // the config turned the companion off since the crash:
                    // the scheduled restart goes (a launched one could
                    // never register)
                    if companion_off_by_config(&self.config(), false).is_some() {
                        launcher.lock().reset_backoff(unix_now());
                        self.companion_changed();
                        continue;
                    }
                    let ticket = launcher.lock().begin_spawn(
                        StartReason::CrashRestart,
                        Instant::now(),
                        unix_now(),
                    );
                    if let Some(ticket) = ticket {
                        self.companion_changed();
                        self.companion_launch(ticket);
                    }
                }
                Action::Signal {
                    pid,
                    generation,
                    signal,
                } => {
                    let (current, has_child) = {
                        let core = launcher.lock();
                        (
                            core.generation == generation && core.pid == Some(pid),
                            core.has_child,
                        )
                    };
                    if current {
                        tracing::warn!(
                            event = "browser.companion.kill",
                            pid,
                            ?signal,
                            "stopping an unhealthy companion"
                        );
                        self.companion_signal(pid, generation, has_child, signal);
                    }
                }
                Action::Poll { pid, generation } => {
                    if self.companion_adopted_process(generation, pid) == RecordedProcess::Gone {
                        self.companion_exited(generation, ExitInfo::unknown());
                    }
                }
            }
        }
        let core = launcher.lock();
        let wait = core.next_wake(Instant::now());
        drop(launcher.wait(core, wait));
    }
}

// The fake spawner and hub helpers are shared with the server's smoke tests.
#[cfg(all(test, unix))]
pub(crate) mod tests;
