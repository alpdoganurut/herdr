//! The browser hub: one per server process. Attach-or-launch per profile
//! (under a per-profile mutex), the sidecar link (spawn, `hello`, ping,
//! kill, respawn), request/reply correlation, the ledger, and the operations
//! `browser.run` executes.
//!
//! Lock order: `host` and `pending` are held only for a line write or a map
//! touch; `state` is never held across a sidecar request.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::host::{self, HostEvent, HostMessage, HostReply, HostRequest, HostWriter, PageInfo};
use super::launch::{self, AttachDecision, Executable, LaunchOptions, RunRecord};
use super::profiles::{self, ProfileStore};
use super::setup::{self, SetupEnv};
use super::shape;
use super::state::{BrowserState, HostStatus, HostTab, ProfileStatus, TabKey};
use super::{unix_now, BrowserError};
use crate::api::schema::{
    BrowserActivity, BrowserActor, BrowserCheckInfo, BrowserFixResult, BrowserGetInfo,
    BrowserLogParams, BrowserOp, BrowserProfileRecord, BrowserRunParams, BrowserRunResult,
    BrowserSettingsInfo, BrowserStatusInfo,
};
use crate::config::{valid_profile_name, BrowserConfig};
use crate::integration::browser_assets;

/// Grace the hub adds to the sidecar's own deadline before giving up on a reply.
pub const REPLY_GRACE: Duration = Duration::from_secs(5);
/// Deadline of the attach handshake: a page dialog can block it, and the
/// first attach to a freshly launched browser restoring its session has
/// been seen to take seconds while the restored tabs come up.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
/// Deadline of `hello` and pings.
pub const PING_TIMEOUT: Duration = Duration::from_secs(2);
pub const SUPERVISOR_INTERVAL: Duration = Duration::from_secs(5);
/// Pings missed in a row before the sidecar counts as wedged.
pub const PING_MISSES: u32 = 2;
/// Respawns within [`RESPAWN_WINDOW`] before the host is marked failed.
pub const RESPAWN_LIMIT: usize = 5;
pub const RESPAWN_WINDOW: Duration = Duration::from_secs(60);
/// How long `stop` waits for a graceful exit before SIGTERM.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

struct HostLink {
    generation: u64,
    pid: u32,
    child: Option<std::process::Child>,
    writer: Box<dyn HostWriter>,
    playwright: String,
    attached: HashSet<String>,
    missed_pings: u32,
}

/// The checks and fixes behind `browser.settings` / `browser.fix`.
#[derive(Debug, Default)]
struct SetupState {
    checks: Vec<BrowserCheckInfo>,
    checked_at: Option<Instant>,
    checked_unix: Option<u64>,
    checking: bool,
    fixing: bool,
    fixes: Vec<BrowserFixResult>,
    /// Fix requests that arrived while one ran: merged ids (`Some(empty)` =
    /// every failing fixable check), drained by the worker before it ends.
    pending: Option<Vec<String>>,
    /// Bumped by every fix batch: a check refresh started before it is
    /// discarded instead of overwriting the fresh results.
    generation: u64,
}

/// Checks older than this are refreshed by the next `browser.settings`.
const CHECKS_STALE_AFTER: Duration = Duration::from_secs(20);

struct Inner {
    config: RwLock<BrowserConfig>,
    home: PathBuf,
    profiles: ProfileStore,
    ledger_path: Mutex<Option<PathBuf>>,
    activity_path: Mutex<Option<PathBuf>>,
    flushed_seq: AtomicU64,
    state: Mutex<BrowserState>,
    host: Mutex<Option<HostLink>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<HostReply>>>,
    next_id: AtomicU64,
    generation: AtomicU64,
    profile_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    host_lock: Mutex<()>,
    respawns: Mutex<VecDeque<Instant>>,
    supervisor: AtomicBool,
    /// The safe fixes after a herdr update ran (once per process, on the first start).
    auto_repaired: std::sync::Once,
    /// Skip the executable check and the `hello` node checks (fake host tests).
    test_mode: AtomicBool,
    /// Serializes the whole persistence transaction (snapshot, save, append,
    /// bookkeeping).
    flush_lock: Mutex<()>,
    /// The last extraction per tab, so paging and `find` are stable and cheap.
    read_cache: Mutex<HashMap<TabKey, ReadCache>>,
    /// The companion extension's state per attached profile (`ready`,
    /// `missing`, `off`, …) for status and doctor.
    companion: Mutex<HashMap<String, String>>,
    /// Panes whose tab groups were released (gone panes), so a poll does not
    /// ask again.
    released: Mutex<HashSet<String>>,
    /// Panes whose release failed: attempts so far and when to try again.
    release_backoff: Mutex<HashMap<String, (u32, Instant)>>,
    /// Whether the last new-tab-page / dashboard push failed (a warning is
    /// logged when that flips, not on every push).
    push_failing: Mutex<HashMap<&'static str, bool>>,
    /// The new tab page push: when the last one went out, whether one is armed.
    ntp_push: Mutex<(Option<Instant>, bool)>,
    /// The doctor's checks (cached; refreshed off the caller's thread) and
    /// the last fixes — the settings overlay's browser section.
    setup: Mutex<SetupState>,
    /// Tests: where the checks look and the fixes write (never the real
    /// home directory).
    setup_env_override: Mutex<Option<SetupEnv>>,
}

/// The new tab page's snapshot goes out at most this often.
const NTP_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// A failed release is retried this many times, each wait one step longer.
const RELEASE_MAX_ATTEMPTS: u32 = 5;
const RELEASE_BACKOFF: Duration = Duration::from_secs(30);

