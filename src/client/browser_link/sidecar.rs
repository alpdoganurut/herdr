//! The sidecar of a machine's companion: `node host.mjs` on this Mac, a child
//! of the helper on stdio, attached to that companion with `remote: true`
//! (which only the helper sends), its own CDP port and nonce, profile
//! `companion`.
//!
//! JSON lines as the server's sidecar speaks them (`browser::host`). Replies
//! go to their callers through one channel each, so the reader never waits
//! on a caller; an answer line can be about 45 MB (a screenshot in base64).
//! Events pass on as they came. Two missed pings in a row: the sidecar is
//! wedged, its group is killed and a new one attaches to the same companion.
//! It ends with its stdin.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::companion::CompanionLink;
use super::gate::GatedOp;
use crate::browser::companion_app::{BACKOFF_SECS, CRASH_LIMIT, CRASH_WINDOW};
use crate::browser::host::{self, HostReply, HostRequest, HostWriter, HOST_PROTOCOL};
use crate::browser::hub::{
    ATTACH_TIMEOUT, COMPANION_PROFILE, PING_MISSES, PING_TIMEOUT, REPLY_GRACE, SUPERVISOR_INTERVAL,
};
use crate::browser::BrowserError;
use crate::integration::browser_assets;

/// The longest line read from the sidecar; a longer one is dropped.
pub(crate) const LINE_MAX: usize = 64 * 1024 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// The control page's quit is sent with this deadline (and not waited for).
const QUIT_DEADLINE: Duration = Duration::from_secs(3);
/// After its stdin closed, the sidecar gets this long to exit.
const EXIT_WAIT: Duration = Duration::from_secs(3);

/// A started sidecar process (or a test's pipes).
pub(crate) struct SidecarProcess {
    pub(crate) pid: u32,
    /// `None` for a process that is not this one's child (tests).
    pub(crate) child: Option<Child>,
    pub(crate) writer: Box<dyn HostWriter>,
    pub(crate) reader: Box<dyn BufRead + Send>,
}

/// Starting and killing sidecars (a fake in tests).
pub(crate) trait SidecarSpawn: Send + Sync {
    fn spawn(&self) -> io::Result<SidecarProcess>;
    /// End a wedged one (its process group).
    fn kill(&self, pid: u32);
}

/// Why the installed sidecar cannot run here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidecarSetupError {
    Missing,
    Outdated(String),
    NoNode,
}

impl SidecarSetupError {
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Missing => "the browser helper is not installed; run \"herdr browser setup\"".into(),
            Self::Outdated(why) => format!("the browser helper is outdated ({why}); run \"herdr browser setup\""),
            Self::NoNode => "the browser helper needs node, and none was found; install Node.js 22+ or set [browser] node".into(),
        }
    }
}

/// The installed sidecar: `node <host dir>/host.mjs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeSidecar {
    node: PathBuf,
    host_dir: PathBuf,
}

impl NodeSidecar {
    pub(crate) fn new(node: PathBuf, host_dir: PathBuf) -> Self {
        Self { node, host_dir }
    }

    /// The sidecar `herdr browser setup` installed under `browser_home`, run
    /// with `configured_node` (`[browser] node`) or the recorded one. Its
    /// scripts are refreshed in place to this herdr's, as the server does
    /// (node_modules is never touched): an older `host.mjs` would ignore
    /// `remote`.
    pub(crate) fn resolve(
        browser_home: &Path,
        configured_node: Option<&str>,
    ) -> Result<Self, SidecarSetupError> {
        let host_dir = browser_home.join(browser_assets::HOST_DIR);
        let runtime = browser_assets::read_runtime(&host_dir).ok_or(SidecarSetupError::Missing)?;
        let installed =
            browser_assets::playwright_installed(&host_dir).ok_or(SidecarSetupError::Missing)?;
        if installed != browser_assets::PLAYWRIGHT_CORE_VERSION {
            return Err(SidecarSetupError::Outdated(format!(
                "playwright-core {installed}, this herdr needs {}",
                browser_assets::PLAYWRIGHT_CORE_VERSION
            )));
        }
        if runtime.assets_sha256 != browser_assets::assets_sha256() {
            browser_assets::install(&host_dir).map_err(|err| {
                SidecarSetupError::Outdated(format!("its scripts could not be refreshed: {err}"))
            })?;
            let mut runtime = runtime.clone();
            runtime.assets_sha256 = browser_assets::assets_sha256();
            let _ = browser_assets::write_runtime(&host_dir, &runtime);
        }
        let node =
            crate::browser::node::discover_default(configured_node, Some(Path::new(&runtime.node)))
                .ok_or(SidecarSetupError::NoNode)?;
        Ok(Self::new(node.path, host_dir))
    }
}

