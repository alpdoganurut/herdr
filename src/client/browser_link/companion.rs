//! A machine's companion on this Mac: the Electron browser the helper starts
//! for that machine's agents, with its own home, profile and proxy.
//!
//! It starts lazily (the first `ensure` from the remote machine, or the
//! user), with `--remote <home>`: `proxy.json` in the home holds the proxy's
//! port and secret, so the secret is in no argv and no environment. The port
//! and secret are fixed for the companion's life: a restarted helper takes
//! the same ones back from `proxy.json` and adopts the process from its run
//! record, or, when the port cannot come back, stops it and launches a new
//! one. Crashes back off and give up as the Local launcher's do (five in five
//! minutes), on the same state machine ([`Core`]); exit 2 (bad flags or
//! `proxy.json`) gives up at once.
//!
//! The helper holds the home's lock: nothing else starts, adopts or stops a
//! process from this home meanwhile. Lock order: the state's mutex is a
//! leaf; no spawn, signal, sidecar call or hook runs under it.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::schema::{BrowserCompanionAppInfo, BrowserHostRegisterParams};
use crate::browser::companion_app::{
    self, Action, CompanionRunRecord, Core, ExitInfo, LaunchSpec, Phase, RecordedProcess,
    SpawnTicket, Spawner, StartReason, StopSignal,
};
use crate::browser::companion_install::{self, CompanionInstall, InstallProblem};
use crate::browser::link_proto::{text, LinkError, TTL_MS};
use crate::browser::{unix_now, BrowserError};

/// The companion's file of its proxy (`{port, secret, label}`).
pub(crate) const PROXY_FILE: &str = "proxy.json";
/// The machines' homes under the companion's home.
pub(crate) const MACHINES_DIR: &str = "machines";
/// The first app version with `--remote`: an older one would ignore it and
/// browse without the proxy.
pub(crate) const REMOTE_APP_MIN: &str = "0.2.13";
/// What an `ensure` waits for the registration: under the remote side's
/// 45 s, with the 8.4 s a companion waits for a proxy that is down and the
/// sidecar's attach in it.
#[cfg(test)]
pub(crate) const ENSURE_WAIT: Duration = Duration::from_secs(40);
/// A registration not renewed this long is gone (the companion renews it
/// every few seconds).
pub(crate) const LEASE_TTL: Duration = Duration::from_millis(TTL_MS);
/// The companion's exit code for flags or a `proxy.json` it refuses.
pub(crate) const BAD_FLAGS_EXIT: i32 = 2;
/// The stop: the control page's quit, then SIGTERM, then SIGKILL.
const QUIT_WAIT: Duration = Duration::from_secs(8);
/// A process with no live registration gets no quit: this grace, then SIGTERM.
const UNATTACHED_WAIT: Duration = Duration::from_millis(500);
const TERM_WAIT: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(2);
/// A launch in flight when a stop comes ends first, at most this long.
const LAUNCH_WAIT: Duration = Duration::from_secs(10);
const WATCH_INTERVAL: Duration = Duration::from_millis(500);
const SECRET_BYTES: usize = 32;
const SECRET_MAX: usize = 256;

// ---------------------------------------------------------------------------
// The machine's home

/// `<companion home>/machines/<key>/`: the lock, the profile, the run
/// record, the log and `proxy.json` of one machine and session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineHome {
    dir: PathBuf,
}

impl MachineHome {
    pub(crate) fn new(companion_home: &Path, key: &str) -> Self {
        Self {
            dir: companion_home.join(MACHINES_DIR).join(key),
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn profile_dir(&self) -> PathBuf {
        companion_install::profile_dir(&self.dir)
    }

    pub(crate) fn log_path(&self) -> PathBuf {
        companion_install::log_path(&self.dir)
    }

    pub(crate) fn proxy_path(&self) -> PathBuf {
        self.dir.join(PROXY_FILE)
    }

    /// The home, 0700 and ours (never through a symlink).
    pub(crate) fn create(&self) -> io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let meta = fs::symlink_metadata(&self.dir)?;
        if !meta.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory",
                self.dir.display()
            )));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

/// `proxy.json`: what the companion's `--remote` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProxyFile {
    pub(crate) port: u16,
    pub(crate) secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
}