thread_local! {
    /// The activity directive of the call running on this thread (set by
    /// `run`, `animate` cleared by a batch that asks), attached to every
    /// sidecar request the call makes.
    static ACTIVITY: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// One cached extraction: what was asked and the page it came from.
struct ReadCache {
    format: &'static str,
    scope: String,
    interactive: bool,
    url: String,
    content: String,
    page: PageInfo,
}

#[derive(Clone)]
pub struct BrowserHub {
    inner: Arc<Inner>,
}

impl BrowserHub {
    pub fn new(config: BrowserConfig) -> Self {
        Self::with_home(config, super::browser_home())
    }

    pub fn with_home(config: BrowserConfig, home: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                config: RwLock::new(config),
                profiles: ProfileStore::new(&home),
                home,
                ledger_path: Mutex::new(None),
                activity_path: Mutex::new(None),
                flushed_seq: AtomicU64::new(0),
                state: Mutex::new(BrowserState::new()),
                host: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                generation: AtomicU64::new(0),
                profile_locks: Mutex::new(HashMap::new()),
                host_lock: Mutex::new(()),
                respawns: Mutex::new(VecDeque::new()),
                supervisor: AtomicBool::new(false),
                auto_repaired: std::sync::Once::new(),
                test_mode: AtomicBool::new(false),
                flush_lock: Mutex::new(()),
                read_cache: Mutex::new(HashMap::new()),
                companion: Mutex::new(HashMap::new()),
                released: Mutex::new(HashSet::new()),
                release_backoff: Mutex::new(HashMap::new()),
                push_failing: Mutex::new(HashMap::new()),
                ntp_push: Mutex::new((None, false)),
                setup: Mutex::new(SetupState::default()),
                setup_env_override: Mutex::new(None),
            }),
        }
    }

    pub fn config(&self) -> BrowserConfig {
        self.inner.config.read().unwrap().clone()
    }

    pub fn apply_config(&self, config: &BrowserConfig) {
        let before = self.config();
        *self.inner.config.write().unwrap() = config.clone();
        // The pinned dashboard follows the config live: the companion adds or
        // removes it in every window of every attached profile.
        let pin_before = before.show_activity && before.pin_dashboard;
        let pin_now = config.show_activity && config.pin_dashboard;
        if pin_before != pin_now {
            self.push_dashboard(pin_now);
        }
    }

    /// Tell the sidecar (every attached profile) whether the dashboard is pinned; off the caller's thread.
    fn push_dashboard(&self, pin: bool) {
        if self.inner.host.lock().unwrap().is_none() {
            return;
        }
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-dashboard".into())
            .spawn(move || {
                let outcome = hub.request(
                    "dashboard",
                    None,
                    None,
                    json!({ "pin": pin }),
                    Duration::from_secs(5),
                );
                hub.note_push("dashboard", outcome.map(|_| ()));
            });
    }

    /// Persist the ledger next to `session.json` in `data_dir` and load what
    /// is there.
    pub fn enable_persistence(&self, data_dir: &Path) {
        let ledger = data_dir.join(crate::persist::browser::LEDGER_FILE);
        let activity = data_dir.join(crate::persist::browser::ACTIVITY_FILE);
        if let Some(snapshot) = crate::persist::browser::load(&ledger) {
            let mut state = self.inner.state.lock().unwrap();
            state.restore(snapshot, unix_now());
            self.inner.flushed_seq.store(
                state.log.back().map(|e| e.seq).unwrap_or(0),
                Ordering::Relaxed,
            );
        }
        *self.inner.ledger_path.lock().unwrap() = Some(ledger);
        *self.inner.activity_path.lock().unwrap() = Some(activity);
    }

    /// Tests: a sidecar on the far end of a pipe, no child process.
    #[cfg(test)]
    pub fn install_fake_host(
        &self,
        writer: Box<dyn HostWriter>,
        reader: Box<dyn std::io::BufRead + Send>,
    ) -> u64 {
        self.inner.test_mode.store(true, Ordering::Relaxed);
        let generation = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        *self.inner.host.lock().unwrap() = Some(HostLink {
            generation,
            pid: 0,
            child: None,
            writer,
            playwright: "fake".into(),
            attached: HashSet::new(),
            missed_pings: 0,
        });
        self.inner
            .state
            .lock()
            .unwrap()
            .set_host(HostStatus::Running {
                pid: 0,
                playwright: "fake".into(),
                node: "fake".into(),
            });
        self.spawn_reader(generation, reader);
        generation
    }

    // ----- App lane -------------------------------------------------------

    pub fn get(&self, since_seq: Option<u64>) -> BrowserGetInfo {
        let config = self.config();
        // One profiles.json read before the lock, not one per profile under it.
        let temporary_names: HashSet<String> = self
            .inner
            .profiles
            .list()
            .into_iter()
            .filter(|entry| entry.temporary)
            .map(|entry| entry.name)
            .collect();
        let temporary = |name: &str| temporary_names.contains(name);
        let state = self.inner.state.lock().unwrap();
        let mut info = state.get_info(
            since_seq,
            unix_now(),
            config.active_seconds,
            config.enabled,
            &temporary,
        );
        drop(state);
        let companion = self.inner.companion.lock().unwrap();
        for profile in &mut info.profiles {
            profile.companion = companion.get(&profile.name).cloned();
        }
        drop(companion);
        info.setup_needed = self.setup_needed();
        info
    }

    /// Every pane id the ledger knows (open tabs' actors and cursors), for
    /// the App to check against its panes.
    pub fn known_pane_ids(&self) -> Vec<String> {
        let state = self.inner.state.lock().unwrap();
        let mut ids: Vec<String> = state
            .tabs
            .values()
            .filter(|record| record.is_open())
            .flat_map(|record| [&record.opened_by, &record.last_actor])
            .filter_map(|actor| actor.pane_id().map(str::to_string))
            .chain(state.cursors.keys().cloned())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Panes that are gone: their tab groups are dissolved — once per pane
    /// when it works; a failed release (no companion, an error) is retried
    /// with a growing wait, at most RELEASE_MAX_ATTEMPTS times; a pane id
    /// seen again in a call is armed again. Never blocks the caller.
    pub fn release_panes(&self, panes: Vec<String>) {
        // No sidecar, nothing to tidy yet: asked again once it runs.
        if panes.is_empty() || self.inner.host.lock().unwrap().is_none() {
            return;
        }
        let fresh: Vec<String> = {
            let mut released = self.inner.released.lock().unwrap();
            let backoff = self.inner.release_backoff.lock().unwrap();
            let now = Instant::now();
            panes
                .into_iter()
                .filter(|pane| {
                    if released.contains(pane) {
                        return false;
                    }
                    match backoff.get(pane) {
                        Some((attempts, _)) if *attempts >= RELEASE_MAX_ATTEMPTS => false,
                        Some((_, next)) if *next > now => false,
                        _ => {
                            released.insert(pane.clone());
                            true
                        }
                    }
                })
                .collect()
        };
        if fresh.is_empty() {
            return;
        }
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-release".into())
            .spawn(move || {
                let outcome = hub.request(
                    "release",
                    None,
                    None,
                    json!({ "keys": fresh }),
                    Duration::from_secs(5),
                );
                let (failed, reason): (Vec<String>, String) = match &outcome {
                    Ok(reply) => (
                        reply.result["failed"]
                            .as_array()
                            .map(|keys| {
                                keys.iter()
                                    .filter_map(|k| k.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        reply.result["reason"]
                            .as_str()
                            .unwrap_or("the sidecar could not release them")
                            .to_string(),
                    ),
                    Err(err) => (fresh.clone(), err.message.clone()),
                };
                let mut released = hub.inner.released.lock().unwrap();
                let mut backoff = hub.inner.release_backoff.lock().unwrap();
                for key in &fresh {
                    if !failed.contains(key) {
                        backoff.remove(key);
                    }
                }
                if !failed.is_empty() {
                    tracing::warn!(event = "browser.release.failed", keys = ?failed, reason = %reason, "tab groups of gone panes not released; retrying later");
                    for key in failed {
                        released.remove(&key);
                        let attempts = backoff.get(&key).map(|(a, _)| *a).unwrap_or(0) + 1;
                        backoff.insert(key, (attempts, Instant::now() + RELEASE_BACKOFF * attempts));
                    }
                }
            });
    }

    /// The new tab page's snapshot: pushed to the companion after the ledger
    /// changed, debounced to one per NTP_MIN_INTERVAL, off the caller's
    /// thread. Nothing when no sidecar runs.
    pub fn schedule_ntp_push(&self) {
        if self.inner.host.lock().unwrap().is_none() {
            return;
        }
        let wait = {
            let mut push = self.inner.ntp_push.lock().unwrap();
            if push.1 {
                return;
            }
            push.1 = true;
            push.0
                .map(|last| NTP_MIN_INTERVAL.saturating_sub(last.elapsed()))
                .unwrap_or(Duration::ZERO)
        };
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-ntp".into())
            .spawn(move || {
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                {
                    let mut push = hub.inner.ntp_push.lock().unwrap();
                    push.0 = Some(Instant::now());
                    push.1 = false;
                }
                let config = hub.config();
                let snapshot = super::ntp::snapshot(&hub.get(None), &config, unix_now());
                let outcome = hub.request(
                    "ntp",
                    None,
                    None,
                    json!({ "snapshot": snapshot }),
                    Duration::from_secs(3),
                );
                hub.note_push("ntp", outcome.map(|_| ()));
            });
    }

    /// A failed companion push is logged once when it starts failing (and
    /// once more when it recovers), never per push.
    fn note_push(&self, what: &'static str, outcome: Result<(), BrowserError>) {
        let mut failing = self.inner.push_failing.lock().unwrap();
        let was = failing.get(what).copied().unwrap_or(false);
        match outcome {
            Ok(()) => {
                if was {
                    tracing::info!(
                        event = "browser.push.recovered",
                        what,
                        "companion push works again"
                    );
                }
                failing.insert(what, false);
            }
            Err(err) => {
                if !was {
                    let event = if what == "ntp" {
                        "browser.ntp.push"
                    } else {
                        "browser.dashboard.push"
                    };
                    tracing::warn!(event = event, code = %err.code, message = %err.message, "companion push failed; further failures are not repeated");
                }
                failing.insert(what, true);
            }
        }
    }

    #[cfg(test)]
    pub fn release_attempts_for_test(&self, pane: &str) -> Option<u32> {
        self.inner
            .release_backoff
            .lock()
            .unwrap()
            .get(pane)
            .map(|(attempts, _)| *attempts)
    }

    /// The first browser start after a herdr update: the safe fixes (assets,
    /// `npm ci` when the lock changed, runtime.json) run by themselves on
    /// the start thread — never a user file, never on the App thread (a
    /// `stop` can reach `ensure_host` from there), once per process.
    fn auto_repair_once(&self) {
        if self.inner.test_mode.load(Ordering::Relaxed) {
            return;
        }
        // Once: concurrent first uses (an agent's run and a start) wait for
        // the one that runs it.
        self.inner.auto_repaired.call_once(|| {
            match setup::auto_repair(&self.config(), &self.setup_env()) {
                Some(Ok(detail)) => {
                    tracing::info!(event = "browser.auto_repair", %detail, "browser helper refreshed after a herdr update")
                }
                Some(Err(error)) => {
                    tracing::warn!(event = "browser.auto_repair", %error, "browser helper could not be refreshed; `herdr browser setup`")
                }
                None => {}
            }
        });
    }

    /// The process environment with this hub's home (tests run on a temporary one).
    fn setup_env(&self) -> SetupEnv {
        if let Some(env) = self.inner.setup_env_override.lock().unwrap().clone() {
            return env;
        }
        let mut env = SetupEnv::from_process();
        env.browser_home = self.inner.home.clone();
        env
    }

    /// Tests: point the checks and fixes at temporary files.
    #[cfg(test)]
    pub fn set_setup_env(&self, env: Option<SetupEnv>) {
        *self.inner.setup_env_override.lock().unwrap() = env;
        let mut setup = self.inner.setup.lock().unwrap();
        setup.checks.clear();
        setup.checked_at = None;
        setup.checked_unix = None;
        setup.fixes.clear();
        setup.pending = None;
    }

    /// Tests: wait for a check refresh or a fix in flight.
    #[cfg(test)]
    pub fn wait_for_setup_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let setup = self.inner.setup.lock().unwrap();
                if !setup.checking && !setup.fixing && setup.pending.is_none() {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A check that only an explicit request may fix is failing (the
    /// Browser row's `!` hint; `browser.get`'s `setup_needed`).
    pub fn setup_needed(&self) -> bool {
        setup::setup_needed(&self.inner.setup.lock().unwrap().checks)
    }

    /// `browser.settings`: the `[browser]` keys the section edits, the
    /// default profile's state, the cached checks and the last fixes.
    pub fn setup_info(&self) -> BrowserSettingsInfo {
        let config = self.config();
        let info = self.get(None);
        let profile = config.default_profile().to_string();
        let running_profile = info
            .profiles
            .iter()
            .find(|p| p.name == profile && (p.state == "running" || p.state == "starting"));
        let status = match running_profile {
            Some(p) if p.state == "starting" => format!("starting · profile {profile}"),
            Some(p) => format!(
                "running · {} tab{} · {} agent{} · profile {profile}",
                p.tabs,
                if p.tabs == 1 { "" } else { "s" },
                p.agents,
                if p.agents == 1 { "" } else { "s" }
            ),
            None if !config.enabled => "off".to_string(),
            None => format!("stopped · profile {profile}"),
        };
        let setup = self.inner.setup.lock().unwrap();
        BrowserSettingsInfo {
            enabled: config.enabled,
            show_activity: config.show_activity,
            pin_dashboard: config.pin_dashboard,
            activity_color: config.activity_color().to_string(),
            steer_agents: config.steer_agents,
            wrap_agents: config.wrap_agents,
            disable_native_browser: config.disable_native_browser,
            mcp_agents: config.mcp_agents.clone(),
            shell_hook: config.shell_hook,
            profile,
            running: running_profile.is_some(),
            status,
            checks: setup.checks.clone(),
            checked_at: setup.checked_unix,
            checking: setup.checking,
            fixing: setup.fixing,
            fixes: setup.fixes.clone(),
            host: crate::platform::hostname().unwrap_or_default(),
        }
    }

    /// Refresh the cached checks off the caller's thread (`force`: even
    /// when they are fresh). One refresh at a time; a fix in flight ends
    /// with its own.
    pub fn refresh_checks(&self, force: bool) {
        self.refresh_checks_if_older(if force {
            Duration::ZERO
        } else {
            CHECKS_STALE_AFTER
        });
    }

    /// Refresh the cached checks when they are older than `max_age`, off the
    /// caller's thread. One refresh at a time; a fix in flight ends with its
    /// own; a result that a fix overtook is discarded (generation).
    pub fn refresh_checks_if_older(&self, max_age: Duration) {
        let generation = {
            let mut setup = self.inner.setup.lock().unwrap();
            let fresh = setup.checked_at.is_some_and(|at| at.elapsed() < max_age);
            if setup.checking || setup.fixing || fresh {
                return;
            }
            setup.checking = true;
            setup.generation
        };
        let hub = self.clone();
        if std::thread::Builder::new()
            .name("herdr-browser-checks".into())
            .spawn(move || {
                hub.compute_checks(Some(generation));
                hub.inner.setup.lock().unwrap().checking = false;
            })
            .is_err()
        {
            self.inner.setup.lock().unwrap().checking = false;
        }
    }

    /// Compute the checks and store them — unless `expected` names a
    /// generation a fix has moved past since (its own results are newer).
    fn compute_checks(&self, expected: Option<u64>) -> bool {
        let config = self.config();
        let env = self.setup_env();
        let live = self.get(None);
        let checks = setup::checks(&config, &env, Some(&live));
        self.store_checks(checks, expected)
    }

    fn store_checks(&self, checks: Vec<BrowserCheckInfo>, expected: Option<u64>) -> bool {
        let mut setup = self.inner.setup.lock().unwrap();
        if expected.is_some_and(|generation| generation != setup.generation) {
            return false;
        }
        setup.checks = checks;
        setup.checked_at = Some(Instant::now());
        setup.checked_unix = Some(unix_now());
        true
    }

    /// `browser.fix`: run the fixes for `ids` (empty: every failing fixable
    /// check) off the caller's thread, then refresh the checks. A request
    /// during a running fix is queued (ids merged; empty wins) and drained by
    /// the same worker with a fresh config. The `extension` fix is a
    /// synchronous restart of the default profile.
    pub fn run_fixes(&self, ids: Vec<String>) {
        {
            let mut setup = self.inner.setup.lock().unwrap();
            if setup.fixing {
                // merge: an "all" request (empty) absorbs named ones
                match setup.pending.as_mut() {
                    None => setup.pending = Some(ids),
                    Some(pending) if pending.is_empty() => {}
                    Some(pending) if ids.is_empty() => pending.clear(),
                    Some(pending) => {
                        for id in ids {
                            if !pending.contains(&id) {
                                pending.push(id);
                            }
                        }
                    }
                }
                return;
            }
            setup.fixing = true;
            setup.fixes.clear();
            setup.pending = None;
            setup.generation += 1;
        }
        let hub = self.clone();
        if std::thread::Builder::new()
            .name("herdr-browser-fix".into())
            .spawn(move || {
                let mut ids = ids;
                let mut results: Vec<BrowserFixResult> = Vec::new();
                loop {
                    results.extend(hub.run_fix_batch(&ids));
                    let mut setup = hub.inner.setup.lock().unwrap();
                    match setup.pending.take() {
                        Some(next) => {
                            setup.generation += 1;
                            drop(setup);
                            ids = next;
                        }
                        None => {
                            setup.fixes = results;
                            setup.fixing = false;
                            break;
                        }
                    }
                }
            })
            .is_err()
        {
            let mut setup = self.inner.setup.lock().unwrap();
            setup.fixing = false;
            setup.pending = None;
        }
    }

    /// One batch of fixes with a fresh config; ends with fresh checks.
    fn run_fix_batch(&self, ids: &[String]) -> Vec<BrowserFixResult> {
        self.compute_checks(None);
        let config = self.config();
        let env = self.setup_env();
        let checks = self.inner.setup.lock().unwrap().checks.clone();
        let mut results = setup::fix_all(ids, &checks, &config, &env);
        let restart = if ids.is_empty() {
            checks
                .iter()
                .any(|c| c.id == "extension" && !c.ok && c.fixable)
        } else {
            ids.iter().any(|id| id == "extension")
        };
        if restart {
            let name = config.default_profile().to_string();
            // Synchronous: stop, mark starting, run — `ok` only when the
            // profile is up again.
            self.stop_profile(&name);
            self.inner
                .state
                .lock()
                .unwrap()
                .set_profile(&name, ProfileStatus::Starting { since: unix_now() });
            results.push(match self.ensure_running(&name) {
                Ok(()) => BrowserFixResult {
                    id: "extension".into(),
                    ok: true,
                    detail: "browser restarted; the new extension files load".into(),
                },
                Err(err) => BrowserFixResult {
                    id: "extension".into(),
                    ok: false,
                    detail: err.message,
                },
            });
        }
        if results.iter().any(|r| r.id == "helper" && r.ok) {
            setup::mark_set_up(&env);
        }
        for result in &results {
            tracing::info!(
                event = "browser.fix",
                id = %result.id,
                ok = result.ok,
                detail = %result.detail,
                "browser setup fix"
            );
        }
        self.compute_checks(None);
        results
    }

    /// Tests: what a fix request would do while a fix runs, and whether a
    /// stale check result is dropped.
    #[cfg(test)]
    pub fn setup_test_state(&self) -> (bool, Option<Vec<String>>, u64) {
        let setup = self.inner.setup.lock().unwrap();
        (setup.fixing, setup.pending.clone(), setup.generation)
    }

    #[cfg(test)]
    pub fn setup_test_bump(&self) {
        self.inner.setup.lock().unwrap().generation += 1;
    }

    #[cfg(test)]
    pub fn setup_test_store(&self, checks: Vec<BrowserCheckInfo>, expected: Option<u64>) -> bool {
        self.store_checks(checks, expected)
    }

    pub fn status(&self) -> BrowserStatusInfo {
        let config = self.config();
        let get = self.get(None);
        let home_env = std::env::var_os("HOME").map(PathBuf::from);
        let (executable, executable_error) =
            match launch::resolve_executable(&config.executable, home_env.as_deref()) {
                Ok(exe) => (Some(exe.display()), None),
                Err(err) => (None, Some(err.message)),
            };
        let host_dir = self.host_dir();
        let runtime = browser_assets::read_runtime(&host_dir);
        let runtime_outdated = runtime
            .as_ref()
            .is_some_and(|runtime| runtime.assets_sha256 != browser_assets::assets_sha256());
        let node = super::node::discover_default(
            config.node(),
            runtime.as_ref().map(|r| Path::new(&r.node)),
        )
        .map(|choice| choice.path.display().to_string());
        let log = self.inner.state.lock().unwrap().activity(50, None, None);
        BrowserStatusInfo {
            get,
            home: self.inner.home.display().to_string(),
            ledger: self
                .inner
                .ledger_path
                .lock()
                .unwrap()
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            executable,
            executable_error,
            node,
            runtime,
            runtime_outdated,
            default_profile: config.default_profile().to_string(),
            autostart: config.autostart,
            log,
        }
    }

    pub fn log(&self, params: &BrowserLogParams) -> Vec<BrowserActivity> {
        let limit = params.limit.unwrap_or(50).clamp(1, 500) as usize;
        let config = self.config();
        let state = self.inner.state.lock().unwrap();
        let tab = params.tab.as_deref().map(|tab| {
            state
                .resolve_tab(config.default_profile(), tab)
                .map(|record| record.id())
                .unwrap_or_else(|| tab.to_string())
        });
        state.activity(limit, params.pane_id.as_deref(), tab.as_deref())
    }

    pub fn profiles(&self) -> Vec<BrowserProfileRecord> {
        let state = self.inner.state.lock().unwrap();
        let mut records: Vec<BrowserProfileRecord> = self
            .inner
            .profiles
            .list()
            .into_iter()
            .map(|entry| BrowserProfileRecord {
                state: state.profile(&entry.name).name().into(),
                exists: self.inner.profiles.exists(&entry.name),
                name: entry.name,
                created_at: entry.created_at,
                temporary: entry.temporary,
            })
            .collect();
        let config = self.config();
        if !records.iter().any(|r| r.name == config.default_profile()) {
            records.push(BrowserProfileRecord {
                name: config.default_profile().to_string(),
                created_at: 0,
                temporary: false,
                exists: false,
                state: "stopped".into(),
            });
            records.sort_by(|a, b| a.name.cmp(&b.name));
        }
        records
    }

    pub fn profile_create(
        &self,
        name: &str,
        temporary: bool,
    ) -> Result<BrowserProfileRecord, BrowserError> {
        let name = if profiles::is_new_request(name) {
            self.inner.profiles.temporary_name(unix_now())
        } else {
            name.to_string()
        };
        self.inner.profiles.ensure(
            &name,
            temporary || name.starts_with(profiles::TEMPORARY_PREFIX),
            unix_now(),
        )?;
        Ok(self
            .profiles()
            .into_iter()
            .find(|record| record.name == name)
            .unwrap_or_default())
    }

    pub fn profile_delete(&self, name: &str) -> Result<(), BrowserError> {
        let config = self.config();
        if name == config.default_profile() {
            return Err(BrowserError::new(
                "profile_protected",
                format!("{name:?} is the default profile; change [browser] default_profile first"),
            ));
        }
        // Never a blocking lock here: this runs on the App thread.
        let lock = self.profile_lock(name);
        let Ok(_guard) = lock.try_lock() else {
            return Err(BrowserError::new(
                "profile_busy",
                format!("profile {name:?} is starting or stopping; retry in a moment"),
            ));
        };
        let status = self.inner.state.lock().unwrap().profile(name);
        if matches!(
            status,
            ProfileStatus::Running { .. }
                | ProfileStatus::Starting { .. }
                | ProfileStatus::InUse { .. }
        ) {
            return Err(BrowserError::new(
                "profile_running",
                format!(
                    "profile {name:?} is {}; `herdr browser stop --profile {name}` first",
                    status.name()
                ),
            ));
        }
        let profile_dir = self.inner.profiles.dir(name);
        let live_pid = launch::read_run_record(&self.inner.home, name)
            .map(|record| record.pid)
            .filter(|pid| crate::platform::process_exists(*pid))
            .or_else(|| {
                launch::singleton_lock_pid(&profile_dir)
                    .filter(|pid| crate::platform::process_exists(*pid))
            });
        if let Some(pid) = live_pid {
            return Err(BrowserError::new(
                "profile_running",
                format!("a Chromium (pid {pid}) still holds profile {name:?}; close it first"),
            ));
        }
        self.inner.profiles.delete(name, unix_now())?;
        let mut state = self.inner.state.lock().unwrap();
        state.profiles.remove(name);
        state.close_profile_tabs(name, unix_now());
        Ok(())
    }

    /// Start a profile in the background; answers the current state.
    pub fn start(&self, profile: Option<&str>) -> Result<BrowserGetInfo, BrowserError> {
        let config = self.config();
        if !config.enabled {
            return Err(BrowserError::disabled());
        }
        let name = self.profile_name(profile, None, &config)?;
        {
            let mut state = self.inner.state.lock().unwrap();
            if !state.profile(&name).is_running() {
                state.set_profile(&name, ProfileStatus::Starting { since: unix_now() });
            }
            if matches!(state.host, HostStatus::Failed { .. }) {
                state.set_host(HostStatus::Absent);
                self.inner.respawns.lock().unwrap().clear();
            }
        }
        let hub = self.clone();
        std::thread::Builder::new()
            .name("herdr-browser-start".into())
            .spawn(move || {
                hub.auto_repair_once();
                if let Err(err) = hub.ensure_running(&name) {
                    tracing::warn!(event = "browser.start", profile = %name, code = %err.code, message = %err.message, "browser start failed");
                }
            })
            .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        Ok(self.get(None))
    }

    /// Stop a profile (or every running one) in the background.
    pub fn stop(&self, profile: Option<&str>, all: bool) -> Result<BrowserGetInfo, BrowserError> {
        let config = self.config();
        let names: Vec<String> = if all {
            self.inner
                .state
                .lock()
                .unwrap()
                .profiles
                .iter()
                .filter(|(_, status)| {
                    status.is_running() || matches!(status, ProfileStatus::Starting { .. })
                })
                .map(|(name, _)| name.clone())
                .collect()
        } else {
            vec![self.profile_name(profile, None, &config)?]
        };
        for name in names {
            let hub = self.clone();
            std::thread::Builder::new()
                .name("herdr-browser-stop".into())
                .spawn(move || hub.stop_profile(&name))
                .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        }
        Ok(self.get(None))
    }

    /// Select a tab and raise the window, as the user (overlay / row). The
    /// sidecar call runs on its own thread so the App loop never waits on it.
    pub fn focus(&self, profile: Option<&str>, tab: &str) -> Result<(), BrowserError> {
        let config = self.config();
        if !config.enabled {
            return Err(BrowserError::disabled());
        }
        let default_profile = profile.unwrap_or(config.default_profile()).to_string();
        let record = {
            let state = self.inner.state.lock().unwrap();
            state
                .resolve_tab(&default_profile, tab)
                .cloned()
                .ok_or_else(|| BrowserError::tab_not_found(tab))?
        };
        if !record.is_open() {
            return Err(BrowserError::tab_closed(&record.id()));
        }
        let hub = self.clone();
        std::thread::Builder::new()
            .name("herdr-browser-focus".into())
            .spawn(move || {
                let ok = hub
                    .request(
                        "focus",
                        Some(&record.profile),
                        Some(&record.target_id),
                        Value::Null,
                        Duration::from_secs(3),
                    )
                    .is_ok();
                let mut state = hub.inner.state.lock().unwrap();
                state.touch(
                    &record.profile,
                    Some(&record.key()),
                    &BrowserActor::User,
                    "focus",
                    &record.short,
                    ok,
                    0,
                    unix_now(),
                );
                drop(state);
                hub.flush();
            })
            .map_err(|err| BrowserError::unavailable(err.to_string()))?;
        Ok(())
    }

    // ----- Connection lane ------------------------------------------------

    /// Execute one `browser.run`. Blocking; called on the connection thread.
    pub fn run(
        &self,
        actor: &BrowserActor,
        params: BrowserRunParams,
    ) -> Result<BrowserRunResult, BrowserError> {
        let started = Instant::now();
        let config = self.config();
        if !config.enabled {
            return Err(BrowserError::disabled());
        }
        if matches!(params.op, BrowserOp::Unknown) {
            return Err(BrowserError::new(
                "unknown_op",
                "this server does not know that browser operation; restart the Claude session or update herdr",
            ));
        }
        let pane = actor.pane_id().map(str::to_string);
        // A qualified tab (`work:t3`) names its profile; it must agree with --profile.
        let tab_profile = match params.tab.as_deref().and_then(|tab| tab.split_once(':')) {
            Some((prefix, _)) => {
                if !valid_profile_name(prefix) {
                    return Err(BrowserError::new(
                        "invalid_request",
                        format!(
                            "tab {:?}: {prefix:?} is not a profile name",
                            params.tab.as_deref().unwrap_or("")
                        ),
                    ));
                }
                Some(prefix)
            }
            None => None,
        };
        let explicit_profile = match (
            params
                .profile
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty()),
            tab_profile,
        ) {
            (Some(explicit), Some(prefix)) if explicit != prefix => {
                return Err(BrowserError::new(
                    "invalid_request",
                    format!(
                        "--profile {explicit} disagrees with tab {}",
                        params.tab.as_deref().unwrap_or("")
                    ),
                ));
            }
            (Some(explicit), _) => Some(explicit),
            (None, prefix) => prefix,
        };
        let profile = self.profile_name(explicit_profile, pane.as_deref(), &config)?;
        self.ensure_running(&profile)?;
        let deadline = Duration::from_millis(match &params.op {
            BrowserOp::Wait { timeout_s, .. } => timeout_s.unwrap_or(30).clamp(1, 300) * 1000 + 500,
            _ => params
                .timeout_ms
                .map(BrowserConfig::clamp_op_timeout_ms)
                .unwrap_or(config.op_timeout_ms()),
        });
        let op_name = params.op.name();
        let now = unix_now();
        // The activity directive rides on this call's sidecar requests.
        if let Some(pane) = &pane {
            self.inner.released.lock().unwrap().remove(pane);
            self.inner.release_backoff.lock().unwrap().remove(pane);
        }
        ACTIVITY.with(|slot| *slot.borrow_mut() = super::activity::directive(actor, &config));
        let result = self.execute(
            actor,
            &profile,
            pane.as_deref(),
            &params,
            &config,
            deadline,
            now,
        );
        ACTIVITY.with(|slot| *slot.borrow_mut() = None);
        let ms = started.elapsed().as_millis() as u64;
        self.record_outcome(
            actor,
            &profile,
            pane.as_deref(),
            &params,
            op_name,
            &result,
            ms,
        );
        self.flush();
        self.schedule_ntp_push();
        match result {
            Ok((mut result, _, _)) => {
                result.ms = ms;
                Ok(result)
            }
            Err(err) => Err(err),
        }
    }

    /// The ledger bookkeeping of one operation (single call or batch step):
    /// the touch on its tab, the activity entry, the pane's cursor clock.
    #[allow(clippy::too_many_arguments)]
    fn record_outcome(
        &self,
        actor: &BrowserActor,
        profile: &str,
        pane: Option<&str>,
        params: &BrowserRunParams,
        op_name: &str,
        result: &Result<(BrowserRunResult, Option<TabKey>, String), BrowserError>,
        ms: u64,
    ) {
        let mut state = self.inner.state.lock().unwrap();
        match result {
            Ok((_, key, detail)) => {
                if matches!(params.op, BrowserOp::Tabs { .. }) {
                    return;
                }
                state.touch(
                    profile,
                    key.as_ref(),
                    actor,
                    op_name,
                    detail,
                    true,
                    ms,
                    unix_now(),
                );
                if let Some(pane) = pane {
                    state.touch_cursor(pane, unix_now());
                }
            }
            Err(err) => {
                let key = params
                    .tab
                    .as_deref()
                    .and_then(|tab| state.resolve_tab(profile, tab).map(|r| r.key()))
                    .or_else(|| pane.and_then(|pane| state.cursor(pane).map(|r| r.key())));
                state.touch(
                    profile,
                    key.as_ref(),
                    actor,
                    op_name,
                    &format!("{}: {}", err.code, err.message),
                    false,
                    ms,
                    unix_now(),
                );
            }
        }
    }

    /// Resolve the profile a call uses: explicit (`new` = fresh temporary),
    /// else the pane's cursor profile, else the default.
    fn profile_name(
        &self,
        explicit: Option<&str>,
        pane: Option<&str>,
        config: &BrowserConfig,
    ) -> Result<String, BrowserError> {
        if let Some(name) = explicit.map(str::trim).filter(|n| !n.is_empty()) {
            if profiles::is_new_request(name) {
                let fresh = self.inner.profiles.temporary_name(unix_now());
                self.inner.profiles.ensure(&fresh, true, unix_now())?;
                return Ok(fresh);
            }
            if !valid_profile_name(name) {
                return Err(BrowserError::invalid_profile(name));
            }
            return Ok(name.to_string());
        }
        if let Some(pane) = pane {
            if let Some(profile) = self.inner.state.lock().unwrap().cursor_profile(pane) {
                return Ok(profile.to_string());
            }
        }
        Ok(config.default_profile().to_string())
    }

    fn resolve_target(
        &self,
        profile: &str,
        explicit: Option<&str>,
        pane: Option<&str>,
    ) -> Result<super::state::BrowserTabRecord, BrowserError> {
        let state = self.inner.state.lock().unwrap();
        if let Some(tab) = explicit.map(str::trim).filter(|t| !t.is_empty()) {
            let record = state
                .resolve_tab(profile, tab)
                .ok_or_else(|| BrowserError::tab_not_found(tab))?;
            if !record.is_open() {
                return Err(BrowserError::tab_closed(&record.id()));
            }
            return Ok(record.clone());
        }
        if let Some(pane) = pane {
            if let Some(record) = state.cursor(pane) {
                if record.profile == profile {
                    return Ok(record.clone());
                }
            }
        }
        Err(BrowserError::no_current_tab())
    }

    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        actor: &BrowserActor,
        profile: &str,
        pane: Option<&str>,
        params: &BrowserRunParams,
        config: &BrowserConfig,
        deadline: Duration,
        now: u64,
    ) -> Result<(BrowserRunResult, Option<TabKey>, String), BrowserError> {
        match &params.op {
            BrowserOp::Open { url, focus, wait } => {
                let url = shape::normalize_url(url)?;
                let reply = match self.request(
                    "open",
                    Some(profile),
                    None,
                    json!({ "url": url, "background": !focus, "wait": wait }),
                    deadline,
                ) {
                    Ok(reply) => reply,
                    Err(err) => {
                        // The tab exists although the page did not load: it is
                        // the caller's (adopted, current) before the error goes out.
                        if let Some(target) = err.target.as_deref() {
                            let key = TabKey::new(profile, target);
                            let mut state = self.inner.state.lock().unwrap();
                            state.adopt_tab(
                                &key,
                                &HostTab {
                                    target: target.to_string(),
                                    url: url.clone(),
                                    title: String::new(),
                                    selected: *focus,
                                    dialog_open: false,
                                },
                                actor,
                                now,
                            );
                            if let Some(pane) = pane {
                                state.set_cursor(pane, &key, actor.tab_id(), now);
                            }
                        }
                        return Err(err);
                    }
                };
                let target = reply.result["target"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if target.is_empty() {
                    return Err(BrowserError::host_failed("open returned no target"));
                }
                let key = TabKey::new(profile, &target);
                let page = reply.page.clone().unwrap_or_default();
                {
                    let mut state = self.inner.state.lock().unwrap();
                    state.adopt_tab(
                        &key,
                        &HostTab {
                            target: target.clone(),
                            url: page.url.clone(),
                            title: page.title.clone(),
                            selected: *focus,
                            dialog_open: page.dialog_open,
                        },
                        actor,
                        now,
                    );
                    if let Some(pane) = pane {
                        state.set_cursor(pane, &key, actor.tab_id(), now);
                    }
                }
                if *focus {
                    let _ = self.request(
                        "focus",
                        Some(profile),
                        Some(&target),
                        Value::Null,
                        Duration::from_secs(3),
                    );
                }
                let record = self.record(&key)?;
                let result = shape::open_result(&record, &page, &reply.result);
                let detail = super::state::display_url(&page.url);
                Ok((result, Some(key), detail))
            }
            BrowserOp::Tabs { mine } => {
                let reply = self.request("tabs", Some(profile), None, Value::Null, deadline)?;
                let tabs: Vec<HostTab> =
                    serde_json::from_value(reply.result["tabs"].clone()).unwrap_or_default();
                let mut state = self.inner.state.lock().unwrap();
                state.reconcile(profile, &tabs, now);
                let cursor = pane.and_then(|pane| state.cursor(pane).map(|r| r.key()));
                let records: Vec<super::state::BrowserTabRecord> = state
                    .open_tabs(profile)
                    .filter(|record| {
                        !*mine || pane.is_some_and(|pane| record.users.iter().any(|u| u == pane))
                    })
                    .cloned()
                    .collect();
                let result = shape::tabs_result(
                    profile,
                    &records,
                    cursor.as_ref(),
                    now,
                    config.active_seconds,
                );
                Ok((result, None, format!("{} tabs", records.len())))
            }
            BrowserOp::Use => {
                let tab = params
                    .tab
                    .as_deref()
                    .map(str::trim)
                    .filter(|tab| !tab.is_empty())
                    .ok_or_else(|| {
                        BrowserError::new(
                            "invalid_request",
                            "use needs a tab (t3 or main:t3); see `herdr browser tabs`",
                        )
                    })?;
                let record = self.resolve_target(profile, Some(tab), None)?;
                let key = record.key();
                if let Some(pane) = pane {
                    self.inner
                        .state
                        .lock()
                        .unwrap()
                        .set_cursor(pane, &key, actor.tab_id(), now);
                }
                let result = shape::simple_result(
                    &record,
                    None,
                    &format!("current tab is now {}", record.id()),
                    json!({ "tab": record.id() }),
                );
                Ok((result, Some(key), record.short.clone()))
            }
            BrowserOp::Snapshot {
                selector,
                ref_,
                offset,
                max,
                interactive,
            } => {
                // The read path with the snapshot format; the ledger keeps "snapshot".
                let read = BrowserRunParams {
                    op: BrowserOp::Read {
                        format: Some("snapshot".into()),
                        selector: selector.clone(),
                        ref_: ref_.clone(),
                        offset: *offset,
                        max: *max,
                        all: false,
                        interactive: *interactive,
                    },
                    ..params.clone()
                };
                self.execute(actor, profile, pane, &read, config, deadline, now)
            }
            BrowserOp::Batch {
                ops,
                stop_on_error,
                final_,
                close_opened,
                animate,
            } => {
                if !*animate {
                    // The steps run without the cursor glide (frame and group stay).
                    ACTIVITY.with(|slot| {
                        if let Some(directive) = slot.borrow_mut().as_mut() {
                            directive["animate"] = json!(false);
                        }
                    });
                }
                self.execute_batch(
                    actor,
                    profile,
                    pane,
                    params,
                    ops,
                    *stop_on_error,
                    final_.as_deref(),
                    *close_opened,
                    config,
                    deadline,
                    now,
                )
            }
            op => {
                let record = self.resolve_target(profile, params.tab.as_deref(), pane)?;
                let key = record.key();
                if let Some(pane) = pane {
                    // Following an explicit --tab moves the cursor there.
                    if params.tab.is_some() {
                        self.inner.state.lock().unwrap().set_cursor(
                            pane,
                            &key,
                            actor.tab_id(),
                            now,
                        );
                    }
                }
                let target = record.target_id.as_str();
                let (result, detail) = match op {
                    BrowserOp::Navigate { url, wait } => {
                        let url = shape::normalize_url(url)?;
                        let reply = self.request(
                            "navigate",
                            Some(profile),
                            Some(target),
                            json!({ "url": url, "wait": wait }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        (
                            shape::nav_result(&record, &page, &reply.result),
                            super::state::display_url(&page.url),
                        )
                    }
                    BrowserOp::History { action } => {
                        let action = match action.as_str() {
                            "back" | "forward" | "reload" => action.as_str(),
                            other => {
                                return Err(BrowserError::new("invalid_request", format!("history action {other:?}: expected back, forward or reload")));
                            }
                        };
                        let reply = self.request(
                            "history",
                            Some(profile),
                            Some(target),
                            json!({ "action": action }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        (
                            shape::nav_result(&record, &page, &reply.result),
                            action.to_string(),
                        )
                    }
                    BrowserOp::Read {
                        format,
                        selector,
                        ref_,
                        offset,
                        max,
                        all,
                        interactive,
                    } => {
                        let format = shape::read_format(format.as_deref())?;
                        let scope = ref_
                            .as_deref()
                            .map(|r| format!("ref:{r}"))
                            .or_else(|| selector.as_deref().map(|s| format!("sel:{s}")))
                            .unwrap_or_default();
                        // Paging (offset > 0) reuses the last extraction of the same
                        // format/scope/URL; a fresh read at offset 0 re-extracts.
                        let cached = if offset.unwrap_or(0) > 0 {
                            self.cached_read(&key, format, &scope, *interactive, &record.url)
                        } else {
                            None
                        };
                        let (content, page) = match cached {
                            Some((content, page)) => (content, page),
                            None => {
                                let reply = self.request(
                                    "read",
                                    Some(profile),
                                    Some(target),
                                    json!({ "format": format, "selector": selector, "ref": ref_, "interactive": interactive }),
                                    deadline,
                                )?;
                                let page = reply.page.clone().unwrap_or_default();
                                let content = reply.result["content"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                self.store_read(
                                    &key,
                                    format,
                                    &scope,
                                    *interactive,
                                    &page,
                                    &content,
                                );
                                (content, page)
                            }
                        };
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        let default_max = if format == "snapshot" {
                            config.snapshot_max_chars()
                        } else {
                            config.read_max_chars()
                        };
                        let paged = shape::page_text(
                            &content,
                            offset.unwrap_or(0),
                            if *all {
                                None
                            } else {
                                Some(max.unwrap_or(default_max).max(200))
                            },
                        );
                        let detail = format!("{format} {}", paged.detail());
                        (
                            shape::read_result(
                                &record,
                                &page,
                                format,
                                &paged,
                                ref_.as_deref(),
                                selector.as_deref(),
                            ),
                            detail,
                        )
                    }
                    BrowserOp::Find {
                        query,
                        max,
                        context,
                    } => {
                        // `find` reuses the last whole-page markdown of this URL.
                        let (content, page) =
                            match self.cached_read(&key, "markdown", "", false, &record.url) {
                                Some(hit) => hit,
                                None => {
                                    let reply = self.request(
                                        "read",
                                        Some(profile),
                                        Some(target),
                                        json!({ "format": "markdown" }),
                                        deadline,
                                    )?;
                                    let page = reply.page.clone().unwrap_or_default();
                                    let content = reply.result["content"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .to_string();
                                    self.store_read(&key, "markdown", "", false, &page, &content);
                                    (content, page)
                                }
                            };
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        let content = content.as_str();
                        let matches = shape::find_matches(
                            content,
                            query,
                            max.unwrap_or(20).clamp(1, 200) as usize,
                            context.unwrap_or(120).clamp(20, 2000) as usize,
                        )?;
                        let detail = format!("{query:?} {} matches", matches.len());
                        (
                            shape::find_result(
                                &record,
                                &page,
                                query,
                                &matches,
                                content.chars().count(),
                            ),
                            detail,
                        )
                    }
                    BrowserOp::Links { filter, max } => {
                        let reply = self.request(
                            "links",
                            Some(profile),
                            Some(target),
                            json!({ "filter": filter, "max": max.unwrap_or(100).clamp(1, 2000) }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        let count = reply.result["links"].as_array().map(Vec::len).unwrap_or(0);
                        (
                            shape::links_result(&record, &page, &reply.result),
                            format!("{count} links"),
                        )
                    }
                    BrowserOp::Screenshot {
                        full,
                        ref_,
                        selector,
                        format,
                        out,
                        front,
                    } => {
                        let format = match format.as_deref().unwrap_or("jpeg") {
                            "jpeg" | "jpg" => "jpeg",
                            "png" => "png",
                            other => {
                                return Err(BrowserError::new(
                                    "invalid_request",
                                    format!("screenshot format {other:?}: expected jpeg or png"),
                                ))
                            }
                        };
                        let shots_dir = self.inner.home.join(super::shots::SHOTS_DIR).join(profile);
                        let (path, inline_path) = super::shots::paths(
                            &shots_dir,
                            &record.short,
                            format,
                            out.as_deref(),
                            now,
                        )?;
                        if *front {
                            self.request(
                                "focus",
                                Some(profile),
                                Some(target),
                                Value::Null,
                                Duration::from_secs(3),
                            )?;
                        }
                        let reply = self.request(
                            "screenshot",
                            Some(profile),
                            Some(target),
                            json!({
                                "path": path, "inline_path": inline_path, "full": full, "ref": ref_, "selector": selector,
                                "format": format, "quality": 70, "max_px": config.screenshot_max_px(),
                            }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        super::shots::prune(&shots_dir, config.screenshot_keep() as usize);
                        let detail = format!(
                            "{} {}x{}",
                            if *full { "full" } else { "viewport" },
                            reply.result["width"].as_u64().unwrap_or(0),
                            reply.result["height"].as_u64().unwrap_or(0)
                        );
                        (
                            shape::screenshot_result(&record, &page, &reply.result, format),
                            detail,
                        )
                    }
                    BrowserOp::Console { level, since, max } => {
                        let reply = self.request("console", Some(profile), Some(target), json!({ "level": level.as_deref().unwrap_or("all"), "since": since, "max": max.unwrap_or(50).clamp(1, 1000) }), deadline)?;
                        let page = reply.page.clone().unwrap_or_default();
                        let record = self.record(&key)?;
                        let count = reply.result["entries"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        (
                            shape::console_result(&record, &page, &reply.result),
                            format!("{count} entries"),
                        )
                    }
                    BrowserOp::Network {
                        failed,
                        match_,
                        type_,
                        since,
                        max,
                    } => {
                        let reply = self.request("network", Some(profile), Some(target), json!({ "failed": failed, "match": match_, "type": type_, "since": since, "max": max.unwrap_or(50).clamp(1, 1000) }), deadline)?;
                        let page = reply.page.clone().unwrap_or_default();
                        let record = self.record(&key)?;
                        let count = reply.result["entries"]
                            .as_array()
                            .map(Vec::len)
                            .unwrap_or(0);
                        (
                            shape::network_result(&record, &page, &reply.result),
                            format!("{count} requests"),
                        )
                    }
                    BrowserOp::Wait {
                        text,
                        gone,
                        selector,
                        url,
                        load,
                        timeout_s,
                    } => {
                        if text.is_none()
                            && gone.is_none()
                            && selector.is_none()
                            && url.is_none()
                            && load.is_none()
                        {
                            return Err(BrowserError::new(
                                "invalid_request",
                                "wait needs one of --text, --gone, --selector, --url or --load",
                            ));
                        }
                        let timeout_ms = timeout_s.unwrap_or(30).clamp(1, 300) * 1000;
                        let reply = self.request("wait", Some(profile), Some(target), json!({ "text": text, "gone": gone, "selector": selector, "url": url, "load": load, "timeout_ms": timeout_ms }), deadline)?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        let what = text
                            .as_deref()
                            .or(gone.as_deref())
                            .or(selector.as_deref())
                            .or(url.as_deref())
                            .or(load.as_deref())
                            .unwrap_or("");
                        (
                            shape::wait_result(&record, &page, &reply.result),
                            format!(
                                "{what:?} {} ms",
                                reply.result["elapsed_ms"].as_u64().unwrap_or(0)
                            ),
                        )
                    }
                    BrowserOp::Scroll { to, by } => {
                        if to.is_none() && by.is_none() {
                            return Err(BrowserError::new(
                                "invalid_request",
                                "scroll needs --to top|bottom|eN or --by PX",
                            ));
                        }
                        let reply = self.request(
                            "scroll",
                            Some(profile),
                            Some(target),
                            json!({ "to": to, "by": by }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        let record = self.record(&key)?;
                        (
                            shape::scroll_result(&record, &page, &reply.result),
                            to.clone()
                                .unwrap_or_else(|| format!("by {}", by.unwrap_or(0))),
                        )
                    }
                    BrowserOp::Eval { expr, max } => {
                        if !config.allow_eval {
                            return Err(BrowserError::new(
                                "eval_disabled",
                                "browser eval is disabled ([browser] allow_eval = false)",
                            ));
                        }
                        // The password rule holds for eval too: the sidecar
                        // snapshots the page's password fields around the
                        // code and refuses (restoring them) when one changed.
                        let reply = self
                            .request(
                                "eval",
                                Some(profile),
                                Some(target),
                                json!({ "expr": expr, "guard_passwords": !config.type_into_password_fields }),
                                deadline,
                            )
                            .map_err(|mut err| {
                                // A refused eval still shows what ran in the ledger.
                                if err.code == "password_field_refused" {
                                    err.message = format!("{} — {}", err.message, shape::eval_detail(expr));
                                }
                                err
                            })?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        let record = self.record(&key)?;
                        (
                            shape::eval_result(
                                &record,
                                &page,
                                &reply.result,
                                max.unwrap_or(4000).clamp(100, 200_000) as usize,
                            ),
                            shape::eval_detail(expr),
                        )
                    }
                    BrowserOp::Dialog { action, text } => {
                        let accept = match action.as_str() {
                            "accept" => true,
                            "dismiss" => false,
                            other => {
                                return Err(BrowserError::new(
                                    "invalid_request",
                                    format!("dialog action {other:?}: expected accept or dismiss"),
                                ))
                            }
                        };
                        let reply = self.request(
                            "dialog",
                            Some(profile),
                            Some(target),
                            json!({ "accept": accept, "text": text }),
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        self.inner.state.lock().unwrap().set_dialog(&key, false);
                        let record = self.record(&key)?;
                        (
                            shape::simple_result(
                                &record,
                                Some(&page),
                                &format!("dialog {action}ed"),
                                reply.result,
                            ),
                            action.clone(),
                        )
                    }
                    BrowserOp::Close => {
                        let _ = self.request(
                            "close",
                            Some(profile),
                            Some(target),
                            Value::Null,
                            deadline,
                        )?;
                        let id = record.id();
                        self.inner.state.lock().unwrap().close_tab(&key, now);
                        let result = BrowserRunResult {
                            header: format!("[{id} · closed]"),
                            text: format!("closed {id}"),
                            tab: Some(id.clone()),
                            data: json!({ "tab": id, "closed": true }),
                            ..Default::default()
                        };
                        (result, record.short.clone())
                    }
                    BrowserOp::Focus => {
                        let reply = self.request(
                            "focus",
                            Some(profile),
                            Some(target),
                            Value::Null,
                            deadline,
                        )?;
                        let page = reply.page.clone().unwrap_or_default();
                        let record = self.record(&key)?;
                        (
                            shape::simple_result(
                                &record,
                                Some(&page),
                                &format!("{} selected and the window raised", record.id()),
                                json!({ "tab": record.id() }),
                            ),
                            record.short.clone(),
                        )
                    }
                    BrowserOp::Click { .. }
                    | BrowserOp::Type { .. }
                    | BrowserOp::Press { .. }
                    | BrowserOp::Select { .. }
                    | BrowserOp::Fill { .. }
                    | BrowserOp::Hover { .. } => {
                        if !config.allow_act {
                            return Err(BrowserError::new(
                                "act_disabled",
                                "browser act (click, type, press, select, fill, hover) is disabled ([browser] allow_act = false)",
                            ));
                        }
                        let (kind, ref_, selector, mut args, typed_len) = act_args(op);
                        let target_name = ref_
                            .clone()
                            .or(selector.clone())
                            .unwrap_or_else(|| "focused element".into());
                        args["kind"] = json!(kind);
                        args["ref"] = json!(ref_);
                        args["selector"] = json!(selector);
                        args["allow_password"] = json!(config.type_into_password_fields);
                        let reply =
                            self.request("act", Some(profile), Some(target), args, deadline)?;
                        let page = reply.page.clone().unwrap_or_default();
                        self.note_page(&key, &page);
                        if page.dialog_open {
                            self.inner.state.lock().unwrap().set_dialog(&key, true);
                        }
                        let record = self.record(&key)?;
                        (
                            shape::act_result(
                                &record,
                                &page,
                                kind,
                                &target_name,
                                &reply.result,
                                typed_len,
                            ),
                            shape::act_detail(kind, &target_name, &reply.result, typed_len),
                        )
                    }
                    BrowserOp::Open { .. }
                    | BrowserOp::Tabs { .. }
                    | BrowserOp::Use
                    | BrowserOp::Snapshot { .. }
                    | BrowserOp::Batch { .. }
                    | BrowserOp::Unknown => unreachable!(),
                };
                Ok((result, Some(key), detail))
            }
        }
    }

    /// A batch: every step through `execute` with its own bookkeeping, one
    /// overall deadline, the cursor moving as steps `use`/`open`; callers
    /// without a pane follow the last step's tab.
    #[allow(clippy::too_many_arguments)]
    fn execute_batch(
        &self,
        actor: &BrowserActor,
        profile: &str,
        pane: Option<&str>,
        params: &BrowserRunParams,
        ops: &[crate::api::schema::BrowserBatchStep],
        stop_on_error: bool,
        final_: Option<&str>,
        close_opened: bool,
        config: &BrowserConfig,
        deadline: Duration,
        now: u64,
    ) -> Result<(BrowserRunResult, Option<TabKey>, String), BrowserError> {
        use crate::api::schema::BATCH_MAX_STEPS;
        if ops.is_empty() {
            return Err(BrowserError::new(
                "invalid_request",
                "batch needs at least one step",
            ));
        }
        if ops.len() > BATCH_MAX_STEPS {
            return Err(BrowserError::new(
                "batch_too_long",
                format!("batch has {} steps; at most {BATCH_MAX_STEPS}", ops.len()),
            ));
        }
        if ops
            .iter()
            .any(|step| matches!(step.op, BrowserOp::Batch { .. } | BrowserOp::Unknown))
        {
            return Err(BrowserError::new(
                "invalid_request",
                "a batch step cannot be a batch or an unknown op",
            ));
        }
        let final_op = match final_ {
            None => None,
            Some("snapshot") => Some(BrowserOp::Read {
                format: Some("snapshot".into()),
                selector: None,
                ref_: None,
                offset: None,
                max: None,
                all: false,
                interactive: false,
            }),
            Some("screenshot") => Some(BrowserOp::Screenshot {
                full: false,
                ref_: None,
                selector: None,
                format: None,
                out: None,
                front: false,
            }),
            Some(other) => {
                return Err(BrowserError::new(
                    "invalid_request",
                    format!("batch final {other:?}: expected snapshot or screenshot"),
                ))
            }
        };
        // The batch's own tab becomes the cursor (or, without a pane, the
        // tab every step follows until one switches it).
        let cursor_before = pane.and_then(|pane| {
            self.inner
                .state
                .lock()
                .unwrap()
                .cursor(pane)
                .map(|record| record.key())
        });
        let mut opened: Vec<String> = Vec::new();
        let mut current_tab: Option<String> = params.tab.clone();
        if let (Some(pane), Some(tab)) = (pane, params.tab.as_deref()) {
            let record = self.resolve_target(profile, Some(tab), None)?;
            self.inner
                .state
                .lock()
                .unwrap()
                .set_cursor(pane, &record.key(), actor.tab_id(), now);
            current_tab = None;
        }
        let started = Instant::now();
        let mut lines: Vec<shape::BatchStepLine> = Vec::new();
        let mut last_key: Option<TabKey> = None;
        let mut last_header = String::new();
        let mut failed = false;
        // `track`: the step's header and tab become the batch result's
        // (false for the tidy-up closes of close_opened).
        let mut run_step = |op: &BrowserOp,
                            tab: Option<String>,
                            index: usize,
                            track: bool,
                            lines: &mut Vec<shape::BatchStepLine>|
         -> Result<BrowserRunResult, BrowserError> {
            let remaining = deadline.saturating_sub(started.elapsed());
            let step_params = BrowserRunParams {
                caller: params.caller.clone(),
                profile: Some(profile.to_string()),
                tab,
                op: op.clone(),
                timeout_ms: None,
            };
            let step_started = Instant::now();
            let result = if remaining < Duration::from_millis(200) {
                Err(BrowserError::timeout(format!(
                    "the batch deadline ({} ms) passed before step {}",
                    deadline.as_millis(),
                    index + 1
                )))
            } else {
                self.execute(
                    actor,
                    profile,
                    pane,
                    &step_params,
                    config,
                    remaining,
                    unix_now(),
                )
            };
            let ms = step_started.elapsed().as_millis() as u64;
            self.record_outcome(actor, profile, pane, &step_params, op.name(), &result, ms);
            match result {
                Ok((result, key, detail)) => {
                    if track {
                        if key.is_some() {
                            last_key = key;
                        }
                        last_header = result.header.clone();
                    }
                    // A snapshot step's refs serve the next steps; its output
                    // travels with the step (already paged to snapshot_max_chars).
                    // The final snapshot is a read, shown once as the final result.
                    let output = match op {
                        BrowserOp::Snapshot { .. } => Some(result.text.clone()),
                        _ => None,
                    };
                    lines.push(shape::BatchStepLine {
                        index,
                        op: op.name().to_string(),
                        outcome: detail,
                        ok: true,
                        skipped: false,
                        output,
                    });
                    Ok(result)
                }
                Err(err) => {
                    lines.push(shape::BatchStepLine {
                        index,
                        op: op.name().to_string(),
                        outcome: format!("{}: {}", err.code, err.message),
                        ok: false,
                        skipped: false,
                        output: None,
                    });
                    Err(err)
                }
            }
        };
        for (index, step) in ops.iter().enumerate() {
            if failed && stop_on_error {
                lines.push(shape::BatchStepLine {
                    index,
                    op: step.op.name().to_string(),
                    outcome: String::new(),
                    ok: false,
                    skipped: true,
                    output: None,
                });
                continue;
            }
            let tab = step.tab.clone().or_else(|| {
                if pane.is_some() {
                    None
                } else {
                    current_tab.clone()
                }
            });
            match run_step(&step.op, tab, index, true, &mut lines) {
                Ok(result) => {
                    if matches!(step.op, BrowserOp::Open { .. }) {
                        if let Some(tab) = result.tab.clone() {
                            opened.push(tab);
                        }
                    }
                    if pane.is_none() {
                        if let Some(tab) = result.tab.clone() {
                            current_tab = Some(tab);
                        }
                    }
                }
                Err(err) => {
                    failed = true;
                    // An open whose page failed still made a tab: close_opened covers it.
                    if let (BrowserOp::Open { .. }, Some(target)) =
                        (&step.op, err.target.as_deref())
                    {
                        let id = self
                            .inner
                            .state
                            .lock()
                            .unwrap()
                            .tabs
                            .get(&TabKey::new(profile, target))
                            .map(|record| record.id());
                        if let Some(id) = id {
                            opened.push(id);
                        }
                    }
                }
            }
        }
        let final_result = match final_op {
            Some(op) if !(failed && stop_on_error) => {
                let tab = if pane.is_some() {
                    None
                } else {
                    current_tab.clone()
                };
                run_step(&op, tab, ops.len(), true, &mut lines).ok()
            }
            _ => None,
        };
        // close_opened: the tabs this batch opened go after the final step;
        // the pane's cursor returns to where it was.
        if close_opened && !opened.is_empty() {
            // The result keeps the final step's header; its tab is the one the
            // caller is left on (the restored cursor), never a closed one.
            let first = lines.len();
            for (index, tab) in (first..).zip(opened.iter()) {
                let _ = run_step(
                    &BrowserOp::Close,
                    Some(tab.clone()),
                    index,
                    false,
                    &mut lines,
                );
            }
            let mut state = self.inner.state.lock().unwrap();
            let restored = cursor_before
                .as_ref()
                .filter(|key| state.tabs.get(key).is_some_and(|record| record.is_open()));
            match (pane, restored) {
                (Some(pane), Some(key)) => {
                    state.set_cursor(pane, key, actor.tab_id(), unix_now());
                    last_key = Some(key.clone());
                }
                _ => {
                    last_key = last_key
                        .filter(|key| state.tabs.get(key).is_some_and(|record| record.is_open()));
                }
            }
        }
        let ok = lines.iter().filter(|line| line.ok).count();
        let failed_count = lines
            .iter()
            .filter(|line| !line.ok && !line.skipped)
            .count();
        let tab_id = last_key.as_ref().and_then(|key| {
            self.inner
                .state
                .lock()
                .unwrap()
                .tabs
                .get(key)
                .map(|r| r.id())
        });
        let header = if last_header.is_empty() {
            format!("[{profile} · batch]")
        } else {
            last_header
        };
        let result = shape::batch_result(header, tab_id, &lines, final_result.as_ref());
        let detail = format!("{} steps · {ok} ok · {failed_count} failed", lines.len());
        Ok((result, last_key, detail))
    }

    /// `[browser] stop_with_server`: close every running profile and wait
    /// for the processes to go, at server shutdown.
    pub fn stop_with_server_if_configured(&self) {
        if !self.config().stop_with_server {
            return;
        }
        let names: Vec<String> = self
            .inner
            .state
            .lock()
            .unwrap()
            .profiles
            .iter()
            .filter(|(_, status)| {
                status.is_running() || matches!(status, ProfileStatus::Starting { .. })
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in names {
            self.stop_profile_with(&name, false);
        }
    }

    fn record(&self, key: &TabKey) -> Result<super::state::BrowserTabRecord, BrowserError> {
        self.inner
            .state
            .lock()
            .unwrap()
            .tabs
            .get(key)
            .cloned()
            .ok_or_else(|| BrowserError::tab_closed(&key.target_id))
    }

    fn note_page(&self, key: &TabKey, page: &PageInfo) {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(record) = state.tabs.get_mut(key) {
            if !page.url.is_empty() {
                record.url = page.url.clone();
            }
            if !page.title.is_empty() {
                record.title = page.title.clone();
            }
            record.dialog_open = page.dialog_open;
            state.dirty = true;
        }
    }

    // ----- Lifecycle ------------------------------------------------------

    fn profile_lock(&self, name: &str) -> Arc<Mutex<()>> {
        self.inner
            .profile_locks
            .lock()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .clone()
    }

    /// Attach-or-launch the profile, make sure the sidecar runs and is
    /// attached to it, reconcile its tabs.
    pub fn ensure_running(&self, name: &str) -> Result<(), BrowserError> {
        let lock = self.profile_lock(name);
        let _guard = lock.lock().unwrap();
        self.start_supervisor();
        let config = self.config();
        let profile_dir = self.inner.profiles.dir(name);
        let log_path = self.inner.profiles.log_path(name);

        let already_attached = {
            let state = self.inner.state.lock().unwrap();
            let running = state.profile(name).is_running();
            let host = self.inner.host.lock().unwrap();
            running
                && host
                    .as_ref()
                    .is_some_and(|link| link.attached.contains(name))
        };
        if already_attached {
            return Ok(());
        }
        // An agent's first use after a herdr update (autostart is off by
        // default) repairs the helper before the sidecar starts: once per
        // process, concurrent first uses wait for it.
        self.auto_repair_once();

        let alive = |pid: u32| crate::platform::process_exists(pid);
        let answers = |port: u16| launch::json_version(port, Duration::from_millis(1500)).is_some();
        let decision = if self.inner.test_mode.load(Ordering::Relaxed) {
            match self.inner.state.lock().unwrap().profile(name) {
                ProfileStatus::Running { pid, port, exe, .. } => {
                    AttachDecision::Attach(RunRecord {
                        pid,
                        port,
                        exe,
                        launched_at: 0,
                        server_pid: 0,
                        argv: vec![],
                        browser: String::new(),
                    })
                }
                _ => AttachDecision::Launch,
            }
        } else {
            launch::attach_decision(&self.inner.home, name, &profile_dir, &alive, &answers)
        };
        let record = match decision {
            AttachDecision::Attach(record) => record,
            AttachDecision::InUse { pid } => {
                self.inner.state.lock().unwrap().set_profile(
                    name,
                    ProfileStatus::InUse {
                        pid: Some(pid),
                        at: unix_now(),
                    },
                );
                return Err(BrowserError::new(
                    "profile_in_use",
                    format!("profile {name:?} is open in another Chromium (pid {pid}) that herdr did not launch; close it or use another profile"),
                ));
            }
            AttachDecision::Launch => {
                self.inner.profiles.ensure(
                    name,
                    name.starts_with(profiles::TEMPORARY_PREFIX),
                    unix_now(),
                )?;
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .set_profile(name, ProfileStatus::Starting { since: unix_now() });
                // The companion extension is installed with the sidecar assets;
                // a fresh server may launch before its first host start.
                let host_dir = self.host_dir();
                let extension_dir = if config.show_activity {
                    match browser_assets::install(&host_dir) {
                        Ok(_) => Some(host_dir.join(browser_assets::COMPANION_DIR)),
                        Err(err) => {
                            tracing::warn!(event = "browser.companion.install", error = %err, "companion extension not installed; tab groups off");
                            None
                        }
                    }
                } else {
                    None
                };
                let options = LaunchOptions {
                    restore: config.restore_tabs && self.inner.profiles.has_launched(name),
                    extra_args: config.extra_args(),
                    first_launch: !self.inner.profiles.has_launched(name),
                    timeout: Duration::from_millis(config.launch_timeout_ms()),
                    server_pid: std::process::id(),
                    extension_dir,
                };
                let launched = {
                    let home_env = std::env::var_os("HOME").map(PathBuf::from);
                    launch::resolve_executable(&config.executable, home_env.as_deref()).and_then(
                        |exe: Executable| {
                            launch::launch(
                                &self.inner.home,
                                name,
                                &profile_dir,
                                &log_path,
                                &exe,
                                &options,
                            )
                        },
                    )
                };
                match launched {
                    Ok(record) => {
                        let _ = self.inner.profiles.mark_launched(name);
                        record
                    }
                    Err(err) => {
                        self.inner
                            .state
                            .lock()
                            .unwrap()
                            .set_profile(name, ProfileStatus::Stopped);
                        return Err(err);
                    }
                }
            }
        };
        self.inner.state.lock().unwrap().set_profile(
            name,
            ProfileStatus::Running {
                pid: record.pid,
                port: record.port,
                since: unix_now(),
                exe: if record.browser.is_empty() {
                    record.exe.clone()
                } else {
                    record.browser.clone()
                },
            },
        );
        self.ensure_host()?;
        // The attach can stall on a page dialog or on a tab that is still
        // restoring; one retry after a pause covers the transient case, the
        // rest is reported as attach_blocked with the remedy.
        let attach_timed_out =
            |err: &BrowserError| err.code == "browser_timeout" || err.code == "attach_timeout";
        let mut attempt = 0;
        let reply = loop {
            attempt += 1;
            match self.request(
                "attach",
                Some(name),
                None,
                json!({ "port": record.port, "pin_dashboard": config.show_activity && config.pin_dashboard }),
                ATTACH_TIMEOUT,
            ) {
                Ok(reply) => break reply,
                Err(err) if attach_timed_out(&err) && attempt < 2 => {
                    tracing::warn!(event = "browser.attach.retry", profile = %name, message = %err.message, "attach timed out; retrying once");
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Err(err) if attach_timed_out(&err) => {
                    return Err(BrowserError::new(
                        "attach_blocked",
                        format!(
                            "the browser did not accept the attach within {} s (twice): a page dialog may be waiting in the Chromium window (answer it), or a restored tab is still loading; retry, or `herdr browser stop` and open again",
                            ATTACH_TIMEOUT.as_secs()
                        ),
                    ));
                }
                Err(err) => return Err(err),
            }
        };
        let tabs: Vec<HostTab> =
            serde_json::from_value(reply.result["tabs"].clone()).unwrap_or_default();
        {
            let mut state = self.inner.state.lock().unwrap();
            state.reconcile(name, &tabs, unix_now());
        }
        if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
            link.attached.insert(name.to_string());
        }
        // The companion extension (tab groups): what the sidecar found.
        let companion = if !config.show_activity {
            "off".to_string()
        } else {
            let state = reply.result["companion"]["state"]
                .as_str()
                .unwrap_or("unknown");
            match reply.result["companion"]["detail"].as_str() {
                Some(detail) if !detail.is_empty() => format!("{state} ({detail})"),
                _ => state.to_string(),
            }
        };
        self.inner
            .companion
            .lock()
            .unwrap()
            .insert(name.to_string(), companion);
        self.flush();
        self.schedule_ntp_push();
        Ok(())
    }

    fn stop_profile(&self, name: &str) {
        self.stop_profile_with(name, true);
    }

    /// `attach`: a running profile this server has not attached yet (a fresh
    /// server with the last one's run record, adopted on first use) is
    /// attached first, so Browser.close reaches it instead of a signal. The
    /// shutdown path passes false: it never starts a sidecar or attaches, it
    /// only closes what is attached and terminates the recorded live pid.
    fn stop_profile_with(&self, name: &str, attach: bool) {
        // Only a live browser is attached to (a Running state whose process is
        // gone must not make this launch one in order to stop it).
        let live_pid = {
            let state = self.inner.state.lock().unwrap();
            state.profile(name).pid()
        }
        .or_else(|| launch::read_run_record(&self.inner.home, name).map(|record| record.pid))
        .filter(|pid| crate::platform::process_exists(*pid));
        let is_attached = || {
            let host = self.inner.host.lock().unwrap();
            host.as_ref()
                .is_some_and(|link| link.attached.contains(name))
        };
        if attach && live_pid.is_some() && !is_attached() {
            if let Err(err) = self.ensure_running(name) {
                tracing::warn!(event = "browser.stop.attach", profile = %name, code = %err.code, message = %err.message, "could not attach before stopping; the process is signalled instead");
            }
        }
        let lock = self.profile_lock(name);
        let _guard = lock.lock().unwrap();
        let status = self.inner.state.lock().unwrap().profile(name);
        let pid = status.pid().or(live_pid);
        let closed = is_attached()
            && self
                .request("close_browser", Some(name), None, Value::Null, STOP_GRACE)
                .is_ok();
        if let Some(pid) = pid {
            let deadline = Instant::now() + STOP_GRACE;
            while crate::platform::process_exists(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if crate::platform::process_exists(pid) {
                launch::terminate(pid);
                let deadline = Instant::now() + STOP_GRACE;
                while crate::platform::process_exists(pid) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if crate::platform::process_exists(pid) {
                // Still up (a page asked the user before closing): it stays
                // ours; the run record and the state keep saying so.
                tracing::warn!(event = "browser.stop.survived", profile = %name, pid, "browser did not exit; profile left running");
                return;
            }
        }
        if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
            link.attached.remove(name);
        }
        launch::clear_run_record(&self.inner.home, name);
        self.inner.companion.lock().unwrap().remove(name);
        let mut state = self.inner.state.lock().unwrap();
        state.close_profile_tabs(name, unix_now());
        state.set_profile(name, ProfileStatus::Stopped);
        tracing::info!(event = "browser.stop", profile = %name, graceful = closed, "browser profile stopped");
        drop(state);
        self.flush();
    }

    fn host_dir(&self) -> PathBuf {
        self.inner.home.join(browser_assets::HOST_DIR)
    }

    /// Make sure a sidecar runs (spawn + `hello` when needed).
    fn ensure_host(&self) -> Result<(), BrowserError> {
        let _guard = self.inner.host_lock.lock().unwrap();
        if self.inner.host.lock().unwrap().is_some() {
            return Ok(());
        }
        if self.inner.test_mode.load(Ordering::Relaxed) {
            return Err(BrowserError::host_failed("no fake host installed"));
        }
        if let HostStatus::Failed { error, .. } = &self.inner.state.lock().unwrap().host {
            return Err(BrowserError::host_failed(format!(
                "the browser sidecar keeps failing ({error}); `herdr browser start` retries, `herdr browser doctor` explains"
            )));
        }
        {
            let mut respawns = self.inner.respawns.lock().unwrap();
            let now = Instant::now();
            while respawns
                .front()
                .is_some_and(|at| now.duration_since(*at) > RESPAWN_WINDOW)
            {
                respawns.pop_front();
            }
            if respawns.len() >= RESPAWN_LIMIT {
                let tail = host::log_tail(&self.host_dir().join("host.log"), 20);
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .set_host(HostStatus::Failed {
                        error: format!(
                            "{RESPAWN_LIMIT} sidecar restarts in {} s",
                            RESPAWN_WINDOW.as_secs()
                        ),
                        log_tail: tail.clone(),
                    });
                return Err(BrowserError::host_failed(format!(
                    "the browser sidecar restarted {RESPAWN_LIMIT} times in a minute; last log lines:\n{tail}"
                )));
            }
            respawns.push_back(now);
        }
        let config = self.config();
        let host_dir = self.host_dir();
        let runtime = browser_assets::read_runtime(&host_dir);
        if runtime.is_none()
            || !host_dir
                .join("node_modules")
                .join("playwright-core")
                .is_dir()
        {
            return Err(BrowserError::new(
                "browser_runtime_missing",
                "the browser sidecar is not installed; run `herdr browser setup`",
            ));
        }
        if runtime
            .as_ref()
            .is_some_and(|r| r.assets_sha256 != browser_assets::assets_sha256())
        {
            // Refresh the assets in place: only the node_modules need npm.
            match browser_assets::install(&host_dir) {
                Ok(_) => {
                    // Scripts refreshed in place; node_modules only npm can. A pin
                    // bump means the installed playwright-core no longer matches.
                    let installed = browser_assets::playwright_installed(&host_dir);
                    if installed.as_deref() != Some(browser_assets::PLAYWRIGHT_CORE_VERSION) {
                        return Err(BrowserError::new(
                            "browser_runtime_outdated",
                            format!(
                                "the installed playwright-core is {}, this herdr expects {}; run `herdr browser setup`",
                                installed.as_deref().unwrap_or("missing"),
                                browser_assets::PLAYWRIGHT_CORE_VERSION
                            ),
                        ));
                    }
                    if let Some(mut r) = runtime.clone() {
                        r.assets_sha256 = browser_assets::assets_sha256();
                        let _ = browser_assets::write_runtime(&host_dir, &r);
                    }
                }
                Err(err) => {
                    return Err(BrowserError::new(
                        "browser_runtime_outdated",
                        format!("the browser sidecar assets are outdated and could not be refreshed ({err}); run `herdr browser setup`"),
                    ));
                }
            }
        }
        let node = super::node::discover_default(
            config.node(),
            runtime.as_ref().map(|r| Path::new(&r.node)),
        )
        .ok_or_else(|| {
            BrowserError::new(
                "browser_runtime_missing",
                "no node binary found; run `herdr browser setup` or set [browser] node",
            )
        })?;
        self.inner
            .state
            .lock()
            .unwrap()
            .set_host(HostStatus::Starting);
        let spawned = host::spawn(&node.path, &host_dir, browser_assets::ENTRY).map_err(|err| {
            self.inner
                .state
                .lock()
                .unwrap()
                .set_host(HostStatus::Absent);
            BrowserError::host_failed(format!(
                "failed to start the browser sidecar with {}: {err}",
                node.path.display()
            ))
        })?;
        let generation = self.inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let pid = spawned.pid;
        *self.inner.host.lock().unwrap() = Some(HostLink {
            generation,
            pid,
            child: Some(spawned.child),
            writer: spawned.writer,
            playwright: String::new(),
            attached: HashSet::new(),
            missed_pings: 0,
        });
        self.spawn_reader(generation, spawned.reader);
        drop(_guard);
        let hello = match self.request("hello", None, None, Value::Null, Duration::from_secs(10)) {
            Ok(reply) => reply,
            Err(err) => {
                let tail = host::log_tail(&host_dir.join("host.log"), 20);
                self.drop_host(generation, Some(format!("hello failed: {}", err.message)));
                return Err(BrowserError::host_failed(format!(
                    "the browser sidecar did not answer hello ({}); host.log:\n{tail}",
                    err.message
                )));
            }
        };
        let protocol = hello.result["host_protocol"].as_u64().unwrap_or(0) as u32;
        if protocol != host::HOST_PROTOCOL {
            self.drop_host(
                generation,
                Some(format!(
                    "host protocol {protocol}, expected {}",
                    host::HOST_PROTOCOL
                )),
            );
            return Err(BrowserError::new(
                "browser_runtime_outdated",
                format!("the installed sidecar speaks protocol {protocol}, this herdr expects {}; run `herdr browser setup`", host::HOST_PROTOCOL),
            ));
        }
        let playwright = hello.result["playwright_version"]
            .as_str()
            .unwrap_or("?")
            .to_string();
        if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
            if link.generation == generation {
                link.playwright = playwright.clone();
            }
        }
        self.inner
            .state
            .lock()
            .unwrap()
            .set_host(HostStatus::Running {
                pid,
                playwright,
                node: node.path.display().to_string(),
            });
        tracing::info!(event = "browser.host.start", pid, node = %node.path.display(), "browser sidecar started");
        Ok(())
    }

    fn spawn_reader(&self, generation: u64, mut reader: Box<dyn std::io::BufRead + Send>) {
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-host-reader".into())
            .spawn(move || {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if let Some(message) = host::parse_line(&line) {
                                hub.handle_message(message);
                            }
                        }
                    }
                }
                hub.drop_host(generation, None);
            });
    }

    fn handle_message(&self, message: HostMessage) {
        match message {
            HostMessage::Reply(reply) => {
                let sender = self.inner.pending.lock().unwrap().remove(&reply.id);
                if let Some(sender) = sender {
                    let _ = sender.send(reply);
                }
            }
            HostMessage::Event(event) => self.handle_event(event),
        }
    }

    fn handle_event(&self, event: HostEvent) {
        let now = unix_now();
        // Console-error counts only mark the ledger dirty; the supervisor's next
        // pass persists them (an error-looping page must not write per event).
        let persist_now = !matches!(&event,
            HostEvent::Tab(tab) if tab.kind == "console_error")
            && !matches!(&event, HostEvent::Log { .. } | HostEvent::Unknown);
        match event {
            HostEvent::Tab(tab) => {
                self.inner.state.lock().unwrap().apply_tab_event(&tab, now);
            }
            HostEvent::Dialog {
                profile,
                target,
                state: dialog_state,
                ..
            } => {
                let key = TabKey::new(&profile, &target);
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .set_dialog(&key, dialog_state == "open");
            }
            HostEvent::Browser {
                profile,
                kind,
                detail,
            } => {
                if kind == "disconnected" {
                    if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
                        link.attached.remove(&profile);
                    }
                    let hub = self.clone();
                    // The pid usually goes a moment later; let the supervisor classify
                    // it (crash vs quit) after a short grace.
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1500));
                        hub.check_profile_alive(&profile, Some(&detail));
                    });
                }
            }
            HostEvent::Log { level, text } => {
                tracing::debug!(event = "browser.host.log", level = %level, "{text}");
            }
            HostEvent::Unknown => {}
        }
        if persist_now {
            self.flush();
            self.schedule_ntp_push();
        }
    }

    /// The sidecar went away (EOF, kill): fail its pending calls, forget it.
    fn drop_host(&self, generation: u64, error: Option<String>) {
        let mut host = self.inner.host.lock().unwrap();
        let Some(link) = host.as_mut() else {
            return;
        };
        if link.generation != generation {
            return;
        }
        if let Some(mut child) = link.child.take() {
            host::kill_group(link.pid);
            let _ = child.wait();
        }
        let pid = link.pid;
        *host = None;
        drop(host);
        self.inner.pending.lock().unwrap().clear();
        let mut state = self.inner.state.lock().unwrap();
        match error {
            Some(error) => {
                tracing::warn!(event = "browser.host.exit", pid, %error, "browser sidecar dropped")
            }
            None => tracing::info!(event = "browser.host.exit", pid, "browser sidecar exited"),
        }
        if !matches!(state.host, HostStatus::Failed { .. }) {
            state.set_host(HostStatus::Absent);
        }
    }

    /// Send one request and wait for its reply.
    pub fn request(
        &self,
        op: &str,
        profile: Option<&str>,
        target: Option<&str>,
        args: Value,
        deadline: Duration,
    ) -> Result<HostReply, BrowserError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let line = serde_json::to_string(&HostRequest {
            id,
            op,
            profile,
            target,
            args,
            deadline_ms: deadline.as_millis() as u64,
            activity: ACTIVITY.with(|slot| slot.borrow().clone()),
        })
        .map_err(|err| BrowserError::host_failed(err.to_string()))?;
        {
            let mut host = self.inner.host.lock().unwrap();
            let Some(link) = host.as_mut() else {
                self.inner.pending.lock().unwrap().remove(&id);
                return Err(BrowserError::new(
                    "browser_host_restarted",
                    "the browser sidecar is not running; retry (it restarts on demand)",
                ));
            };
            if let Err(err) = link.writer.write_line(&line) {
                self.inner.pending.lock().unwrap().remove(&id);
                let generation = link.generation;
                drop(host);
                self.drop_host(generation, Some(err.to_string()));
                return Err(BrowserError::new(
                    "browser_host_restarted",
                    "the browser sidecar went away while sending; retry",
                ));
            }
        }
        match rx.recv_timeout(deadline + REPLY_GRACE) {
            Ok(reply) => {
                if reply.ok {
                    Ok(reply)
                } else {
                    let error = reply.error.unwrap_or_default();
                    let code = if error.code.is_empty() { "browser_error".to_string() } else { error.code };
                    Err(BrowserError::new(&code, error.message).with_target(error.target))
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(BrowserError::timeout(format!(
                    "{op} exceeded {} ms (the page may still be loading; try `herdr browser read`)",
                    deadline.as_millis()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(BrowserError::new(
                "browser_host_restarted",
                "the browser sidecar restarted during the call; retry (refs are lost, take a new snapshot)",
            )),
        }
    }

    fn start_supervisor(&self) {
        if self.inner.supervisor.swap(true, Ordering::Relaxed) {
            return;
        }
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("herdr-browser-supervisor".into())
            .spawn(move || loop {
                std::thread::sleep(SUPERVISOR_INTERVAL);
                hub.supervise();
            });
    }

    /// One supervisor pass: dead browsers, wedged sidecar, pruning, flush.
    pub fn supervise(&self) {
        let running: Vec<String> = self
            .inner
            .state
            .lock()
            .unwrap()
            .profiles
            .iter()
            .filter(|(_, status)| status.is_running())
            .map(|(name, _)| name.clone())
            .collect();
        for name in running {
            self.check_profile_alive(&name, None);
        }
        let has_host = self.inner.host.lock().unwrap().is_some();
        if has_host && !self.inner.test_mode.load(Ordering::Relaxed) {
            match self.request("ping", None, None, Value::Null, PING_TIMEOUT) {
                Ok(_) => {
                    if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
                        link.missed_pings = 0;
                    }
                }
                Err(err) if err.code == "browser_timeout" => {
                    let wedged = {
                        let mut host = self.inner.host.lock().unwrap();
                        match host.as_mut() {
                            Some(link) => {
                                link.missed_pings += 1;
                                (link.missed_pings >= PING_MISSES).then_some(link.generation)
                            }
                            None => None,
                        }
                    };
                    if let Some(generation) = wedged {
                        self.drop_host(generation, Some("sidecar missed two pings; killed".into()));
                    }
                }
                Err(_) => {}
            }
        }
        self.inner.state.lock().unwrap().prune(unix_now());
        self.flush();
    }

    /// Reclassify a running profile whose process is gone.
    fn check_profile_alive(&self, name: &str, detail: Option<&str>) {
        let status = self.inner.state.lock().unwrap().profile(name);
        let ProfileStatus::Running { pid, .. } = status else {
            return;
        };
        if self.inner.test_mode.load(Ordering::Relaxed) && pid == 0 {
            return;
        }
        if crate::platform::process_exists(pid) {
            return;
        }
        let exit_type = crate::persist::browser::profile_exit_type(&self.inner.profiles.dir(name));
        let now = unix_now();
        let new_status = if exit_type.as_deref() == Some("Crashed") {
            ProfileStatus::Crashed {
                at: now,
                detail: detail
                    .map(str::to_string)
                    .unwrap_or_else(|| "the browser process exited unexpectedly".into()),
            }
        } else {
            ProfileStatus::UserQuit { at: now }
        };
        if let Some(link) = self.inner.host.lock().unwrap().as_mut() {
            link.attached.remove(name);
        }
        launch::clear_run_record(&self.inner.home, name);
        let mut state = self.inner.state.lock().unwrap();
        state.close_profile_tabs(name, now);
        state.set_profile(name, new_status);
        tracing::info!(event = "browser.profile.exit", profile = %name, pid, exit_type = exit_type.as_deref().unwrap_or("?"), "browser process is gone");
    }

    /// Write the ledger and append new activity when something changed.
    pub fn flush(&self) {
        let ledger = self.inner.ledger_path.lock().unwrap().clone();
        let Some(ledger) = ledger else {
            return;
        };
        // One persistence transaction at a time: concurrent flushes would
        // append the same activity twice and race on the temp file.
        let _flushing = self.inner.flush_lock.lock().unwrap();
        let (snapshot, new_entries) = {
            let mut state = self.inner.state.lock().unwrap();
            if !state.dirty {
                return;
            }
            state.dirty = false;
            let flushed = self.inner.flushed_seq.load(Ordering::Relaxed);
            let new_entries: Vec<BrowserActivity> = state
                .log
                .iter()
                .filter(|entry| entry.seq > flushed)
                .cloned()
                .collect();
            (state.snapshot(), new_entries)
        };
        if let Err(err) = crate::persist::browser::save(&ledger, &snapshot) {
            tracing::warn!(event = "browser.ledger.save", err = %err, "failed to write browser.json");
            self.inner.state.lock().unwrap().dirty = true;
        }
        if let Some(last) = new_entries.last() {
            let activity = self.inner.activity_path.lock().unwrap().clone();
            let appended = match activity {
                Some(activity) => {
                    match crate::persist::browser::append_activity(&activity, &new_entries) {
                        Ok(()) => true,
                        Err(err) => {
                            tracing::warn!(event = "browser.activity.append", err = %err, "failed to append browser activity");
                            false
                        }
                    }
                }
                None => true,
            };
            if appended {
                self.inner.flushed_seq.store(last.seq, Ordering::Relaxed);
            } else {
                self.inner.state.lock().unwrap().dirty = true;
            }
        }
    }

    fn cached_read(
        &self,
        key: &TabKey,
        format: &'static str,
        scope: &str,
        interactive: bool,
        url: &str,
    ) -> Option<(String, PageInfo)> {
        let cache = self.inner.read_cache.lock().unwrap();
        let entry = cache.get(key)?;
        (entry.format == format
            && entry.scope == scope
            && entry.interactive == interactive
            && entry.url == url)
            .then(|| (entry.content.clone(), entry.page.clone()))
    }

    fn store_read(
        &self,
        key: &TabKey,
        format: &'static str,
        scope: &str,
        interactive: bool,
        page: &PageInfo,
        content: &str,
    ) {
        let url = if page.url.is_empty() {
            self.inner
                .state
                .lock()
                .unwrap()
                .tabs
                .get(key)
                .map(|record| record.url.clone())
                .unwrap_or_default()
        } else {
            page.url.clone()
        };
        self.inner.read_cache.lock().unwrap().insert(
            key.clone(),
            ReadCache {
                format,
                scope: scope.to_string(),
                interactive,
                url,
                content: content.to_string(),
                page: page.clone(),
            },
        );
    }

    /// Tests: the profile store and home.
    #[cfg(test)]
    pub fn profiles_store_for_test(&self) -> &ProfileStore {
        &self.inner.profiles
    }

    #[cfg(test)]
    pub fn home_for_test(&self) -> &Path {
        &self.inner.home
    }

    #[cfg(test)]
    pub fn profile_lock_for_test(&self, name: &str) -> Arc<Mutex<()>> {
        self.profile_lock(name)
    }

    /// Tests: a look at the state.
    #[cfg(test)]
    pub fn with_state<R>(&self, f: impl FnOnce(&BrowserState) -> R) -> R {
        f(&self.inner.state.lock().unwrap())
    }

    /// Tests: mutate the ledger directly (fork smoke tests seed a tab).
    #[cfg(test)]
    pub fn with_state_mut<R>(&self, f: impl FnOnce(&mut BrowserState) -> R) -> R {
        f(&mut self.inner.state.lock().unwrap())
    }

    /// Tests: mark a profile running without a launch.
    #[cfg(test)]
    pub fn set_profile_running_for_test(&self, name: &str, pid: u32, port: u16) {
        self.inner.test_mode.store(true, Ordering::Relaxed);
        self.inner.state.lock().unwrap().set_profile(
            name,
            ProfileStatus::Running {
                pid,
                port,
                since: unix_now(),
                exe: "fake".into(),
            },
        );
    }

    /// Whether a sidecar link exists (tests).
    #[cfg(test)]
    pub fn has_host(&self) -> bool {
        self.inner.host.lock().unwrap().is_some()
    }
}

/// The sidecar arguments of an act op: `(kind, ref, selector, args, typed_len)`.
fn act_args(
    op: &BrowserOp,
) -> (
    &'static str,
    Option<String>,
    Option<String>,
    Value,
    Option<usize>,
) {
    match op {
        BrowserOp::Click { ref_, selector } => {
            ("click", ref_.clone(), selector.clone(), json!({}), None)
        }
        BrowserOp::Hover { ref_, selector } => {
            ("hover", ref_.clone(), selector.clone(), json!({}), None)
        }
        BrowserOp::Type {
            ref_,
            selector,
            text,
            submit,
            clear,
        } => (
            "type",
            ref_.clone(),
            selector.clone(),
            json!({ "text": text, "submit": submit, "clear": clear }),
            Some(text.chars().count()),
        ),
        BrowserOp::Fill {
            ref_,
            selector,
            text,
        } => (
            "fill",
            ref_.clone(),
            selector.clone(),
            json!({ "text": text }),
            Some(text.chars().count()),
        ),
        BrowserOp::Select {
            ref_,
            selector,
            value,
        } => (
            "select",
            ref_.clone(),
            selector.clone(),
            json!({ "value": value }),
            None,
        ),
        BrowserOp::Press {
            key,
            ref_,
            selector,
        } => (
            "press",
            ref_.clone(),
            selector.clone(),
            json!({ "key": key }),
            None,
        ),
        _ => ("unknown", None, None, json!({}), None),
    }
}