impl SidecarSpawn for NodeSidecar {
    fn spawn(&self) -> io::Result<SidecarProcess> {
        let spawned = host::spawn(&self.node, &self.host_dir, browser_assets::ENTRY)?;
        Ok(SidecarProcess {
            pid: spawned.pid,
            child: Some(spawned.child),
            writer: spawned.writer,
            reader: spawned.reader,
        })
    }

    fn kill(&self, pid: u32) {
        host::kill_group(pid);
    }
}

/// What the helper learns from the sidecar (C4).
pub(crate) trait SidecarHooks: Send + Sync {
    /// A sidecar event line, as it came (`ev{event}`).
    fn event(&self, event: Value);
    /// It attached to the companion; its answer (the tabs, for `host_up`).
    fn attached(&self, answer: &Value);
    /// The attached sidecar went away (`host_down`); a new one attaches by
    /// itself when it can.
    fn lost(&self, reason: &str);
}

/// The supervisor's pace and the reader's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidecarOptions {
    pub(crate) ping_every: Duration,
    pub(crate) ping_timeout: Duration,
    /// Missed in a row before it is restarted.
    pub(crate) misses: u32,
    pub(crate) line_max: usize,
}

impl Default for SidecarOptions {
    fn default() -> Self {
        Self {
            ping_every: SUPERVISOR_INTERVAL,
            ping_timeout: PING_TIMEOUT,
            misses: PING_MISSES,
            line_max: LINE_MAX,
        }
    }
}

struct Link {
    generation: u64,
    pid: u32,
    child: Option<Child>,
    writer: Arc<Mutex<Box<dyn HostWriter>>>,
    misses: u32,
}

#[derive(Default)]
struct State {
    link: Option<Link>,
    generation: u64,
    next_id: u64,
    /// The companion's CDP port and nonce while it is attached or to be re-attached.
    companion: Option<(u16, String)>,
    attached: bool,
    drops: VecDeque<Instant>,
    retry_at: Option<Instant>,
    /// Gave up restarting it (five drops in five minutes); an attach tries again.
    failed: Option<String>,
    closed: bool,
    supervisor: bool,
}

impl State {
    /// A sidecar went away unasked at `now`: the next attach waits its
    /// backoff; five in five minutes and no more are started by themselves.
    fn note_drop(&mut self, why: &str, now: Instant) {
        while self
            .drops
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= CRASH_WINDOW)
        {
            self.drops.pop_front();
        }
        self.drops.push_back(now);
        if self.drops.len() >= CRASH_LIMIT {
            self.failed = Some(why.to_string());
            self.retry_at = None;
        } else {
            let delay = BACKOFF_SECS[(self.drops.len() - 1).min(BACKOFF_SECS.len() - 1)];
            self.retry_at = Some(now + Duration::from_secs(delay));
        }
    }
}

type Pending = HashMap<u64, mpsc::Sender<HostReply>>;

struct Inner {
    spawner: Arc<dyn SidecarSpawn>,
    hooks: Arc<dyn SidecarHooks>,
    options: SidecarOptions,
    state: Mutex<State>,
    pending: Mutex<Pending>,
    /// One start at a time.
    starting: Mutex<()>,
    changed: Condvar,
}