fn secret_ok(secret: &str) -> bool {
    !secret.is_empty()
        && secret.len() <= SECRET_MAX
        && secret.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The file as the companion would take it, or `None`.
pub(crate) fn read_proxy_file(home: &MachineHome) -> Option<ProxyFile> {
    let file: ProxyFile = serde_json::from_slice(&fs::read(home.proxy_path()).ok()?).ok()?;
    (file.port != 0 && secret_ok(&file.secret)).then_some(file)
}

/// 0600, written whole (temp file and rename).
pub(crate) fn write_proxy_file(home: &MachineHome, file: &ProxyFile) -> io::Result<()> {
    let json = serde_json::to_vec(file).map_err(io::Error::other)?;
    let tmp = home
        .dir()
        .join(format!(".{PROXY_FILE}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        out.write_all(&json)?;
        out.sync_all()?;
        fs::rename(&tmp, home.proxy_path())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// A new proxy secret: 64 hex digits from the system's random source.
pub(crate) fn new_secret() -> io::Result<String> {
    let mut bytes = [0u8; SECRET_BYTES];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `app` is `REMOTE_APP_MIN` or newer (an unreadable version is not).
fn app_supports_remote(app: &str) -> bool {
    let parse = |v: &str| -> Option<Vec<u64>> {
        v.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse().ok())
            .collect()
    };
    match (parse(app), parse(REMOTE_APP_MIN)) {
        (Some(app), Some(min)) => app >= min,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Seams

/// The proxy's port and secret, fixed for a companion's life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyGrant {
    pub(crate) port: u16,
    pub(crate) secret: String,
}

/// The helper's proxy (C4 starts `proxy::Proxy` with it).
pub(crate) trait ProxyBinder: Send + Sync {
    /// Listen on 127.0.0.1:`port` (0: any free port) with `secret`; the port
    /// it listens on. A later call replaces the listener (after an adoption
    /// whose port could not come back, and after a shutdown).
    fn bind(&self, port: u16, secret: &str) -> io::Result<u16>;
}

/// The sidecar as the companion needs it ([`super::sidecar::Sidecar`]).
pub(crate) trait CompanionLink: Send + Sync {
    /// Attach to the registering companion (its own CDP port and nonce);
    /// what the sidecar answered (its tabs).
    fn attach(&self, port: u16, nonce: &str) -> Result<Value, BrowserError>;
    /// The control page's quit, sent and not waited for (the exit answers).
    fn quit(&self);
    /// The companion's process is gone: forget its port and nonce.
    fn gone(&self);
}

/// What the helper learns from the companion (C4).
pub(crate) trait CompanionHooks: Send + Sync {
    /// The state a client shows changed ([`Companion::info`]).
    fn changed(&self);
    /// The process went away; `reason` for the remote machine's log
    /// (`host_down`).
    fn exited(&self, reason: &str);
}

/// What a companion is started with.
#[derive(Debug, Clone)]
pub(crate) struct CompanionOptions {
    /// `<browser home>/companion-app`: Electron and the app files (read here, never written).
    pub(crate) companion_home: PathBuf,
    /// The machine's key (`ssh::machine_key`).
    pub(crate) key: String,
    /// The machine's name on the companion's surfaces.
    pub(crate) label: String,
    /// The helper's API socket for it (`<key>.api`).
    pub(crate) api_socket: PathBuf,
}

/// Why `ensure` or `start` has no browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnsureError {
    NotInstalled,
    Failed(String),
}

impl EnsureError {
    /// `ensure_re`'s error; `mac` names this Mac.
    pub(crate) fn link_error(&self, mac: &str) -> LinkError {
        LinkError::new(
            "host_unavailable",
            match self {
                Self::NotInstalled => text::not_installed(mac),
                Self::Failed(detail) => text::start_failed(mac, detail),
            },
        )
    }
}

/// What the helper's start found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Recovered {
    /// No companion of this home lives.
    Nothing,
    /// The recorded companion, with its port and secret.
    Adopted { pid: u32 },
    /// Its port could not come back: it was stopped and a new one starts.
    Relaunched,
    /// A live pid nothing tells apart from a reused one, or one that would
    /// not end: left alone.
    LeftAlone,
}

// ---------------------------------------------------------------------------
// The companion

struct State {
    core: Core,
    grant: Option<ProxyGrant>,
    /// The current registration was made or renewed then.
    lease_at: Option<Instant>,
    /// The generation whose process has a waiter (our own child).
    child_generation: Option<u64>,
    /// A stop runs: no launch, and only adopted pids are polled.
    stopping: bool,
    /// Shut down or dropped: the driver ends.
    closed: bool,
    driver: bool,
}

impl State {
    fn lease_live(&self, now: Instant) -> bool {
        self.core.registered
            && self
                .lease_at
                .is_some_and(|at| now.saturating_duration_since(at) < LEASE_TTL)
    }

    fn has_child(&self) -> bool {
        self.child_generation == Some(self.core.generation)
    }
}

struct Inner {
    options: CompanionOptions,
    home: MachineHome,
    spawner: Arc<dyn Spawner>,
    binder: Arc<dyn ProxyBinder>,
    link: Arc<dyn CompanionLink>,
    hooks: Arc<dyn CompanionHooks>,
    state: Mutex<State>,
    changed: Condvar,
}

/// One machine's companion. Dropping it leaves a running process alone (the
/// next helper adopts it); [`Companion::shutdown`] ends it.
pub(crate) struct Companion {
    inner: Arc<Inner>,
}

impl Drop for Companion {
    fn drop(&mut self) {
        self.inner.lock().closed = true;
        self.inner.changed.notify_all();
    }
}

impl Companion {
    /// The machine's home is made (0700) now; nothing starts.
    pub(crate) fn new(
        options: CompanionOptions,
        spawner: Arc<dyn Spawner>,
        binder: Arc<dyn ProxyBinder>,
        link: Arc<dyn CompanionLink>,
        hooks: Arc<dyn CompanionHooks>,
    ) -> io::Result<Self> {
        let home = MachineHome::new(&options.companion_home, &options.key);
        home.create()?;
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                home,
                spawner,
                binder,
                link,
                hooks,
                state: Mutex::new(State {
                    core: Core::new(unix_now()),
                    grant: None,
                    lease_at: None,
                    child_generation: None,
                    stopping: false,
                    closed: false,
                    driver: false,
                }),
                changed: Condvar::new(),
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn home(&self) -> &MachineHome {
        &self.inner.home
    }

    /// The proxy's port and secret, once bound.
    #[cfg(test)]
    pub(crate) fn grant(&self) -> Option<ProxyGrant> {
        self.inner.lock().grant.clone()
    }

    /// The helper's start: adopt the recorded companion with its port and
    /// secret, or clear the record, or stop it and start a new one.
    pub(crate) fn recover(&self) -> Recovered {
        self.inner.recover()
    }

    /// The remote side needs the browser: start it unless it runs, and wait
    /// for its registration, at most `wait`. On the caller's thread.
    pub(crate) fn ensure(&self, wait: Duration) -> Result<(), EnsureError> {
        self.inner.ensure(wait)
    }

    /// The user's start (the backoff and `failed` are reset); answers at once.
    pub(crate) fn start(&self, reason: StartReason) -> Result<(), EnsureError> {
        self.inner.start(reason)
    }

    /// The companion's `browser.host_register`: only the process this
    /// helper launched or adopted (its launch token); the sidecar attaches
    /// to it. What the sidecar answered (for `host_up`).
    pub(crate) fn register(
        &self,
        params: &BrowserHostRegisterParams,
    ) -> Result<Value, BrowserError> {
        self.inner.register(params)
    }

    /// The registration of `token` was renewed; `false`: it is not the current one.
    pub(crate) fn heartbeat(&self, token: &str) -> bool {
        let mut state = self.inner.lock();
        let current = state.core.registered && state.core.token.as_deref() == Some(token);
        if current {
            state.lease_at = Some(Instant::now());
        }
        current
    }

    /// The process of `token` unregistered: it is quitting on its own.
    pub(crate) fn unregistered(&self, token: &str) {
        {
            let mut state = self.inner.lock();
            if state.core.token.as_deref() == Some(token) {
                state.core.unregistered(Some(token));
                state.lease_at = None;
            }
        }
        self.inner.notify();
    }

    /// A registration lives.
    pub(crate) fn live(&self) -> bool {
        self.inner.lock().lease_live(Instant::now())
    }

    /// Stop it (the control page's quit, SIGTERM, SIGKILL); blocking. A
    /// later `ensure` starts it again.
    pub(crate) fn stop(&self) {
        self.inner.stop(false);
        self.inner.unblock();
    }

    /// Stop, then start again for the user.
    pub(crate) fn restart(&self) -> Result<(), EnsureError> {
        self.stop();
        self.start(StartReason::User)
    }

    /// Forget the machine's logins: stop, then remove its profile (renamed
    /// away first). Nothing outside the home is touched; the next `ensure`
    /// starts a signed-out companion.
    pub(crate) fn forget(&self) -> io::Result<()> {
        let gone = self.inner.stop(false);
        let result = if gone {
            forget_profile(&self.inner.home)
        } else {
            Err(io::Error::other(
                "the companion did not exit; its logins were kept",
            ))
        };
        self.inner.unblock();
        result
    }

    /// The helper exits: the companion quits, nothing starts again.
    pub(crate) fn shutdown(&self) {
        self.inner.stop(true);
        self.inner.lock().closed = true;
        self.inner.notify();
    }

    /// Whether an installed companion app can run for a remote machine
    /// (the helper's `ready` on its link).
    pub(crate) fn install_check(&self) -> Result<(), EnsureError> {
        installed(&self.inner.options.companion_home).map(|_| ())
    }

    /// The companion as the clients show it.
    pub(crate) fn info(&self) -> BrowserCompanionAppInfo {
        self.inner.info()
    }

    /// One driver pass at `now` (crash restarts, the register deadline, the
    /// lease rule, adopted pids); the driver thread runs it.
    #[cfg(test)]
    pub(crate) fn tick(&self, now: Instant) {
        self.inner.tick(now);
    }

    /// The process of `generation` ended with `exit` (its waiter).
    #[cfg(test)]
    pub(crate) fn process_exited(&self, generation: u64, exit: ExitInfo) {
        self.inner.process_exited(generation, exit);
    }

    /// The current launch: its generation and token.
    #[cfg(test)]
    pub(crate) fn current(&self) -> (u64, Option<String>) {
        let state = self.inner.lock();
        (state.core.generation, state.core.token.clone())
    }
}

fn forget_profile(home: &MachineHome) -> io::Result<()> {
    let profile = home.profile_dir();
    if fs::symlink_metadata(&profile).is_err() {
        return Ok(());
    }
    let away = home.dir().join(format!(
        "profile.forgotten-{}",
        crate::browser::session::random_hex(8)
    ));
    fs::rename(&profile, &away)?;
    fs::remove_dir_all(&away)
}

/// The install a remote companion can run, or why there is none.
fn installed(companion_home: &Path) -> Result<CompanionInstall, EnsureError> {
    let install = companion_install::resolve(companion_home).map_err(|problem| match problem {
        InstallProblem::NotInstalled | InstallProblem::NoElectron => EnsureError::NotInstalled,
        InstallProblem::Unsupported(why) => EnsureError::Failed(why),
    })?;
    if !app_supports_remote(&install.record.app) {
        return Err(EnsureError::Failed(format!(
            "its companion app is {}; remote machines need {REMOTE_APP_MIN} or newer: run \"herdr browser setup --companion\" there",
            if install.record.app.is_empty() { "unknown" } else { &install.record.app }
        )));
    }
    Ok(install)
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, State>, timeout: Duration) -> MutexGuard<'a, State> {
        match self.changed.wait_timeout(guard, timeout) {
            Ok((guard, _)) => guard,
            Err(poisoned) => poisoned.into_inner().0,
        }
    }

    fn notify(&self) {
        self.changed.notify_all();
        self.hooks.changed();
    }

    fn ensure(self: &Arc<Self>, wait: Duration) -> Result<(), EnsureError> {
        let deadline = Instant::now() + wait;
        let mut launched: Option<u64> = None;
        loop {
            let now = Instant::now();
            let (phase, spawning, generation, last_error, blocked, crashes) = {
                let state = self.lock();
                if state.lease_live(now) {
                    return Ok(());
                }
                (
                    state.core.phase,
                    state.core.spawning,
                    state.core.generation,
                    state.core.last_error.clone(),
                    state.closed || state.stopping || state.core.shutting_down,
                    state.core.crashes_in_window(now) as usize,
                )
            };
            if blocked {
                return Err(EnsureError::Failed("it is stopping".into()));
            }
            match phase {
                // five crashes, or one that a restart would repeat (exit 2)
                Phase::Failed if crashes < companion_app::CRASH_LIMIT => {
                    return Err(EnsureError::Failed(
                        last_error.unwrap_or_else(|| "it failed".into()),
                    ))
                }
                Phase::Failed => {
                    return Err(EnsureError::Failed(format!(
                        "it crashed {} times in 5 minutes; last error: {}",
                        companion_app::CRASH_LIMIT,
                        last_error.as_deref().unwrap_or("unknown")
                    )))
                }
                Phase::Stopped if !spawning => {
                    if launched == Some(generation) {
                        return Err(EnsureError::Failed(last_error.unwrap_or_else(|| {
                            format!(
                                "it exited before it registered; log: {}",
                                self.home.log_path().display()
                            )
                        })));
                    }
                    installed(&self.options.companion_home)?;
                    let ticket = self.lock().core.begin_spawn(
                        StartReason::OnDemand,
                        Instant::now(),
                        unix_now(),
                    );
                    if let Some(ticket) = ticket {
                        launched = Some(ticket.generation);
                        self.notify();
                        self.launch(ticket);
                        continue;
                    }
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EnsureError::Failed(format!(
                    "it did not start within {} s; log: {}",
                    wait.as_secs(),
                    self.home.log_path().display()
                )));
            }
            let state = self.lock();
            if state.core.generation == generation && state.core.phase == phase {
                drop(self.wait(state, (deadline - now).min(Duration::from_millis(250))));
            }
        }
    }

    fn start(self: &Arc<Self>, reason: StartReason) -> Result<(), EnsureError> {
        installed(&self.options.companion_home)?;
        let ticket = {
            let mut state = self.lock();
            if state.closed || state.stopping || state.core.shutting_down {
                return Err(EnsureError::Failed("it is stopping".into()));
            }
            state.core.reset_backoff(unix_now());
            state.core.begin_spawn(reason, Instant::now(), unix_now())
        };
        self.notify();
        if let Some(ticket) = ticket {
            let inner = Arc::clone(self);
            std::thread::Builder::new()
                .name("browser-link-companion-start".into())
                .spawn(move || inner.launch(ticket))
                .map_err(|err| EnsureError::Failed(err.to_string()))?;
        }
        Ok(())
    }

    /// The proxy's grant: the one of this helper's life, else a new port and secret.
    fn grant(&self) -> io::Result<ProxyGrant> {
        if let Some(grant) = self.lock().grant.clone() {
            return Ok(grant);
        }
        let secret = new_secret()?;
        let port = self.binder.bind(0, &secret)?;
        let grant = ProxyGrant { port, secret };
        self.lock().grant = Some(grant.clone());
        Ok(grant)
    }

    /// Run a launch on this thread: install, proxy, `proxy.json`, spawn,
    /// record, watch.
    fn launch(self: &Arc<Self>, ticket: SpawnTicket) {
        let generation = ticket.generation;
        let fail = |error: String| {
            tracing::warn!(event = "browser.link.companion.launch", %error, "remote companion launch failed");
            self.lock()
                .core
                .spawn_failed(generation, error, Instant::now(), unix_now());
            self.notify();
            self.ensure_driver();
        };
        let install = match installed(&self.options.companion_home) {
            Ok(install) => install,
            Err(EnsureError::NotInstalled) => {
                return fail("the companion browser is not installed".into())
            }
            Err(EnsureError::Failed(why)) => return fail(why),
        };
        let grant = match self.grant() {
            Ok(grant) => grant,
            Err(err) => return fail(format!("the browser's proxy did not start: {err}")),
        };
        let file = ProxyFile {
            port: grant.port,
            secret: grant.secret,
            label: Some(self.options.label.clone()).filter(|label| !label.is_empty()),
        };
        if let Err(err) = write_proxy_file(&self.home, &file) {
            return fail(format!(
                "{} not written: {err}",
                self.home.proxy_path().display()
            ));
        }
        let home = self.home.dir().display().to_string();
        let mut args = vec!["--managed".to_string()];
        if ticket.background {
            args.push("--background".into());
        }
        args.extend([
            "--launch-token".to_string(),
            ticket.token.clone(),
            "--socket".into(),
            self.options.api_socket.display().to_string(),
            "--profile-dir".into(),
            self.home.profile_dir().display().to_string(),
            "--remote".into(),
            home,
        ]);
        let spec = LaunchSpec {
            executable: install.executable.clone(),
            app_dir: install.app_dir.clone(),
            args,
            log: self.home.log_path(),
        };
        if !self.lock().core.launch_goes_ahead(generation, unix_now()) {
            self.notify();
            return;
        }
        let spawned = match self.spawner.spawn(&spec) {
            Ok(spawned) => spawned,
            Err(err) => {
                return fail(format!(
                    "could not start {}: {err}",
                    spec.executable.display()
                ))
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
            start_token: self
                .spawner
                .facts(pid)
                .and_then(|facts| facts.start_token)
                .unwrap_or(0),
            server_pid: std::process::id(),
            socket: self.options.api_socket.display().to_string(),
            background: ticket.background,
        };
        if let Err(err) = companion_app::write_run_record(self.home.dir(), &record) {
            tracing::warn!(event = "browser.link.companion.run_record", error = %err, "remote companion run record not written; a restarted helper cannot adopt it");
        }
        let has_child = spawned.child.is_some();
        {
            let mut state = self.lock();
            if state
                .core
                .spawned(generation, pid, record, has_child, unix_now())
                && has_child
            {
                state.child_generation = Some(generation);
            }
        }
        tracing::info!(
            event = "browser.link.companion.spawn",
            pid,
            background = ticket.background,
            "remote companion started"
        );
        if let Some(mut child) = spawned.child {
            let weak = Arc::downgrade(self);
            let _ = std::thread::Builder::new()
                .name("browser-link-companion-wait".into())
                .spawn(move || {
                    let exit = child
                        .wait()
                        .map(ExitInfo::from_status)
                        .unwrap_or_else(|_| ExitInfo::unknown());
                    if let Some(inner) = weak.upgrade() {
                        inner.process_exited(generation, exit);
                    }
                });
        }
        self.notify();
        self.ensure_driver();
    }

    fn register(&self, params: &BrowserHostRegisterParams) -> Result<Value, BrowserError> {
        let token = params
            .launch_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty());
        let current = |state: &State| {
            token.is_some()
                && token == state.core.token.as_deref()
                && (state.core.pid.is_some() || state.core.spawning)
        };
        {
            let state = self.lock();
            if state.closed || state.stopping {
                return Err(BrowserError::new(
                    "host_unavailable",
                    "this browser is stopping",
                ));
            }
            if !current(&state) {
                return Err(BrowserError::new(
                    "host_in_use",
                    "this socket serves the companion herdr started for this machine; quit this one",
                ));
            }
        }
        let answer = self.link.attach(params.cdp.port, &params.nonce)?;
        {
            let mut state = self.lock();
            if !current(&state) || state.stopping {
                return Err(BrowserError::new(
                    "host_unavailable",
                    "this browser is stopping",
                ));
            }
            state.core.registered(token, unix_now());
            state.lease_at = Some(Instant::now());
        }
        self.notify();
        Ok(answer)
    }

    fn recover(self: &Arc<Self>) -> Recovered {
        let Some(record) = companion_app::read_run_record(self.home.dir()) else {
            return Recovered::Nothing;
        };
        match self.record_process(&record) {
            RecordedProcess::Gone => {
                companion_app::clear_run_record(self.home.dir(), Some(&record.launch_token));
                return Recovered::Nothing;
            }
            RecordedProcess::Unknown => {
                tracing::warn!(
                    event = "browser.link.companion.adopt",
                    pid = record.pid,
                    "the recorded companion cannot be told from a reused pid; left alone"
                );
                return Recovered::LeftAlone;
            }
            RecordedProcess::Same => {}
        }
        let pid = record.pid;
        let same_socket = record.socket == self.options.api_socket.display().to_string();
        let grant = read_proxy_file(&self.home).filter(|_| same_socket).and_then(|file| {
            match self.binder.bind(file.port, &file.secret) {
                Ok(port) if port == file.port => Some(ProxyGrant {
                    port,
                    secret: file.secret,
                }),
                Ok(port) => {
                    tracing::warn!(event = "browser.link.companion.adopt", want = file.port, port, "the proxy came back on another port");
                    None
                }
                Err(err) => {
                    tracing::warn!(event = "browser.link.companion.adopt", port = file.port, error = %err, "the companion's proxy port cannot come back");
                    None
                }
            }
        });
        {
            let mut state = self.lock();
            if state.core.busy() || state.closed {
                return Recovered::LeftAlone;
            }
            state.core.adopt(record, Instant::now(), unix_now());
            state.child_generation = None;
            state.lease_at = None;
            if grant.is_some() {
                state.grant = grant.clone();
            }
        }
        self.notify();
        self.ensure_driver();
        if grant.is_some() {
            tracing::info!(
                event = "browser.link.companion.adopt",
                pid,
                "adopted the machine's running companion"
            );
            return Recovered::Adopted { pid };
        }
        // its proxy cannot come back with that port and secret: a new one
        if !self.stop(false) {
            self.unblock();
            return Recovered::LeftAlone;
        }
        self.unblock();
        match self.start(StartReason::OnDemand) {
            Ok(()) => Recovered::Relaunched,
            Err(err) => {
                tracing::warn!(event = "browser.link.companion.adopt", error = ?err, "the replacement companion did not start");
                Recovered::Relaunched
            }
        }
    }

    /// What a run record's pid is now.
    fn record_process(&self, record: &CompanionRunRecord) -> RecordedProcess {
        let facts = (record.pid != 0)
            .then(|| self.spawner.facts(record.pid))
            .flatten();
        companion_app::match_record(record, facts.as_ref())
    }

    /// The process of `generation`, which has no waiter here: what its run
    /// record's pid is now (`Gone` once it is not current).
    fn adopted_process(&self, generation: u64, pid: u32) -> RecordedProcess {
        let record = {
            let state = self.lock();
            (state.core.generation == generation)
                .then(|| state.core.run.clone())
                .flatten()
        };
        match record {
            Some(record) if record.pid == pid => self.record_process(&record),
            _ => RecordedProcess::Gone,
        }
    }

    fn process_exited(&self, generation: u64, exit: ExitInfo) {
        let tail = companion_app::log_tail(&self.home.log_path(), 3);
        let token = {
            let state = self.lock();
            (state.core.generation == generation
                && (state.core.pid.is_some() || state.core.spawning))
                .then(|| state.core.token.clone())
                .flatten()
        };
        if let Some(token) = token.as_deref() {
            companion_app::clear_run_record(self.home.dir(), Some(token));
        }
        let (current, phase) = {
            let mut state = self.lock();
            let current = state
                .core
                .exited(generation, &exit, &tail, Instant::now(), unix_now());
            if current {
                state.lease_at = None;
                state.child_generation = None;
                if exit.known
                    && exit.code == Some(BAD_FLAGS_EXIT)
                    && state.core.phase == Phase::Crashed
                {
                    // its flags or proxy.json: a restart would fail the same way
                    state.core.phase = Phase::Failed;
                    state.core.since_unix = unix_now();
                    state.core.next_retry_unix = None;
                    state.core.last_error = Some(format!(
                        "the companion refused its flags or {PROXY_FILE} (exit {BAD_FLAGS_EXIT}){}",
                        if tail.is_empty() {
                            String::new()
                        } else {
                            format!(" — {}", tail.join(" | "))
                        }
                    ));
                }
            }
            (current, state.core.phase)
        };
        if current {
            self.link.gone();
            self.hooks.exited(&exit.describe());
            tracing::info!(event = "browser.link.companion.exit", status = %exit.describe(), phase = phase.as_str(), "remote companion exited");
        }
        self.notify();
    }

    /// Stop the process: the control page's quit while it is registered,
    /// else a short grace; then SIGTERM, then SIGKILL. `true` once no
    /// process lives. Launches stay blocked until [`Self::unblock`].
    fn stop(&self, final_stop: bool) -> bool {
        {
            let mut state = self.lock();
            state.core.shutting_down = true;
            state.stopping = true;
            if state.core.phase == Phase::Crashed {
                state.core.reset_backoff(unix_now());
            }
        }
        self.notify();
        // a launch in flight is called off or spawns first
        let until = Instant::now() + LAUNCH_WAIT;
        let mut state = self.lock();
        while state.core.spawning && Instant::now() < until {
            state = self.wait(state, Duration::from_millis(100));
        }
        let target = state.core.pid.map(|pid| {
            (
                pid,
                state.core.generation,
                state.has_child(),
                state.core.token.clone(),
                state.lease_live(Instant::now()),
            )
        });
        drop(state);
        let mut gone = true;
        if let Some((pid, generation, has_child, token, attached)) = target {
            // its exit is a stop, not a crash
            self.lock().core.unregistered(token.as_deref());
            if attached {
                self.link.quit();
            }
            gone = self.wait_gone(
                generation,
                pid,
                has_child,
                if attached { QUIT_WAIT } else { UNATTACHED_WAIT },
            ) || (self.signal(pid, generation, has_child, StopSignal::Term)
                && self.wait_gone(generation, pid, has_child, TERM_WAIT))
                || (self.signal(pid, generation, has_child, StopSignal::Kill)
                    && self.wait_gone(generation, pid, has_child, KILL_WAIT));
            if !gone {
                tracing::warn!(
                    event = "browser.link.companion.stop",
                    pid,
                    "remote companion did not exit after SIGKILL"
                );
                self.lock().core.abandon(
                    generation,
                    format!("pid {pid} did not exit after SIGKILL"),
                    unix_now(),
                );
                companion_app::clear_run_record(self.home.dir(), token.as_deref());
            }
        }
        if gone && self.lock().core.pid.is_none() {
            let _ = fs::remove_file(self.home.proxy_path());
        }
        {
            let mut state = self.lock();
            state.stopping = false;
            state.lease_at = None;
            if final_stop {
                state.grant = None;
            }
        }
        self.notify();
        gone
    }

    /// Launches may begin again (not after a shutdown).
    fn unblock(&self) {
        {
            let mut state = self.lock();
            if !state.closed {
                state.core.shutting_down = false;
            }
        }
        self.notify();
    }

    /// Signal the process of `generation`; one without a waiter here only
    /// while its pid is still the process its run record names. `false`: it
    /// is gone (its exit is reported here).
    fn signal(&self, pid: u32, generation: u64, has_child: bool, signal: StopSignal) -> bool {
        if !has_child {
            match self.adopted_process(generation, pid) {
                RecordedProcess::Same => {}
                RecordedProcess::Gone => {
                    self.process_exited(generation, ExitInfo::unknown());
                    return false;
                }
                RecordedProcess::Unknown => {
                    tracing::warn!(
                        event = "browser.link.companion.signal",
                        pid,
                        ?signal,
                        "the companion's pid cannot be told from a reused one; not signalled"
                    );
                    return true;
                }
            }
        }
        self.spawner.signal(pid, signal);
        true
    }

    /// Wait until the process of `generation` is gone, at most `within`.
    fn wait_gone(&self, generation: u64, pid: u32, has_child: bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let gone = |state: &State| state.core.generation != generation || state.core.pid.is_none();
        loop {
            if gone(&self.lock()) {
                return true;
            }
            if !has_child && self.adopted_process(generation, pid) == RecordedProcess::Gone {
                self.process_exited(generation, ExitInfo::unknown());
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let state = self.lock();
            if gone(&state) {
                return true;
            }
            drop(self.wait(state, (deadline - now).min(Duration::from_millis(100))));
        }
    }

    fn tick(self: &Arc<Self>, now: Instant) {
        let actions = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            if state.stopping {
                // the stop's own waits see the exit; an adopted pid is polled
                match state.core.pid {
                    Some(pid) if !state.has_child() => vec![Action::Poll {
                        pid,
                        generation: state.core.generation,
                    }],
                    _ => Vec::new(),
                }
            } else {
                let live = state.lease_live(now);
                state.core.due(now, live)
            }
        };
        for action in actions {
            match action {
                Action::Respawn => {
                    let ticket = self.lock().core.begin_spawn(
                        StartReason::CrashRestart,
                        Instant::now(),
                        unix_now(),
                    );
                    if let Some(ticket) = ticket {
                        self.notify();
                        self.launch(ticket);
                    }
                }
                Action::Signal {
                    pid,
                    generation,
                    signal,
                } => {
                    let (current, has_child) = {
                        let state = self.lock();
                        (
                            state.core.generation == generation && state.core.pid == Some(pid),
                            state.has_child(),
                        )
                    };
                    if current {
                        tracing::warn!(
                            event = "browser.link.companion.kill",
                            pid,
                            ?signal,
                            "stopping an unhealthy remote companion"
                        );
                        self.signal(pid, generation, has_child, signal);
                    }
                }
                Action::Poll { pid, generation } => {
                    if self.adopted_process(generation, pid) == RecordedProcess::Gone {
                        self.process_exited(generation, ExitInfo::unknown());
                    }
                }
            }
        }
    }

    /// One driver thread per companion, from the first launch or adoption;
    /// it ends with the companion.
    fn ensure_driver(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.driver || state.closed {
                return;
            }
            state.driver = true;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let started = std::thread::Builder::new()
            .name("browser-link-companion".into())
            .spawn(move || loop {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                {
                    let mut state = inner.lock();
                    if state.closed {
                        state.driver = false;
                        return;
                    }
                }
                inner.tick(Instant::now());
                let state = inner.lock();
                let wait = state.core.next_wake(Instant::now()).min(WATCH_INTERVAL);
                drop(inner.wait(state, wait));
            });
        if started.is_err() {
            self.lock().driver = false;
        }
    }

    fn info(&self) -> BrowserCompanionAppInfo {
        let install = companion_install::read_install(&self.options.companion_home);
        let state = self.lock();
        let core = &state.core;
        let name = if state.stopping {
            "stopping".to_string()
        } else if core.phase == Phase::Stopped && !core.busy() && install.is_none() {
            "not_installed".to_string()
        } else {
            core.phase.as_str().to_string()
        };
        BrowserCompanionAppInfo {
            state: name,
            managed: true,
            pid: core.pid,
            version: core
                .run
                .as_ref()
                .map(|run| run.app.clone())
                .filter(|v| !v.is_empty()),
            installed: install.as_ref().map(|i| i.app.clone()),
            electron: install.as_ref().map(|i| i.electron.clone()),
            update_ready: false,
            update: None,
            started_unix: core.started_unix,
            since_unix: Some(core.since_unix),
            restarts: core.crashes_in_window(Instant::now()),
            next_retry_unix: core
                .next_retry_unix
                .filter(|_| core.phase == Phase::Crashed),
            last_error: core.last_error.clone(),
            autostart: false,
            log: Some(self.home.log_path().display().to_string()),
        }
    }
}

#[cfg(test)]
mod tests;