/// A machine's sidecar. Started by the first attach; [`Sidecar::shutdown`]
/// (or dropping it) closes its stdin.
pub(crate) struct Sidecar {
    inner: Arc<Inner>,
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

fn gone_error() -> BrowserError {
    BrowserError::host_failed("the browser sidecar went away")
}

impl Sidecar {
    pub(crate) fn new(
        spawner: Arc<dyn SidecarSpawn>,
        hooks: Arc<dyn SidecarHooks>,
        options: SidecarOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                spawner,
                hooks,
                options,
                state: Mutex::new(State::default()),
                pending: Mutex::new(HashMap::new()),
                starting: Mutex::new(()),
                changed: Condvar::new(),
            }),
        }
    }

    /// Attach to the companion (started when none runs); the attach answer.
    /// It is kept: a restarted sidecar attaches again with the same port and nonce.
    pub(crate) fn attach_companion(&self, port: u16, nonce: &str) -> Result<Value, BrowserError> {
        self.inner.lock().failed = None;
        self.inner.attach(port, nonce)
    }

    /// One gated op (the helper's own id).
    pub(crate) fn op(&self, op: &GatedOp) -> Result<HostReply, BrowserError> {
        let wait = Duration::from_millis(op.deadline_ms()) + REPLY_GRACE;
        self.inner
            .exchange(|id| serde_json::to_string(&op.request(id)), wait)
    }

    /// A request of the helper's own (profile `companion`).
    pub(crate) fn request(
        &self,
        op: &str,
        target: Option<&str>,
        args: Value,
        deadline: Duration,
    ) -> Result<HostReply, BrowserError> {
        self.inner.call(
            op,
            Some(COMPANION_PROFILE),
            target,
            args,
            deadline,
            deadline + REPLY_GRACE,
        )
    }

    /// It is attached to the companion now.
    #[cfg(test)]
    pub(crate) fn attached(&self) -> bool {
        let state = self.inner.lock();
        state.attached && state.link.is_some()
    }

    /// The running sidecar's pid.
    #[cfg(test)]
    pub(crate) fn pid(&self) -> Option<u32> {
        self.inner.lock().link.as_ref().map(|link| link.pid)
    }

    /// One supervisor pass at `now`: the ping, and a re-attach that is due.
    #[cfg(test)]
    pub(crate) fn tick(&self, now: Instant) {
        self.inner.tick(now);
    }

    /// Close its stdin (it exits), at most a few seconds, then its group.
    pub(crate) fn shutdown(&self) {
        self.inner.shutdown();
    }
}

impl CompanionLink for Sidecar {
    fn attach(&self, port: u16, nonce: &str) -> Result<Value, BrowserError> {
        self.attach_companion(port, nonce)
    }

    fn quit(&self) {
        let deadline = QUIT_DEADLINE.as_millis() as u64;
        let sent = self.inner.send_only(|id| {
            serde_json::to_string(&HostRequest {
                id,
                op: "control_quit",
                profile: Some(COMPANION_PROFILE),
                target: None,
                args: Value::Null,
                deadline_ms: deadline,
                activity: None,
            })
        });
        if let Err(err) = sent {
            tracing::debug!(event = "browser.link.sidecar.quit", code = %err.code, "control_quit not sent");
        }
    }

    fn gone(&self) {
        let mut state = self.inner.lock();
        state.companion = None;
        state.attached = false;
    }
}

/// One line, at most `max` bytes (a longer one is read to its end and
/// dropped: `Some(None)`); `None` at the end.
fn read_line_capped(
    reader: &mut dyn BufRead,
    max: usize,
    line: &mut Vec<u8>,
) -> io::Result<Option<bool>> {
    line.clear();
    let mut over = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok((!line.is_empty() || over).then_some(!over));
        }
        let (take, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buf.len(), false),
        };
        if !over {
            if line.len() + take > max + 1 {
                over = true;
                line.clear();
            } else {
                line.extend_from_slice(&buf[..take]);
            }
        }
        reader.consume(take);
        if done {
            return Ok(Some(!over));
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn pending(&self) -> MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A sidecar runs (spawn + `hello` when none does).
    fn ensure_link(self: &Arc<Self>) -> Result<(), BrowserError> {
        let _one = self
            .starting
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        {
            let state = self.lock();
            if state.closed {
                return Err(gone_error());
            }
            if state.link.is_some() {
                return Ok(());
            }
        }
        let process = self.spawner.spawn().map_err(|err| {
            BrowserError::host_failed(format!("the browser sidecar did not start: {err}"))
        })?;
        let generation = {
            let mut state = self.lock();
            state.generation += 1;
            state.link = Some(Link {
                generation: state.generation,
                pid: process.pid,
                child: process.child,
                writer: Arc::new(Mutex::new(process.writer)),
                misses: 0,
            });
            state.generation
        };
        self.spawn_reader(generation, process.reader);
        self.ensure_supervisor();
        let hello = self
            .call("hello", None, None, Value::Null, HELLO_TIMEOUT, HELLO_TIMEOUT)
            .and_then(|reply| {
                let protocol = reply.result["host_protocol"].as_u64().unwrap_or(0);
                if protocol == u64::from(HOST_PROTOCOL) {
                    Ok(())
                } else {
                    Err(BrowserError::new(
                        "browser_runtime_outdated",
                        format!("the sidecar speaks protocol {protocol}, this herdr {HOST_PROTOCOL}; run \"herdr browser setup\""),
                    ))
                }
            });
        if let Err(err) = hello {
            self.drop_link(generation, None);
            return Err(err);
        }
        tracing::info!(
            event = "browser.link.sidecar.start",
            pid = process.pid,
            "remote browser sidecar started"
        );
        Ok(())
    }

    fn spawn_reader(self: &Arc<Self>, generation: u64, mut reader: Box<dyn BufRead + Send>) {
        let weak: Weak<Self> = Arc::downgrade(self);
        let max = self.options.line_max;
        let _ = std::thread::Builder::new()
            .name("browser-link-sidecar-reader".into())
            .spawn(move || {
                let mut line = Vec::new();
                loop {
                    match read_line_capped(&mut *reader, max, &mut line) {
                        Ok(Some(true)) => {
                            let Some(inner) = weak.upgrade() else { return };
                            inner.handle_line(&line);
                        }
                        Ok(Some(false)) => {
                            tracing::warn!(
                                event = "browser.link.sidecar.line",
                                max,
                                "a sidecar line over the limit was dropped"
                            );
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                if let Some(inner) = weak.upgrade() {
                    inner.drop_link(generation, Some("the browser sidecar exited".into()));
                }
            });
    }

    fn handle_line(&self, line: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if value.get("event").is_some() {
            if value["event"] == "browser"
                && value["kind"] == "disconnected"
                && value["profile"] == COMPANION_PROFILE
            {
                self.lock().attached = false;
            }
            self.hooks.event(value);
            return;
        }
        let Some(id) = value.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Ok(reply) = serde_json::from_value::<HostReply>(value) else {
            return;
        };
        if let Some(sender) = self.pending().remove(&id) {
            let _ = sender.send(reply);
        }
    }

    /// Write one request (`line` from its id); its reply, at most `wait`.
    fn exchange(
        &self,
        line: impl FnOnce(u64) -> serde_json::Result<String>,
        wait: Duration,
    ) -> Result<HostReply, BrowserError> {
        let (sender, receiver) = mpsc::channel();
        let id = self.write(line, Some(sender))?;
        match receiver.recv_timeout(wait) {
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending().remove(&id);
                Err(BrowserError::timeout(format!(
                    "the browser sidecar did not answer within {} ms",
                    wait.as_millis()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(gone_error()),
        }
    }

    fn send_only(
        &self,
        line: impl FnOnce(u64) -> serde_json::Result<String>,
    ) -> Result<(), BrowserError> {
        self.write(line, None).map(|_| ())
    }

    fn write(
        &self,
        line: impl FnOnce(u64) -> serde_json::Result<String>,
        reply: Option<mpsc::Sender<HostReply>>,
    ) -> Result<u64, BrowserError> {
        let (id, writer, generation) = {
            let mut state = self.lock();
            state.next_id += 1;
            let id = state.next_id;
            let link = state.link.as_ref().ok_or_else(gone_error)?;
            (id, Arc::clone(&link.writer), link.generation)
        };
        let text = line(id).map_err(|err| BrowserError::new("invalid_request", err.to_string()))?;
        if let Some(reply) = reply {
            self.pending().insert(id, reply);
        }
        let written = writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .write_line(&text);
        if let Err(err) = written {
            self.pending().remove(&id);
            self.drop_link(
                generation,
                Some(format!("the browser sidecar's stdin failed: {err}")),
            );
            return Err(gone_error());
        }
        Ok(id)
    }

    fn call(
        &self,
        op: &str,
        profile: Option<&str>,
        target: Option<&str>,
        args: Value,
        deadline: Duration,
        wait: Duration,
    ) -> Result<HostReply, BrowserError> {
        self.exchange(
            |id| {
                serde_json::to_string(&HostRequest {
                    id,
                    op,
                    profile,
                    target,
                    args,
                    deadline_ms: deadline.as_millis() as u64,
                    activity: None,
                })
            },
            wait,
        )
    }

    fn attach(self: &Arc<Self>, port: u16, nonce: &str) -> Result<Value, BrowserError> {
        self.ensure_link()?;
        let args = json!({ "port": port, "kind": "companion", "nonce": nonce, "remote": true });
        let mut attempt = 0;
        let reply = loop {
            attempt += 1;
            let reply = self.call(
                "attach",
                Some(COMPANION_PROFILE),
                None,
                args.clone(),
                ATTACH_TIMEOUT,
                ATTACH_TIMEOUT + REPLY_GRACE,
            )?;
            if reply.ok {
                break reply;
            }
            let error = reply.error.unwrap_or_default();
            if (error.code == "browser_timeout" || error.code == "attach_timeout") && attempt < 2 {
                tracing::warn!(event = "browser.link.sidecar.attach", message = %error.message, "companion attach timed out; retrying once");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            return Err(BrowserError::new(
                if error.code.is_empty() {
                    "attach_failed"
                } else {
                    &error.code
                },
                error.message,
            ));
        };
        {
            let mut state = self.lock();
            state.companion = Some((port, nonce.to_string()));
            state.attached = true;
            state.retry_at = None;
        }
        self.hooks.attached(&reply.result);
        Ok(reply.result)
    }

    /// The sidecar of `generation` went away (EOF, a failed write, wedged):
    /// its calls fail, its group goes; `why` for a drop that was no shutdown.
    fn drop_link(&self, generation: u64, why: Option<String>) {
        let (link, lost) = {
            let mut state = self.lock();
            if state.link.as_ref().map(|link| link.generation) != Some(generation) {
                return;
            }
            let link = state.link.take();
            let was_attached = std::mem::take(&mut state.attached);
            let lost = match (&why, state.closed) {
                (Some(why), false) => {
                    state.note_drop(why, Instant::now());
                    (was_attached || state.companion.is_some()).then(|| why.clone())
                }
                _ => None,
            };
            (link, lost)
        };
        self.pending().clear();
        if let Some(mut link) = link {
            if let Some(mut child) = link.child.take() {
                self.spawner.kill(link.pid);
                let _ = child.wait();
            } else {
                self.spawner.kill(link.pid);
            }
            tracing::warn!(
                event = "browser.link.sidecar.exit",
                pid = link.pid,
                why = why.as_deref().unwrap_or("stopped"),
                "remote browser sidecar dropped"
            );
        }
        if let Some(why) = lost {
            self.hooks.lost(&why);
        }
        self.changed.notify_all();
    }

    fn tick(self: &Arc<Self>, now: Instant) {
        let generation = {
            let state = self.lock();
            if state.closed {
                return;
            }
            state.link.as_ref().map(|link| link.generation)
        };
        if let Some(generation) = generation {
            self.ping(generation);
        }
        // a companion that is not attached (a new sidecar, a failed attach,
        // its CDP connection closed) is attached again on the backoff
        let reattach = {
            let state = self.lock();
            let due = state.failed.is_none() && state.retry_at.is_none_or(|at| at <= now);
            state
                .companion
                .clone()
                .filter(|_| due && !state.attached && !state.closed)
        };
        if let Some((port, nonce)) = reattach {
            if let Err(err) = self.attach(port, &nonce) {
                tracing::warn!(event = "browser.link.sidecar.reattach", code = %err.code, message = %err.message, "the sidecar did not attach again");
                let mut state = self.lock();
                if state.failed.is_none() && !state.closed {
                    state.note_drop(&err.message, now);
                }
            }
        }
    }

    /// One ping; two missed in a row and the sidecar of `generation` goes.
    fn ping(&self, generation: u64) {
        let timeout = self.options.ping_timeout;
        match self.call("ping", None, None, Value::Null, timeout, timeout) {
            Ok(_) => {
                if let Some(link) = self.lock().link.as_mut() {
                    link.misses = 0;
                }
            }
            Err(err) if err.code == "browser_timeout" => {
                let wedged = {
                    let mut state = self.lock();
                    match state
                        .link
                        .as_mut()
                        .filter(|link| link.generation == generation)
                    {
                        Some(link) => {
                            link.misses += 1;
                            link.misses >= self.options.misses
                        }
                        None => false,
                    }
                };
                if wedged {
                    self.drop_link(
                        generation,
                        Some(format!(
                            "the browser sidecar missed {} pings; restarted",
                            self.options.misses
                        )),
                    );
                }
            }
            Err(_) => {}
        }
    }

    /// One supervisor thread per sidecar, from its first start.
    fn ensure_supervisor(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.supervisor || state.closed {
                return;
            }
            state.supervisor = true;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        let every = self.options.ping_every;
        let started = std::thread::Builder::new()
            .name("browser-link-sidecar".into())
            .spawn(move || loop {
                {
                    let Some(inner) = weak.upgrade() else { return };
                    let state = inner.lock();
                    if state.closed {
                        return;
                    }
                    let _ = inner.changed.wait_timeout(state, every);
                }
                let Some(inner) = weak.upgrade() else { return };
                inner.tick(Instant::now());
            });
        if started.is_err() {
            self.lock().supervisor = false;
        }
    }

    fn shutdown(&self) {
        let link = {
            let mut state = self.lock();
            state.closed = true;
            state.attached = false;
            state.companion = None;
            state.link.take()
        };
        self.changed.notify_all();
        self.pending().clear();
        let Some(link) = link else { return };
        let Link {
            pid, child, writer, ..
        } = link;
        // its stdin closes once the last writer goes
        drop(writer);
        if let Some(mut child) = child {
            let until = Instant::now() + EXIT_WAIT;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < until => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    _ => break,
                }
            }
            self.spawner.kill(pid);
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests;
