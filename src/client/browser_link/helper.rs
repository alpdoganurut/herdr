//! The helper (`herdr browser-link <machine>`): one process per machine and
//! session on this Mac that joins the remote server (B), the machine's own
//! companion and sidecar, the browser's proxy and every client that shows
//! the machine (contract sections 1, 2.2-2.7).
//!
//! - The link: one connection to B (`browser.link_open` over ssh), pings
//!   numbered by the gate, frames written in lane order, every op gated
//!   right before the sidecar gets it, blobs and file pulls paced
//!   ([`super::files`]). Each open is an epoch: what is pending is keyed by
//!   B's `(boot_id, epoch)`, and an older epoch's answer goes nowhere.
//! - The companion's API ([`super::api_socket`]): eight methods answered
//!   here, `link.file`, the events stream, and B's allowlisted methods
//!   relayed; while the link is down those answer `link_down` at once.
//! - The clients ([`super::clients`]): placements on the helper's own
//!   board, the companion's asks relayed to their client.
//!
//! The helper ends 30 s after its last client left.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::api_socket::{event_line, Answer, ApiHandler, ApiSocket};
use super::clients::{
    ClientHandler, Clients, CompanionAction, HelperMessage, LinkView, MachineFacts,
};
use super::files::{file_answer, take_pictures, Blobs, FilePulls, Pull};
use super::gate::{Gate, GatedOp, HelperOp, Policy, Verdict};
use crate::api::schema::{
    BrowserCompanionAction, BrowserCompanionAppInfo, BrowserHostKind, BrowserHostLeaseInfo,
    BrowserHostRegisterParams, BrowserPlacementAction,
};
use crate::browser::host::HostReply;
use crate::browser::hub::COMPANION_PROFILE;
use crate::browser::link_proto::{
    bye, code, text, ApiReply, ApiRequest, BrowserLinkInfo, BrowserLinkOpenParams,
    BrowserLinkReady, Bye, EnsureReply, FileReply, HelperFrame, HostUp, LineBuffer, LinkError,
    LinkReply, ServerFrame, WriteLane, API_METHODS, ENSURE_MS, EVENT_KINDS, EVENT_LINK_UP,
    HELPER_SILENCE_MS, IN_USE_RETRY_MS, LINK_PROTOCOL, PING_MS, RECONNECT_FIRST_MS,
    RECONNECT_MAX_MS,
};
use crate::browser::placements::{ActionRefused, ClientFacts, PlacementBoard};
use crate::browser::state::HostTab;
use crate::browser::BrowserError;
use crate::server::headless::placement_action::PlacementActionPayload;
use crate::server::headless::view_cycle::ViewCyclePayload;

/// The lease the helper gives its companion: herdr's values.
const HEARTBEAT_MS: u64 = 5_000;
const LEASE_TTL_MS: u64 = 15_000;
const HOST_PROTOCOL: u32 = 1;
/// What the lease advertises: herdr's own list (`page_messages` never: page
/// API is off for a remote machine's pages).
const SERVER_FEATURES: [&str; 8] = [
    "host_sync",
    "host_report",
    "send_page",
    "show",
    "page_errors",
    "placements",
    "placement_actions",
    "view_cycle",
];
/// The request id of `browser.link_open` on its connection.
const OPEN_ID: &str = "browser-link-open";
/// A relayed call waits this long for B.
const RELAY_TIMEOUT: Duration = Duration::from_secs(30);
/// A `link.file` waits this long (the companion's own timeout is 20 s).
const FILE_TIMEOUT: Duration = Duration::from_secs(20);
/// A `bye` still goes out this long after the link ended.
const BYE_GRACE: Duration = Duration::from_secs(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// What the helper is built from

/// The machine's companion as the helper drives it: the method names of
/// C5's `companion::Companion`, so its impl is one line each (`instance`,
/// `readiness` and `focus` are not in C5 yet). The helper never calls one of
/// these under its own lock, so the companion may call [`CompanionHooks`]
/// under its own.
pub(crate) trait CompanionControl: Send + Sync {
    /// The helper's start: adopt the recorded companion or clear it
    /// (C5's `recover`, its `Recovered` dropped).
    fn recover(&self);
    /// The launch the link is opened for: the running or adopted
    /// companion's instance, else one kept until the next launch.
    fn instance(&self) -> String;
    /// `ready` and `detail` of `browser.link_open`.
    fn readiness(&self) -> (BrowserLinkReady, Option<String>);
    /// Starts it unless it runs and waits for its registration, at most
    /// `wait`. `Err`: the `ensure_re` error (C5's
    /// `EnsureError::link_error(<this Mac's label>)`).
    fn ensure(&self, wait: Duration) -> Result<(), LinkError>;
    /// The companion's `browser.host_register`: only the current launch
    /// token; the sidecar attaches to it (`remote: true`, profile
    /// `companion`). `Ok`: the attach's answer (`tabs`, for `host_up`).
    fn register(&self, params: &BrowserHostRegisterParams) -> Result<Value, BrowserError>;
    /// Its heartbeat; `false`: that registration is not the current one.
    fn heartbeat(&self, launch_token: &str) -> bool;
    /// It unregistered: it quits on its own (no crash).
    fn unregistered(&self, launch_token: &str);
    /// The user's start (the window shows).
    fn start(&self) -> Result<(), String>;
    /// Stops it; blocking.
    fn stop(&self);
    fn restart(&self) -> Result<(), String>;
    /// Raises it for the user (`raise{user:true}`; not in C5 yet).
    fn focus(&self) -> Result<(), String>;
    /// "Forget <machine>'s logins": stop, wait for the exit, then remove
    /// its profile and nothing else.
    fn forget(&self) -> io::Result<()>;
    /// The helper exits: the companion quits, nothing starts again.
    fn shutdown(&self);
    /// What `browser.companion`, the lease's `update` and the clients show.
    fn info(&self) -> BrowserCompanionAppInfo;
}

/// The machine's sidecar (C5's `sidecar::Sidecar`): `host.mjs` attached to
/// the companion as profile `companion` with `remote: true`.
pub(crate) trait SidecarOps: Send + Sync {
    /// One gated op, on the caller's thread until it is answered (its
    /// deadline and the grace); the full reply, `ok` or not. `Err`: no
    /// answer came; the op's error.
    fn op(&self, op: &GatedOp) -> Result<HostReply, BrowserError>;
    /// Ends the sidecar (the helper quits).
    fn shutdown(&self);
}

/// What the sidecar tells the helper (the same as C5's
/// `sidecar::SidecarHooks`; the integrator keeps one).
pub(crate) trait SidecarHooks: Send + Sync {
    /// A sidecar event line, as it came (`ev{event}`).
    fn event(&self, event: Value);
    /// It attached again by itself; its answer (the tabs, for `host_up`).
    fn attached(&self, answer: &Value);
    /// The attached sidecar went away (`host_down`).
    fn lost(&self, reason: &str);
}

/// What the companion tells the helper (the same as C5's
/// `companion::CompanionHooks`).
pub(crate) trait CompanionHooks: Send + Sync {
    /// What [`CompanionControl::info`] says changed (the clients hear it on
    /// the next tick).
    fn changed(&self);
    /// The process went away; `reason` for the remote machine's log.
    fn exited(&self, reason: &str);
}

/// The browser's proxy, bound by the companion's launcher with the port and
/// secret of `proxy.json`, fixed for the companion's life (the same as C5's
/// `companion::ProxyBinder`).
pub(crate) trait ProxyBinder: Send + Sync {
    /// Listens on 127.0.0.1:`port` (0: a free one) with `secret`; the port
    /// it listens on. A later call replaces the listener.
    fn bind(&self, port: u16, secret: &str) -> io::Result<u16>;
}

/// One connection to B's API socket, ready for a request line.
pub(crate) struct LinkConnection {
    pub(crate) reader: Box<dyn Read + Send>,
    pub(crate) writer: Box<dyn Write + Send>,
    /// Ends the connection: a blocked read or write returns.
    pub(crate) end: Box<dyn FnOnce() + Send>,
}

pub(crate) trait LinkTransport: Send + Sync {
    fn open(&self) -> io::Result<LinkConnection>;
}

/// The helper's waits (tests shorten them).
#[derive(Debug, Clone)]
pub(crate) struct Timing {
    pub(crate) ping: Duration,
    pub(crate) silence: Duration,
    pub(crate) first_retry: Duration,
    pub(crate) max_retry: Duration,
    /// After `link_in_use` or `replaced`.
    pub(crate) slow_retry: Duration,
    /// A refusal that stays (`link_disabled`) waits up to this.
    pub(crate) steady_retry: Duration,
    pub(crate) answer: Duration,
    pub(crate) tick: Duration,
    pub(crate) idle_exit: Duration,
    pub(crate) lease_ttl: Duration,
    /// A registration waits this long for B's `host_ok`.
    pub(crate) host_ok_wait: Duration,
    pub(crate) ensure: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_millis(PING_MS),
            silence: Duration::from_millis(HELPER_SILENCE_MS),
            first_retry: Duration::from_millis(RECONNECT_FIRST_MS),
            max_retry: Duration::from_millis(RECONNECT_MAX_MS),
            slow_retry: Duration::from_millis(IN_USE_RETRY_MS),
            steady_retry: Duration::from_secs(300),
            answer: Duration::from_secs(20),
            tick: Duration::from_millis(250),
            idle_exit: Duration::from_secs(30),
            lease_ttl: Duration::from_millis(LEASE_TTL_MS),
            host_ok_wait: Duration::from_secs(3),
            ensure: Duration::from_millis(ENSURE_MS - 5_000),
        }
    }
}

pub(crate) type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// Everything a helper is built from.
pub(crate) struct Deps {
    /// This Mac's name, for B's texts.
    pub(crate) label: String,
    /// The machine's name, for the companion's texts.
    pub(crate) machine: String,
    /// Random per Mac, kept.
    pub(crate) host_id: String,
    pub(crate) herdr: String,
    pub(crate) companion: Arc<dyn CompanionControl>,
    pub(crate) sidecar: Arc<dyn SidecarOps>,
    pub(crate) transport: Arc<dyn LinkTransport>,
    /// A's own config, read per op.
    pub(crate) policy: Arc<dyn Fn() -> Policy + Send + Sync>,
    /// The clock that counts sleep.
    pub(crate) clock: Clock,
    /// Called with whether the link is open (the forwards dial only then).
    pub(crate) on_link: Arc<dyn Fn(bool) + Send + Sync>,
    pub(crate) timing: Timing,
}

// ---------------------------------------------------------------------------
// The writer's queue

#[derive(Default)]
struct QueueState {
    lanes: [std::collections::VecDeque<String>; 3],
    bye: Option<String>,
    ended: bool,
}

/// The lines of one link, in lane order, for its writer thread.
#[derive(Default)]
struct WriteQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

fn lane_index(lane: WriteLane) -> usize {
    match lane {
        WriteLane::Alive => 0,
        WriteLane::Work => 1,
        WriteLane::Bulk => 2,
    }
}

impl WriteQueue {
    fn push(&self, frame: &HelperFrame) {
        let line = match frame.encode() {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!(%err, "a frame for the browser link is too large; dropped");
                return;
            }
        };
        let mut state = lock(&self.state);
        if state.ended {
            return;
        }
        state.lanes[lane_index(frame.lane())].push_back(line);
        self.changed.notify_all();
    }

    /// The link ends: what waits is dropped, `bye` still goes out.
    fn end(&self, bye: Option<Bye>) {
        let mut state = lock(&self.state);
        if state.ended {
            return;
        }
        for lane in &mut state.lanes {
            lane.clear();
        }
        state.bye = bye.and_then(|bye| HelperFrame::Bye(bye).encode().ok());
        state.ended = true;
        self.changed.notify_all();
    }

    fn next(&self) -> Option<String> {
        let mut state = lock(&self.state);
        loop {
            if let Some(line) = state.lanes.iter_mut().find_map(|lane| lane.pop_front()) {
                return Some(line);
            }
            if let Some(bye) = state.bye.take() {
                return Some(bye);
            }
            if state.ended {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

// ---------------------------------------------------------------------------
// State

/// How the link is, for the lease and the clients.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkStatus {
    Connecting,
    Linked,
    Reconnecting,
    /// Another Mac has the machine's browser; B's text.
    InUse(String),
    /// The machine takes no link (`link_disabled`, `browser_disabled`, a
    /// link protocol it does not speak); its text.
    Off(String),
}

type EpochKey = (String, u64);

struct Session {
    key: EpochKey,
    gate: Gate,
    queue: Arc<WriteQueue>,
    /// B's lease for the companion, once `host_ok` came.
    host_ok: Option<String>,
    /// `host_up` went out on this link (and no `host_down` since).
    host_up: bool,
    /// When B was last heard, on the clock.
    heard: Duration,
    /// It carried something: the backoff starts over after it.
    served: bool,
    /// B's goodbye.
    bye: Option<String>,
}

struct Lease {
    id: String,
    instance: String,
    version: String,
    electron: String,
    launch_token: Option<String>,
    expires: Duration,
}

struct Attached {
    instance: String,
    version: String,
    electron: String,
    tabs: Vec<HostTab>,
}

struct PendingOp {
    key: EpochKey,
    link_id: u64,
    op: &'static str,
}

#[derive(Default)]
struct Waiters {
    api: HashMap<u64, mpsc::Sender<Option<ApiReply>>>,
    files: HashMap<u64, mpsc::Sender<Option<FileReply>>>,
}

struct State {
    session: Option<Session>,
    link: LinkStatus,
    lease: Option<Lease>,
    attached: Option<Attached>,
    ops: HashMap<u64, PendingOp>,
    blobs: Blobs,
    files: FilePulls,
    waiters: Waiters,
    next_api_id: u64,
    /// The upstream `events.subscribe` of this epoch.
    subscribe_id: Option<u64>,
    streams: Vec<mpsc::Sender<String>>,
    cycle_keys: Vec<String>,
    /// Connected clients (view cycles go to them).
    clients: Vec<u64>,
    /// What the clients were told last.
    told: Option<LinkView>,
}

/// One helper; see the module docs.
pub(crate) struct Helper {
    deps: Deps,
    state: Mutex<State>,
    changed: Condvar,
    board: PlacementBoard,
    running: AtomicBool,
    /// Numbers the ops at the sidecar.
    next_ticket: AtomicU64,
    clients: Mutex<Option<Arc<Clients>>>,
    me: Weak<Helper>,
}

/// The helper behind a socket or the sidecar, without keeping it alive.
#[derive(Clone)]
struct Weakly(Weak<Helper>);

/// What a link's threads tell its loop.
enum LinkEvent {
    Answer(Result<BrowserLinkInfo, LinkError>),
    Ended(String),
}

/// What comes after a link.
#[derive(Debug, Clone, PartialEq, Eq)]
enum After {
    /// A link that carried something, or an open that failed: the normal
    /// backoff.
    Retry {
        served: bool,
    },
    /// `link_in_use` or `replaced`.
    Slow,
    /// A refusal that stays.
    Steady,
    Stop,
}

impl Helper {
    pub(crate) fn new(deps: Deps) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            deps,
            state: Mutex::new(State {
                session: None,
                link: LinkStatus::Connecting,
                lease: None,
                attached: None,
                ops: HashMap::new(),
                blobs: Blobs::default(),
                files: FilePulls::default(),
                waiters: Waiters::default(),
                next_api_id: 0,
                subscribe_id: None,
                streams: Vec::new(),
                cycle_keys: Vec::new(),
                clients: Vec::new(),
                told: None,
            }),
            changed: Condvar::new(),
            board: PlacementBoard::new(),
            running: AtomicBool::new(true),
            next_ticket: AtomicU64::new(1),
            clients: Mutex::new(None),
            me: me.clone(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn now(&self) -> Duration {
        (self.deps.clock)()
    }

    fn running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);
        self.changed.notify_all();
    }

    fn weakly(&self) -> Weakly {
        Weakly(self.me.clone())
    }

    pub(crate) fn api_handler(&self) -> Arc<dyn ApiHandler> {
        Arc::new(self.weakly())
    }

    pub(crate) fn client_handler(&self) -> Arc<dyn ClientHandler> {
        Arc::new(self.weakly())
    }

    pub(crate) fn set_clients(&self, clients: Arc<Clients>) {
        *lock(&self.clients) = Some(clients);
    }

    fn clients(&self) -> Option<Arc<Clients>> {
        lock(&self.clients).clone()
    }

    fn linked(state: &State) -> bool {
        state
            .session
            .as_ref()
            .is_some_and(|session| session.host_ok.is_some())
    }

    fn queue(state: &State, frame: &HelperFrame) {
        if let Some(session) = &state.session {
            session.queue.push(frame);
        }
    }

    /// What the bulk lane's windows let out now.
    fn pump(state: &mut State) {
        let Some(session) = &state.session else {
            return;
        };
        let queue = Arc::clone(&session.queue);
        while let Some(chunk) = state.blobs.next_chunk() {
            queue.push(&HelperFrame::Blob(chunk));
        }
        while let Some(request) = state.files.next_frame() {
            queue.push(&HelperFrame::File(request));
        }
    }

    // -----------------------------------------------------------------------
    // The link

    /// Opens the link again and again until the helper stops.
    pub(crate) fn link_loop(self: &Arc<Self>) {
        let timing = self.deps.timing.clone();
        let mut retry = timing.first_retry;
        let mut steady = timing.slow_retry;
        while self.running() {
            let wait = match self.one_link() {
                After::Stop => break,
                After::Retry { served } => {
                    steady = timing.slow_retry;
                    if served {
                        retry = timing.first_retry;
                    }
                    let wait = retry;
                    retry = (retry * 2).min(timing.max_retry);
                    wait
                }
                After::Slow => {
                    retry = timing.first_retry;
                    timing.slow_retry
                }
                After::Steady => {
                    retry = timing.first_retry;
                    let wait = steady;
                    steady = (steady * 2).min(timing.steady_retry);
                    wait
                }
            };
            self.sleep(wait);
        }
    }

    /// Waits `wait`, or less when the helper stops.
    fn sleep(&self, wait: Duration) {
        let deadline = Instant::now() + wait;
        let mut state = self.state();
        while self.running() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn one_link(self: &Arc<Self>) -> After {
        let conn = match self.deps.transport.open() {
            Ok(conn) => conn,
            Err(err) => {
                tracing::info!(%err, "the browser link did not open");
                self.set_link(LinkStatus::Reconnecting);
                return After::Retry { served: false };
            }
        };
        let LinkConnection {
            reader,
            mut writer,
            end,
        } = conn;
        let opened = self.now();
        let (ready, detail) = self.deps.companion.readiness();
        let params = BrowserLinkOpenParams {
            link_protocol: LINK_PROTOCOL,
            host_id: self.deps.host_id.clone(),
            instance: self.deps.companion.instance(),
            label: self.deps.label.clone(),
            herdr: self.deps.herdr.clone(),
            features: Vec::new(),
            ready,
            detail,
            takeover: false,
        };
        let request = json!({"id": OPEN_ID, "method": crate::browser::link_proto::LINK_OPEN_METHOD, "params": params});
        if writeln!(writer, "{request}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            end();
            return After::Retry { served: false };
        }
        let (events, heard) = mpsc::channel();
        let queue = Arc::new(WriteQueue::default());
        let reader = {
            let helper = Arc::clone(self);
            let events = events.clone();
            let queue = Arc::clone(&queue);
            std::thread::Builder::new()
                .name("browser-link-read".into())
                .spawn(move || helper.read_link(reader, &events, opened, &queue))
        };
        let Ok(reader) = reader else {
            end();
            return After::Retry { served: false };
        };
        let info = match heard.recv_timeout(self.deps.timing.answer) {
            Ok(LinkEvent::Answer(Ok(info))) => info,
            Ok(LinkEvent::Answer(Err(error))) => {
                end();
                let _ = reader.join();
                return self.refused(error);
            }
            Ok(LinkEvent::Ended(why)) => {
                tracing::info!(%why, "the browser link ended before its answer");
                end();
                let _ = reader.join();
                self.drop_session_of(&queue);
                self.set_link(LinkStatus::Reconnecting);
                return After::Retry { served: false };
            }
            Err(_) => {
                end();
                let _ = reader.join();
                // an answer that came as it timed out opened a session
                self.drop_session_of(&queue);
                self.set_link(LinkStatus::Reconnecting);
                return After::Retry { served: false };
            }
        };
        let key: EpochKey = (info.boot_id.clone(), info.epoch);
        tracing::info!(epoch = info.epoch, "browser link open");
        let writer_thread = {
            let queue = Arc::clone(&queue);
            let events = events.clone();
            std::thread::Builder::new()
                .name("browser-link-write".into())
                .spawn(move || {
                    while let Some(line) = queue.next() {
                        if writer
                            .write_all(line.as_bytes())
                            .and_then(|()| writer.flush())
                            .is_err()
                        {
                            let _ = events.send(LinkEvent::Ended("the link broke".into()));
                            return;
                        }
                    }
                })
        };
        (self.deps.on_link)(true);
        self.changed.notify_all();
        let after = match writer_thread {
            Ok(writer_thread) => {
                let after = self.keep_link(&key, &queue, &heard);
                let ended = Instant::now();
                // the writer may still say goodbye
                while !writer_thread.is_finished() && ended.elapsed() < BYE_GRACE {
                    std::thread::sleep(Duration::from_millis(10));
                }
                end();
                let _ = writer_thread.join();
                after
            }
            Err(_) => {
                end();
                After::Retry { served: false }
            }
        };
        let _ = reader.join();
        self.drop_session(&key);
        after
    }

    /// Pings and watches the open link until it ends.
    fn keep_link(
        &self,
        key: &EpochKey,
        queue: &WriteQueue,
        heard: &mpsc::Receiver<LinkEvent>,
    ) -> After {
        let timing = &self.deps.timing;
        let mut last_ping: Option<Instant> = None;
        loop {
            if !self.running() {
                queue.end(Some(Bye {
                    code: bye::QUIT.into(),
                    message: "the helper quits".into(),
                }));
                return After::Stop;
            }
            if last_ping.is_none_or(|at| at.elapsed() >= timing.ping) {
                let mut state = self.state();
                let now = self.now();
                if let Some(session) = state.session.as_mut().filter(|s| s.key == *key) {
                    let n = session.gate.ping(now);
                    session.queue.push(&HelperFrame::Ping { n });
                }
                last_ping = Some(Instant::now());
            }
            match heard.recv_timeout(timing.tick) {
                Ok(LinkEvent::Ended(why)) => {
                    queue.end(None);
                    let state = self.state();
                    let session = state.session.as_ref().filter(|s| s.key == *key);
                    let served = session.is_some_and(|s| s.served);
                    let bye = session.and_then(|s| s.bye.clone());
                    tracing::info!(%why, ?bye, "browser link ended");
                    return match bye.as_deref() {
                        Some(bye::REPLACED) => After::Slow,
                        _ => After::Retry { served },
                    };
                }
                Ok(LinkEvent::Answer(_)) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    queue.end(None);
                    return After::Retry { served: false };
                }
            }
            let silent = {
                let state = self.state();
                state
                    .session
                    .as_ref()
                    .filter(|s| s.key == *key)
                    .is_none_or(|s| self.now().saturating_sub(s.heard) >= timing.silence)
            };
            if silent {
                tracing::info!("the browser link went silent");
                queue.end(None);
                let served = self
                    .state()
                    .session
                    .as_ref()
                    .is_some_and(|s| s.key == *key && s.served);
                return After::Retry { served };
            }
        }
    }

    /// `browser.link_open` answered an error.
    fn refused(&self, error: LinkError) -> After {
        tracing::info!(code = %error.code, "the browser link was refused");
        match error.code.as_str() {
            code::LINK_IN_USE => {
                self.set_link(LinkStatus::InUse(error.message));
                After::Slow
            }
            code::LINK_DISABLED | code::BROWSER_DISABLED | code::LINK_PROTOCOL_UNSUPPORTED => {
                self.set_link(LinkStatus::Off(error.message));
                After::Steady
            }
            _ => {
                self.set_link(LinkStatus::Reconnecting);
                After::Retry { served: false }
            }
        }
    }

    fn set_link(&self, link: LinkStatus) {
        self.state().link = link;
    }

    /// The link of `key` is open: its session begins before any of its
    /// frames is read.
    fn open_session(&self, key: &EpochKey, opened: Duration, queue: &Arc<WriteQueue>) {
        let mut state = self.state();
        state.session = Some(Session {
            key: key.clone(),
            gate: Gate::new(self.deps.label.clone(), opened),
            queue: Arc::clone(queue),
            host_ok: None,
            host_up: false,
            heard: self.now(),
            served: false,
            bye: None,
        });
        state.link = LinkStatus::Reconnecting;
        // the same companion is back: B reopens the same targets
        if state.attached.is_some() {
            self.send_host_up(&mut state);
        }
        drop(state);
        self.changed.notify_all();
    }

    /// The reading side: the answer line, then frames.
    fn read_link(
        &self,
        mut reader: Box<dyn Read + Send>,
        events: &mpsc::Sender<LinkEvent>,
        opened: Duration,
        queue: &Arc<WriteQueue>,
    ) {
        let mut lines = LineBuffer::default();
        let mut buf = vec![0u8; 64 * 1024];
        let mut key: Option<EpochKey> = None;
        loop {
            let read = match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = events.send(LinkEvent::Ended("the connection closed".into()));
                    return;
                }
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    let _ = events.send(LinkEvent::Ended(err.to_string()));
                    return;
                }
            };
            lines.push(&buf[..read]);
            loop {
                let line = match lines.next_line() {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(err) => {
                        self.protocol_end(key.as_ref(), &err.to_string());
                        let _ = events.send(LinkEvent::Ended(err.to_string()));
                        return;
                    }
                };
                let Some(epoch) = key.as_ref() else {
                    let answer = parse_answer(&line);
                    if let Ok(info) = &answer {
                        let epoch = (info.boot_id.clone(), info.epoch);
                        self.open_session(&epoch, opened, queue);
                        key = Some(epoch);
                    }
                    let failed = answer.is_err();
                    let _ = events.send(LinkEvent::Answer(answer));
                    if failed {
                        return;
                    }
                    continue;
                };
                match ServerFrame::decode(&line) {
                    Ok(frame) => {
                        let ended = matches!(frame, ServerFrame::Bye(_));
                        self.on_frame(epoch, frame);
                        if ended {
                            let _ = events.send(LinkEvent::Ended("the server said bye".into()));
                            return;
                        }
                    }
                    Err(err) => {
                        self.protocol_end(Some(epoch), &err.to_string());
                        let _ = events.send(LinkEvent::Ended(err.to_string()));
                        return;
                    }
                }
            }
        }
    }

    /// B sent a line that is no frame: the link ends with `bye{protocol}`.
    fn protocol_end(&self, key: Option<&EpochKey>, why: &str) {
        let state = self.state();
        if let Some(session) = state.session.as_ref().filter(|s| Some(&s.key) == key) {
            session.queue.end(Some(Bye {
                code: bye::PROTOCOL.into(),
                message: why.chars().take(200).collect(),
            }));
        }
    }

    fn on_frame(&self, key: &EpochKey, frame: ServerFrame) {
        let now = self.now();
        let mut state = self.state();
        let Some(session) = state.session.as_mut().filter(|s| s.key == *key) else {
            return;
        };
        session.heard = now;
        match frame {
            ServerFrame::Pong { .. } => session.served = true,
            ServerFrame::Ensure(ensure) => {
                let verdict = session.gate.ensure(&ensure, now);
                match verdict {
                    Err(error) => session.queue.push(&HelperFrame::EnsureRe(EnsureReply {
                        id: ensure.id,
                        ok: false,
                        error: Some(error),
                    })),
                    Ok(()) => {
                        drop(state);
                        self.spawn_ensure(key.clone(), ensure.id);
                    }
                }
            }
            ServerFrame::HostOk { lease_id } => {
                session.host_ok = Some(lease_id);
                session.served = true;
                state.link = LinkStatus::Linked;
                // B's events once per link: a later host_ok (the sidecar came
                // back) keeps that stream
                if state.subscribe_id.is_none() {
                    state.next_api_id += 1;
                    let id = state.next_api_id;
                    state.subscribe_id = Some(id);
                    let subscriptions: Vec<Value> = EVENT_KINDS
                        .iter()
                        .map(|kind| json!({"type": kind}))
                        .collect();
                    Self::queue(
                        &state,
                        &HelperFrame::Api(ApiRequest {
                            id,
                            method: "events.subscribe".into(),
                            params: json!({"subscriptions": subscriptions}),
                        }),
                    );
                }
                let line = event_line(&json!({"event": EVENT_LINK_UP, "data": {}}));
                state
                    .streams
                    .retain(|stream| stream.send(line.clone()).is_ok());
                drop(state);
                self.changed.notify_all();
            }
            ServerFrame::Op(op) => {
                drop(state);
                self.run_op(key, op);
            }
            ServerFrame::BlobAck(ack) => {
                if let Some(due) = state.blobs.ack(&ack) {
                    for reply in due {
                        Self::queue(&state, &HelperFrame::Re(reply));
                    }
                }
                Self::pump(&mut state);
            }
            ServerFrame::FileRe(reply) => {
                if let Some(ticket) = state.files.answered(&reply) {
                    if let Some(waiter) = state.waiters.files.remove(&ticket) {
                        let _ = waiter.send(Some(reply));
                    }
                }
                Self::pump(&mut state);
            }
            ServerFrame::ApiRe(reply) => {
                if state.subscribe_id == Some(reply.id) {
                    if let Some(error) = &reply.error {
                        tracing::warn!(code = %error.code, "the machine's events did not subscribe");
                    }
                } else if let Some(waiter) = state.waiters.api.remove(&reply.id) {
                    let _ = waiter.send(Some(reply));
                }
            }
            ServerFrame::Event { event } => {
                let line = event_line(&event);
                state
                    .streams
                    .retain(|stream| stream.send(line.clone()).is_ok());
            }
            ServerFrame::Bye(bye) => {
                session.bye = Some(bye.code);
                session.queue.end(None);
            }
            ServerFrame::Unknown => {}
        }
    }

    /// The link of `key` is over: everything waiting on it ends now.
    fn drop_session(&self, key: &EpochKey) {
        let mut state = self.state();
        if state.session.as_ref().is_some_and(|s| s.key == *key) {
            state.session = None;
        }
        if matches!(state.link, LinkStatus::Linked | LinkStatus::Connecting) {
            state.link = LinkStatus::Reconnecting;
        }
        state.subscribe_id = None;
        state.blobs.clear();
        state.ops.retain(|_, op| op.key != *key);
        for (_, waiter) in state.waiters.api.drain() {
            let _ = waiter.send(None);
        }
        for ticket in state.files.drain() {
            if let Some(waiter) = state.waiters.files.remove(&ticket) {
                let _ = waiter.send(None);
            }
        }
        drop(state);
        (self.deps.on_link)(false);
        self.changed.notify_all();
    }

    /// The session of this queue, if its answer opened one, ends.
    fn drop_session_of(&self, queue: &Arc<WriteQueue>) {
        let key = self
            .state()
            .session
            .as_ref()
            .filter(|session| Arc::ptr_eq(&session.queue, queue))
            .map(|session| session.key.clone());
        if let Some(key) = key {
            self.drop_session(&key);
        }
    }

    fn send_host_up(&self, state: &mut State) {
        let Some(attached) = &state.attached else {
            return;
        };
        let frame = HelperFrame::HostUp(HostUp {
            instance: attached.instance.clone(),
            version: attached.version.clone(),
            electron: attached.electron.clone(),
            tabs: attached.tabs.clone(),
        });
        if let Some(session) = state.session.as_mut() {
            session.queue.push(&frame);
            session.host_up = true;
        }
    }

    // -----------------------------------------------------------------------
    // Ops and the sidecar

    fn spawn_ensure(&self, key: EpochKey, id: u64) {
        let helper = self.me.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-link-ensure".into())
            .spawn(move || {
                if let Some(helper) = helper.upgrade() {
                    helper.ensure(&key, id);
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "no thread for the browser's ensure");
        }
    }

    /// B needs the browser: start the companion, then answer once it serves
    /// (its `host_up` goes first).
    fn ensure(&self, key: &EpochKey, id: u64) {
        let started = Instant::now();
        let wait = self.deps.timing.ensure;
        let result = self.deps.companion.ensure(wait).and_then(|()| {
            let not_attached = |detail: &str| {
                LinkError::new(
                    code::HOST_UNAVAILABLE,
                    text::start_failed(&self.deps.label, detail),
                )
            };
            let mut state = self.state();
            loop {
                let up = state.attached.is_some()
                    && state
                        .session
                        .as_ref()
                        .is_some_and(|s| s.key == *key && s.host_up);
                if up {
                    return Ok(());
                }
                if state.session.as_ref().is_none_or(|s| s.key != *key) {
                    return Err(not_attached("the link dropped"));
                }
                let left = wait.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(not_attached("it did not attach in time"));
                }
                state = self
                    .changed
                    .wait_timeout(state, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        });
        let error = result.err();
        let state = self.state();
        if let Some(session) = state.session.as_ref().filter(|s| s.key == *key) {
            session.queue.push(&HelperFrame::EnsureRe(EnsureReply {
                id,
                ok: error.is_none(),
                error,
            }));
        }
    }

    fn run_op(&self, key: &EpochKey, op: crate::browser::link_proto::LinkOp) {
        let link_id = op.id;
        let policy = (self.deps.policy)();
        let mut state = self.state();
        let attached = state.attached.is_some();
        let Some(session) = state.session.as_ref().filter(|s| s.key == *key) else {
            return;
        };
        // gated right before the sidecar gets it: a sleep since the read counts
        let verdict = session.gate.op(op, policy, self.now());
        let answer = |state: &State, reply: LinkReply| Self::queue(state, &HelperFrame::Re(reply));
        match verdict {
            Verdict::Refuse(error) => answer(&state, error_reply(link_id, error)),
            Verdict::Helper(HelperOp::Hello) => answer(
                &state,
                ok_reply(
                    link_id,
                    json!({"host_protocol": crate::browser::host::HOST_PROTOCOL}),
                ),
            ),
            Verdict::Helper(HelperOp::Ping) => answer(&state, ok_reply(link_id, json!({}))),
            Verdict::Run(_) if !attached => answer(
                &state,
                error_reply(
                    link_id,
                    LinkError::new(
                        code::HOST_UNAVAILABLE,
                        text::start_failed(&self.deps.label, "it is not running"),
                    ),
                ),
            ),
            Verdict::Run(gated) => {
                let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
                state.ops.insert(
                    ticket,
                    PendingOp {
                        key: key.clone(),
                        link_id,
                        op: gated.op(),
                    },
                );
                drop(state);
                let helper = self.me.clone();
                let sidecar = Arc::clone(&self.deps.sidecar);
                let spawned = std::thread::Builder::new()
                    .name("browser-link-op".into())
                    .stack_size(256 * 1024)
                    .spawn(move || {
                        let answer = sidecar
                            .op(&gated)
                            .map_err(|err| LinkError::new(err.code(), err.message()));
                        if let Some(helper) = helper.upgrade() {
                            helper.op_done(ticket, answer);
                        }
                    });
                if let Err(err) = spawned {
                    tracing::warn!(%err, "no thread for a browser op");
                    self.op_done(ticket, Err(LinkError::new(code::HOST_BUSY, text::busy())));
                }
            }
        }
    }

    /// An op's answer from the sidecar; nothing when the link it came on is
    /// over, or the sidecar's loss answered it already.
    fn op_done(&self, ticket: u64, answer: Result<HostReply, LinkError>) {
        let mut state = self.state();
        let Some(op) = state.ops.remove(&ticket) else {
            return;
        };
        if state.session.as_ref().is_none_or(|s| s.key != op.key) {
            return;
        }
        let reply = match answer {
            Ok(reply) => reply,
            Err(error) => {
                Self::queue(&state, &HelperFrame::Re(error_reply(op.link_id, error)));
                return;
            }
        };
        let mut reply = LinkReply::from(reply);
        reply.id = op.link_id;
        let pictures = if op.op == "screenshot" && reply.ok {
            match take_pictures(&mut reply.result) {
                Ok(pictures) => pictures,
                Err(why) => {
                    let error = LinkError::new("too_large", why);
                    Self::queue(&state, &HelperFrame::Re(error_reply(op.link_id, error)));
                    return;
                }
            }
        } else {
            Vec::new()
        };
        if let Some(reply) = state.blobs.push(reply, pictures) {
            Self::queue(&state, &HelperFrame::Re(reply));
        }
        Self::pump(&mut state);
    }

    fn sidecar_event(&self, event: Value) {
        let state = self.state();
        if state.session.as_ref().is_some_and(|s| s.host_up) {
            Self::queue(&state, &HelperFrame::Ev { event });
        }
    }

    /// The sidecar is gone: every op it had is answered, then `host_down`.
    fn host_gone(&self, state: &mut State, reason: &str) {
        state.attached = None;
        let key = state.session.as_ref().map(|s| s.key.clone());
        let mut ops: Vec<(u64, &'static str)> = state
            .ops
            .drain()
            .filter(|(_, op)| Some(&op.key) == key.as_ref())
            .map(|(_, op)| (op.link_id, op.op))
            .collect();
        // a picture still on its way: its answer does not come either
        ops.extend(state.blobs.ops().into_iter().map(|id| (id, "screenshot")));
        ops.sort_unstable();
        for (link_id, op) in ops {
            state.blobs.forget(link_id);
            let lost = LinkError::new(code::HOST_LINK_LOST, text::link_lost(&self.deps.label, op));
            Self::queue(state, &HelperFrame::Re(error_reply(link_id, lost)));
        }
        if let Some(session) = state.session.as_mut().filter(|s| s.host_up) {
            session.host_up = false;
            // B's lease goes with it: nothing is relayed until the next host_ok
            session.host_ok = None;
            session.queue.push(&HelperFrame::HostDown {
                reason: reason.into(),
            });
            if state.link == LinkStatus::Linked {
                state.link = LinkStatus::Reconnecting;
            }
        }
        Self::pump(state);
    }

    fn sidecar_lost(&self, why: &str) {
        let mut state = self.state();
        if state.attached.is_none() {
            return;
        }
        tracing::warn!(%why, "the browser's sidecar is gone");
        self.host_gone(&mut state, why);
    }

    /// The sidecar attached again by itself: `host_up` for the live lease.
    fn sidecar_attached(&self, answer: &Value) {
        let mut state = self.state();
        if state.attached.is_some() {
            return;
        }
        let Some(lease) = &state.lease else {
            return;
        };
        state.attached = Some(Attached {
            instance: lease.instance.clone(),
            version: lease.version.clone(),
            electron: lease.electron.clone(),
            tabs: tabs_of(answer),
        });
        self.send_host_up(&mut state);
        drop(state);
        self.changed.notify_all();
    }

    /// The companion's process is gone: its lease and sidecar with it.
    fn companion_exited(&self, reason: &str) {
        let mut state = self.state();
        if state.lease.take().is_none() && state.attached.is_none() {
            return;
        }
        tracing::info!(%reason, "the machine's companion exited");
        self.host_gone(&mut state, reason);
    }

    // -----------------------------------------------------------------------
    // The companion's API

    fn lease_live(&self, state: &State, lease_id: &str) -> bool {
        state
            .lease
            .as_ref()
            .is_some_and(|lease| lease.id == lease_id && lease.expires > self.now())
    }

    fn link_field(state: &State) -> Value {
        match &state.link {
            LinkStatus::Linked => json!({"state": "linked"}),
            LinkStatus::InUse(text) => json!({"state": "in_use", "text": text}),
            LinkStatus::Off(text) => json!({"state": "reconnecting", "text": text}),
            LinkStatus::Connecting | LinkStatus::Reconnecting => json!({"state": "reconnecting"}),
        }
    }

    /// `update` is the companion's, asked before the helper's lock.
    fn lease_answer(&self, state: &State, update: Option<String>) -> Answer {
        let Some(lease) = &state.lease else {
            return lease_unknown();
        };
        let expires_unix =
            crate::browser::unix_now() + lease.expires.saturating_sub(self.now()).as_secs();
        let info = BrowserHostLeaseInfo {
            lease_id: lease.id.clone(),
            profile: COMPANION_PROFILE.into(),
            state: "attached".into(),
            heartbeat_ms: HEARTBEAT_MS,
            ttl_ms: self.deps.timing.lease_ttl.as_millis() as u64,
            expires_unix,
            host_protocol: HOST_PROTOCOL,
            server_features: SERVER_FEATURES.iter().map(|f| (*f).to_owned()).collect(),
            // only the helper's own launch registers: it is always managed
            update,
            view_cycle_keys: state.cycle_keys.clone(),
        };
        let mut lease = match serde_json::to_value(info) {
            Ok(lease) => lease,
            Err(err) => return Answer::error("internal_error", err.to_string()),
        };
        lease["link"] = Self::link_field(state);
        Answer::Result(json!({"type": "browser_host_lease", "lease": lease}))
    }

    fn host_register(&self, params: Value) -> Answer {
        let params: BrowserHostRegisterParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(err) => return Answer::error("invalid_request", err.to_string()),
        };
        if params.protocol != HOST_PROTOCOL {
            return Answer::error(
                "host_protocol_unsupported",
                format!(
                    "host protocol {} is not supported; this herdr speaks {HOST_PROTOCOL}",
                    params.protocol
                ),
            );
        }
        if params.kind == BrowserHostKind::Unknown {
            return Answer::error("invalid_request", "unknown host kind");
        }
        if params.instance.trim().is_empty() || params.nonce.trim().is_empty() {
            return Answer::error("invalid_request", "instance and nonce are required");
        }
        if !matches!(
            params.cdp.host.trim(),
            "127.0.0.1" | "localhost" | "::1" | "[::1]"
        ) || params.cdp.port == 0
        {
            return Answer::error(
                "invalid_request",
                "the DevTools endpoint must be a loopback address with a port",
            );
        }
        {
            let mut state = self.state();
            if let Some(lease) = &state.lease {
                if lease.expires > self.now() && lease.instance != params.instance {
                    return Answer::error(
                        "host_in_use",
                        "another companion is registered for this machine; close it first",
                    );
                }
            }
            // the sidecar attaches to this launch now: the old one's pages go;
            // without a lease, the attach hook of this attach says nothing
            // (the registration says `host_up` itself)
            state.lease = None;
            if state.attached.is_some() {
                self.host_gone(&mut state, "the companion registered again");
            }
        }
        let answer = match self.deps.companion.register(&params) {
            Ok(answer) => answer,
            Err(error) => return Answer::error(error.code(), error.message()),
        };
        let info = self.deps.companion.info();
        let electron = info.electron.clone().unwrap_or_default();
        let update = update_of(&info);
        let token = params
            .launch_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
        {
            let mut state = self.state();
            state.lease = Some(Lease {
                id: format!("L{}", crate::browser::session::random_hex(24)),
                instance: params.instance.clone(),
                version: params.version.clone(),
                electron: electron.clone(),
                launch_token: token,
                expires: self.now() + self.deps.timing.lease_ttl,
            });
            state.attached = Some(Attached {
                instance: params.instance.clone(),
                version: params.version.clone(),
                electron,
                tabs: tabs_of(&answer),
            });
            self.send_host_up(&mut state);
        }
        self.changed.notify_all();
        tracing::info!(instance = %params.instance, "the machine's companion registered");
        // the lease says linked when B takes it at once
        let started = Instant::now();
        let mut state = self.state();
        while state.session.as_ref().is_some_and(|s| s.host_ok.is_none()) {
            let left = self
                .deps
                .timing
                .host_ok_wait
                .saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        self.lease_answer(&state, update)
    }

    fn host_heartbeat(&self, params: &Value) -> Answer {
        let lease_id = lease_id(params);
        let token = {
            let state = self.state();
            if !self.lease_live(&state, lease_id) {
                return lease_unknown();
            }
            state.lease.as_ref().and_then(|l| l.launch_token.clone())
        };
        // the companion's own idea of its registration, asked unlocked
        let current = token
            .as_deref()
            .is_none_or(|token| self.deps.companion.heartbeat(token));
        let update = update_of(&self.deps.companion.info());
        let now = self.now();
        let mut state = self.state();
        if !current {
            if state.lease.as_ref().is_some_and(|l| l.id == lease_id) {
                state.lease = None;
                self.host_gone(
                    &mut state,
                    "the companion's registration is not the current one",
                );
            }
            return lease_unknown();
        }
        match state.lease.as_mut() {
            Some(lease) if lease.id == lease_id => lease.expires = now + self.deps.timing.lease_ttl,
            _ => return lease_unknown(),
        }
        self.lease_answer(&state, update)
    }

    fn host_unregister(&self, params: &Value) -> Answer {
        let mut state = self.state();
        if !self.lease_live(&state, lease_id(params)) {
            return lease_unknown();
        }
        let lease = state.lease.take();
        self.host_gone(&mut state, "the companion closed");
        drop(state);
        if let Some(token) = lease.as_ref().and_then(|l| l.launch_token.as_deref()) {
            self.deps.companion.unregistered(token);
        }
        tracing::info!("the machine's companion unregistered");
        Answer::ok()
    }

    /// A lease that was not renewed in time goes, as herdr's does.
    fn expire_lease(&self) {
        let mut state = self.state();
        if state
            .lease
            .as_ref()
            .is_none_or(|lease| lease.expires > self.now())
        {
            return;
        }
        let lease = state.lease.take();
        self.host_gone(&mut state, "the companion stopped heartbeating");
        drop(state);
        tracing::warn!(instance = ?lease.map(|l| l.instance), "the machine's companion lease expired");
    }

    fn companion_call(&self, params: &Value) -> Answer {
        let action = params
            .get("action")
            .cloned()
            .and_then(|action| serde_json::from_value(action).ok())
            .unwrap_or(BrowserCompanionAction::Unknown);
        let done = match action {
            BrowserCompanionAction::Start => self.deps.companion.start(),
            BrowserCompanionAction::Stop => {
                self.deps.companion.stop();
                Ok(())
            }
            BrowserCompanionAction::Restart => self.deps.companion.restart(),
            BrowserCompanionAction::Focus => self.deps.companion.focus(),
            BrowserCompanionAction::Status => Ok(()),
            BrowserCompanionAction::Unknown => {
                return Answer::error("invalid_params", "unknown companion action")
            }
        };
        match done {
            Ok(()) => Answer::Result(
                json!({"type": "browser_companion", "companion": self.deps.companion.info()}),
            ),
            Err(why) => Answer::error("companion_failed", why),
        }
    }

    fn placements(&self, params: &Value) -> Answer {
        if !self.lease_live(&self.state(), lease_id(params)) {
            return lease_unknown();
        }
        let since = params.get("since_rev").and_then(Value::as_u64);
        let wait = params
            .get("wait_ms")
            .and_then(Value::as_u64)
            .map_or(Duration::ZERO, Duration::from_millis);
        let snapshot = self.board.wait(since, wait, &self.running);
        Answer::Result(json!({
            "type": "browser_placements",
            "boot_id": snapshot.boot_id,
            "rev": snapshot.rev,
            "placements": snapshot.placements,
        }))
    }

    fn placement_report(&self, params: &Value) -> Answer {
        if !self.lease_live(&self.state(), lease_id(params)) {
            return lease_unknown();
        }
        let field = |name: &str| params.get(name).and_then(Value::as_str);
        let (Some(id), Some(state)) = (field("id"), field("state")) else {
            return Answer::error("invalid_params", "id and state are required");
        };
        self.board.report(id, state, field("reason"));
        Answer::ok()
    }

    fn boot_id(&self) -> String {
        self.board.wait(None, Duration::ZERO, &self.running).boot_id
    }

    fn placement_action(&self, params: &Value) -> Answer {
        if !self.lease_live(&self.state(), lease_id(params)) {
            return lease_unknown();
        }
        let action = params
            .get("action")
            .cloned()
            .and_then(|action| serde_json::from_value(action).ok())
            .unwrap_or(BrowserPlacementAction::Unknown);
        if action == BrowserPlacementAction::Unknown {
            return Answer::error(
                "invalid_params",
                "unknown placement action (promote, restore or close)",
            );
        }
        let id = params.get("id").and_then(Value::as_str).unwrap_or_default();
        match self.board.queue_action(id, action) {
            Ok(revision) => {
                let boot_id = self.boot_id();
                if let Some(clients) = self.clients() {
                    for client in self.board.client_ids() {
                        for record in self.board.actions_for(client, revision - 1) {
                            clients.send(
                                client,
                                &HelperMessage::PlacementAction(PlacementActionPayload {
                                    boot_id: boot_id.clone(),
                                    revision: record.revision,
                                    id: record.id,
                                    tab_id: record.tab_id,
                                    action: record.action,
                                }),
                            );
                        }
                    }
                }
                Answer::ok()
            }
            Err(ActionRefused::NotFound) => Answer::error(
                "not_found",
                format!("no client holds placement {}", id.trim()),
            ),
            Err(ActionRefused::Unsupported) => Answer::error(
                "unsupported",
                "this herdr client predates the bar's expand and close",
            ),
        }
    }

    fn view_cycle(&self, params: &Value) -> Answer {
        if !self.lease_live(&self.state(), lease_id(params)) {
            return lease_unknown();
        }
        let field = |name: &str| params.get(name).and_then(Value::as_str);
        match self.board.queue_view_cycle(field("id"), field("tab_id")) {
            Ok(revision) => {
                let boot_id = self.boot_id();
                let targets = self.state().clients.clone();
                if let Some(clients) = self.clients() {
                    for client in targets {
                        for record in self.board.view_cycles_for(client, revision - 1) {
                            clients.send(
                                client,
                                &HelperMessage::ViewCycle(ViewCyclePayload {
                                    boot_id: boot_id.clone(),
                                    revision: record.revision,
                                    tab_id: record.tab_id,
                                }),
                            );
                        }
                    }
                }
                Answer::ok()
            }
            Err(ActionRefused::NotFound) => {
                Answer::error("not_found", "no herdr client to take the cycle")
            }
            Err(ActionRefused::Unsupported) => Answer::error(
                "unsupported",
                "this herdr client predates the browser's cycle key",
            ),
        }
    }

    fn link_down(&self) -> Answer {
        Answer::error(
            code::LINK_DOWN,
            format!(
                "the connection to {} is down; it reconnects by itself",
                self.deps.machine
            ),
        )
    }

    fn link_file(&self, params: &Value) -> Answer {
        let pull = match Pull::parse(params) {
            Ok(pull) => pull,
            Err(why) => return Answer::error("invalid_params", why),
        };
        let (tx, rx) = mpsc::channel();
        let ticket = {
            let mut state = self.state();
            if !Self::linked(&state) {
                return self.link_down();
            }
            let Some(ticket) = state.files.ask(pull) else {
                return Answer::error(
                    "host_busy",
                    "16 pieces of shown files are already on the way",
                );
            };
            state.waiters.files.insert(ticket, tx);
            Self::pump(&mut state);
            ticket
        };
        match rx.recv_timeout(FILE_TIMEOUT) {
            Ok(Some(reply)) => Answer::Result(file_answer(&reply)),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => self.link_down(),
            Err(RecvTimeoutError::Timeout) => {
                let mut state = self.state();
                state.files.cancel(ticket);
                state.waiters.files.remove(&ticket);
                Answer::error("timeout", "the shown file did not come in time")
            }
        }
    }

    fn subscribe(&self) -> Answer {
        let (tx, rx) = mpsc::channel();
        self.state().streams.push(tx);
        Answer::Stream(json!({"type": "subscription_started"}), rx)
    }

    fn relay(&self, method: &str, mut params: Value) -> Answer {
        if let Some(params) = params.as_object_mut() {
            // B puts its own lease in
            params.remove("lease_id");
        }
        let (tx, rx) = mpsc::channel();
        let id = {
            let mut state = self.state();
            if !Self::linked(&state) {
                return self.link_down();
            }
            state.next_api_id += 1;
            let id = state.next_api_id;
            state.waiters.api.insert(id, tx);
            Self::queue(
                &state,
                &HelperFrame::Api(ApiRequest {
                    id,
                    method: method.to_owned(),
                    params,
                }),
            );
            id
        };
        match rx.recv_timeout(RELAY_TIMEOUT) {
            Ok(Some(reply)) if reply.ok => Answer::Result(reply.result),
            Ok(Some(reply)) => {
                let error = reply.error.unwrap_or_default();
                // B's lease is not the companion's: never make it register again
                if error.code == "lease_unknown" {
                    self.link_down()
                } else {
                    Answer::Error {
                        code: error.code,
                        message: error.message,
                    }
                }
            }
            Ok(None) | Err(RecvTimeoutError::Disconnected) => self.link_down(),
            Err(RecvTimeoutError::Timeout) => {
                self.state().waiters.api.remove(&id);
                Answer::error("timeout", format!("{method} timed out on the link"))
            }
        }
    }

    fn call(&self, method: &str, params: Value) -> Answer {
        match method {
            "browser.host_register" => self.host_register(params),
            "browser.host_heartbeat" => self.host_heartbeat(&params),
            "browser.host_unregister" => self.host_unregister(&params),
            "browser.companion" => self.companion_call(&params),
            "browser.placements" => self.placements(&params),
            "browser.placement_report" => self.placement_report(&params),
            "browser.placement_action" => self.placement_action(&params),
            "browser.view_cycle" => self.view_cycle(&params),
            "link.file" => self.link_file(&params),
            "events.subscribe" => self.subscribe(),
            method if API_METHODS.contains(&method) => self.relay(method, params),
            method => Answer::error(
                "invalid_request",
                format!(
                    "{} is not available to a remote machine's browser",
                    method.chars().take(64).collect::<String>()
                ),
            ),
        }
    }

    // -----------------------------------------------------------------------
    // Clients

    fn view(&self) -> LinkView {
        let link = self.state().link.clone();
        let (link, owner, text) = match link {
            LinkStatus::Connecting => ("connecting", None, None),
            LinkStatus::Linked => ("linked", None, None),
            LinkStatus::Reconnecting => (
                "reconnecting",
                None,
                Some(format!("{}: reconnecting", self.deps.machine)),
            ),
            LinkStatus::InUse(text) => ("in_use", Some(text.clone()), Some(text)),
            LinkStatus::Off(text) => ("off", None, Some(text)),
        };
        LinkView {
            link: link.into(),
            owner,
            text,
            companion: self.deps.companion.info(),
            reports: self.board.statuses(),
        }
    }

    /// Tells the clients what changed since the last time.
    fn tell_clients(&self) {
        let view = self.view();
        let mut state = self.state();
        if state.told.as_ref() == Some(&view) {
            return;
        }
        state.told = Some(view.clone());
        drop(state);
        if let Some(clients) = self.clients() {
            clients.broadcast(&HelperMessage::State(Box::new(view)));
        }
    }

    fn client_companion(&self, action: CompanionAction) {
        let companion = &self.deps.companion;
        let done = match action {
            CompanionAction::Start => companion.start(),
            CompanionAction::Stop => {
                companion.stop();
                Ok(())
            }
            CompanionAction::Restart => companion.restart(),
            CompanionAction::Focus => companion.focus(),
            // only after the user confirmed it (the client asks)
            CompanionAction::Forget => companion.forget().map_err(|err| err.to_string()),
            CompanionAction::Unknown => Ok(()),
        };
        if let Err(why) = done {
            tracing::info!(%why, ?action, "the companion did not take the user's action");
        }
    }

    // -----------------------------------------------------------------------
    // The process

    /// Runs the helper until its last client left [`Timing::idle_exit`]
    /// ago: the API socket at `api`, the clients at `link`, the link.
    pub(crate) fn serve(self: &Arc<Self>, api: &Path, link: &Path) -> io::Result<()> {
        let api = ApiSocket::bind(api, self.api_handler())?;
        let clients = Arc::new(Clients::bind(link, self.client_handler())?);
        self.set_clients(Arc::clone(&clients));
        let link_thread = {
            let helper = Arc::clone(self);
            std::thread::Builder::new()
                .name("browser-link".into())
                .spawn(move || helper.link_loop())?
        };
        while self.running() {
            std::thread::sleep(self.deps.timing.tick);
            self.expire_lease();
            self.tell_clients();
            if clients
                .idle_since()
                .is_some_and(|since| since.elapsed() >= self.deps.timing.idle_exit)
            {
                tracing::info!("the last client left; the browser link ends");
                self.stop();
            }
        }
        self.stop();
        let _ = link_thread.join();
        // the companion quits with its helper
        self.deps.companion.shutdown();
        self.deps.sidecar.shutdown();
        lock(&self.clients).take();
        drop(clients);
        drop(api);
        self.state().streams.clear();
        Ok(())
    }
}

fn lease_id(params: &Value) -> &str {
    params
        .get("lease_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn lease_unknown() -> Answer {
    let err = crate::browser::hub::lease_unknown();
    Answer::error(err.code(), err.message())
}

fn ok_reply(id: u64, result: Value) -> LinkReply {
    LinkReply {
        id,
        ok: true,
        result,
        page: None,
        error: None,
    }
}

fn error_reply(id: u64, error: LinkError) -> LinkReply {
    LinkReply {
        id,
        ok: false,
        result: Value::Null,
        page: None,
        error: Some(error),
    }
}

/// The lease's `update`: what a restart applies, while one waits.
fn update_of(info: &BrowserCompanionAppInfo) -> Option<String> {
    info.update_ready
        .then(|| info.update.clone().or_else(|| info.installed.clone()))
        .flatten()
}

/// The tabs of a sidecar's attach answer, for `host_up`.
fn tabs_of(answer: &Value) -> Vec<HostTab> {
    answer
        .get("tabs")
        .cloned()
        .and_then(|tabs| serde_json::from_value(tabs).ok())
        .unwrap_or_default()
}

/// The answer line of `browser.link_open`.
fn parse_answer(line: &[u8]) -> Result<BrowserLinkInfo, LinkError> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|err| LinkError::new(code::INVALID_REQUEST, format!("no answer: {err}")))?;
    if let Some(error) = value.get("error") {
        let field = |name: &str| error.get(name).and_then(Value::as_str).unwrap_or_default();
        return Err(LinkError::new(field("code"), field("message")));
    }
    value
        .get("result")
        .and_then(|result| result.get("link"))
        .cloned()
        .and_then(|link| serde_json::from_value(link).ok())
        .ok_or_else(|| LinkError::new(code::INVALID_REQUEST, "the answer has no link"))
}

impl ApiHandler for Weakly {
    fn call(&self, method: &str, params: Value) -> Answer {
        match self.0.upgrade() {
            Some(helper) => helper.call(method, params),
            None => Answer::error(code::LINK_DOWN, "the browser link ends"),
        }
    }
}

impl SidecarHooks for Weakly {
    fn event(&self, event: Value) {
        if let Some(helper) = self.0.upgrade() {
            helper.sidecar_event(event);
        }
    }
    fn attached(&self, answer: &Value) {
        if let Some(helper) = self.0.upgrade() {
            helper.sidecar_attached(answer);
        }
    }
    fn lost(&self, reason: &str) {
        if let Some(helper) = self.0.upgrade() {
            helper.sidecar_lost(reason);
        }
    }
}

impl CompanionHooks for Weakly {
    fn changed(&self) {}
    fn exited(&self, reason: &str) {
        if let Some(helper) = self.0.upgrade() {
            helper.companion_exited(reason);
        }
    }
}

/// The hooks the companion and the sidecar are made with, before the helper
/// they call exists: [`Hooks::bind`] it once it does.
#[derive(Default)]
pub(crate) struct Hooks(std::sync::OnceLock<Weakly>);

impl Hooks {
    pub(crate) fn bind(&self, helper: &Arc<Helper>) {
        let _ = self.0.set(helper.weakly());
    }
}

impl SidecarHooks for Hooks {
    fn event(&self, event: Value) {
        if let Some(helper) = self.0.get() {
            helper.event(event);
        }
    }
    fn attached(&self, answer: &Value) {
        if let Some(helper) = self.0.get() {
            helper.attached(answer);
        }
    }
    fn lost(&self, reason: &str) {
        if let Some(helper) = self.0.get() {
            SidecarHooks::lost(helper, reason);
        }
    }
}

impl CompanionHooks for Hooks {
    fn changed(&self) {}
    fn exited(&self, reason: &str) {
        if let Some(helper) = self.0.get() {
            helper.exited(reason);
        }
    }
}

impl ClientHandler for Weakly {
    fn hello(&self, client: u64, _machine: &MachineFacts, cycle_keys: Vec<String>) {
        let Some(helper) = self.0.upgrade() else {
            return;
        };
        helper.board.set_takes_actions(client, true);
        helper.board.set_takes_view_cycle(client, true);
        {
            let mut state = helper.state();
            state.cycle_keys = cycle_keys;
            state.clients.push(client);
        }
        let view = helper.view();
        if let Some(clients) = helper.clients() {
            clients.send(client, &HelperMessage::State(Box::new(view)));
        }
    }

    fn placement(
        &self,
        client: u64,
        spec: Option<crate::api::schema::BrowserPlacementSpec>,
        active: bool,
        focused: bool,
    ) {
        let Some(helper) = self.0.upgrade() else {
            return;
        };
        let facts = ClientFacts {
            focused,
            foreground: focused && active,
            surface_active: active,
        };
        helper
            .board
            .set(client, spec, facts, crate::browser::unix_now());
        if focused && active {
            helper.board.set_foreground(Some(client));
        }
    }

    fn companion(&self, _client: u64, action: CompanionAction) {
        let Some(helper) = self.0.upgrade() else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("browser-link-companion".into())
            .spawn(move || helper.client_companion(action));
    }

    fn gone(&self, client: u64) {
        if let Some(helper) = self.0.upgrade() {
            helper.board.remove(client);
            helper.state().clients.retain(|id| *id != client);
        }
    }
}

// ---------------------------------------------------------------------------
// The machine's home

/// Holds the machine home's `lock` (flock) for the helper's life: one helper
/// per machine and session.
pub(crate) struct HomeLock(std::fs::File);

/// `Ok(None)`: another helper holds it.
pub(crate) fn lock_home(home: &Path) -> io::Result<Option<HomeLock>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(home.join("lock"))?;
    // SAFETY: a valid descriptor of a file this function owns.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if locked {
        return Ok(Some(HomeLock(file)));
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}

/// The machine's home, 0700: `<browser home>/companion-app/machines/<key>`.
pub(crate) fn machine_home(key: &str) -> io::Result<PathBuf> {
    let home = crate::browser::companion_install::companion_home(&crate::browser::browser_home())
        .join("machines")
        .join(key);
    std::fs::create_dir_all(&home)?;
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    Ok(home)
}

/// This Mac's id for the link (`state_dir/client/browser-link/id`), made
/// once.
pub(crate) fn host_id(state_dir: &Path) -> io::Result<String> {
    let dir = state_dir.join("client").join("browser-link");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(id.to_owned());
        }
    }
    let id = crate::browser::session::random_hex(32);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "{id}")?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// The real parts: ssh, the proxy

/// The link over the client's shared ssh connection.
pub(crate) struct SshTransport(pub(crate) Arc<crate::remote::SavedSshLink>);

impl LinkTransport for SshTransport {
    fn open(&self) -> io::Result<LinkConnection> {
        let child = super::ssh::open_link(&self.0)?;
        let process = child.process;
        Ok(LinkConnection {
            reader: Box::new(child.reader),
            writer: child.writer,
            end: Box::new(move || {
                let end = process.finish(Duration::from_secs(1));
                tracing::debug!(code = ?end.code, stderr = %end.stderr, "the browser link's ssh ended");
            }),
        })
    }
}

impl super::proxy::Classifier
    for super::classify::Classifier<
        super::classify::SystemResolver,
        super::classify::SystemInterfaces,
    >
{
    fn canonical(&self, host: &str) -> Result<String, String> {
        super::classify::Host::parse(host)
            .map(|host| host.to_string())
            .map_err(|refusal| refusal.text().to_owned())
    }

    fn route(&self, host: &str, port: u16) -> super::proxy::Route {
        match super::classify::Classifier::route(self, host, port) {
            super::classify::Route::Direct(pinned) => {
                super::proxy::Route::Direct(pinned.addresses().to_vec())
            }
            super::classify::Route::Remote => super::proxy::Route::Remote,
            super::classify::Route::Refuse(refusal) => {
                super::proxy::Route::Refuse(refusal.text().to_owned())
            }
        }
    }
}

/// The browser's proxy as the companion's launcher binds it.
pub(crate) struct HelperProxy {
    pub(crate) label: String,
    /// The warm-up answer's `x-herdr-proxy`: random per helper.
    pub(crate) instance: String,
    /// `egress = "remote"`.
    pub(crate) remote_only: bool,
    pub(crate) dial: Arc<dyn super::forward::Dial>,
    pub(crate) proxy: Mutex<Option<super::proxy::Proxy>>,
}

impl HelperProxy {
    pub(crate) fn unbind(&self) {
        lock(&self.proxy).take();
    }
}

impl ProxyBinder for HelperProxy {
    fn bind(&self, port: u16, secret: &str) -> io::Result<u16> {
        let mut proxy = lock(&self.proxy);
        // the old one lets go of its port first
        proxy.take();
        let started = super::proxy::Proxy::start(
            port,
            super::proxy::ProxyOptions {
                label: self.label.clone(),
                instance: self.instance.clone(),
                secret: secret.to_owned(),
                remote_only: self.remote_only,
                classifier: Arc::new(super::classify::Classifier::system()),
                dial: Arc::clone(&self.dial),
            },
        )?;
        let port = started.port();
        *proxy = Some(started);
        Ok(port)
    }
}

/// What the companion and the sidecar (C5) are made from: the machine's
/// facts, the proxy the launcher binds and the hooks into the helper.
pub(crate) struct MachineParts<'a> {
    pub(crate) key: &'a str,
    /// `<browser home>/companion-app/machines/<key>`, 0700, locked.
    pub(crate) home: &'a Path,
    /// The companion's `--socket`.
    pub(crate) api_socket: &'a Path,
    pub(crate) label: &'a str,
    pub(crate) proxy: Arc<dyn ProxyBinder>,
    pub(crate) hooks: Arc<Hooks>,
}

pub(crate) type PartsFactory =
    fn(&MachineParts<'_>) -> io::Result<(Arc<dyn CompanionControl>, Arc<dyn SidecarOps>)>;

/// `herdr browser-link <machine>`: the machine's catalog id or label. Exits
/// 0 when another helper serves it.
pub(crate) fn main(machine: &str, parts: PartsFactory) -> io::Result<()> {
    use super::forward::{Forwards, Master};
    use super::ssh::{machine_key, machine_ssh, socket_dir, BrowserMaster, MASTERS};

    let profiles =
        crate::client::endpoint::EndpointCatalog::load_profiles().map_err(io::Error::other)?;
    let profile = profiles
        .iter()
        .find(|profile| profile.id.as_str() == machine)
        .or_else(|| profiles.iter().find(|profile| profile.label == machine))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no machine {machine}")))?;
    let key = machine_key(
        &crate::config::state_dir(),
        &profile.target,
        &profile.session,
    );
    let home = machine_home(&key)?;
    let Some(_lock) = lock_home(&home)? else {
        tracing::info!("another helper serves this machine");
        return Ok(());
    };
    let dir = socket_dir()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let link = machine_ssh(
        profile.id.as_str(),
        &profile.target,
        &profile.session,
        Arc::clone(&cancelled),
    )?;
    let masters: Vec<Arc<dyn Master>> = (0..MASTERS)
        .map(|index| {
            Arc::new(BrowserMaster::new(Arc::clone(&link), &dir, &key, index)) as Arc<dyn Master>
        })
        .collect();
    let forwards = Arc::new(Forwards::new(&dir, &home, &key, masters)?);
    let proxy = Arc::new(HelperProxy {
        label: profile.label.clone(),
        instance: crate::browser::session::random_hex(16),
        remote_only: false,
        dial: Arc::clone(&forwards) as Arc<dyn super::forward::Dial>,
        proxy: Mutex::new(None),
    });
    let api_socket = dir.join(format!("{key}.api"));
    let hooks = Arc::new(Hooks::default());
    let (companion, sidecar) = parts(&MachineParts {
        key: &key,
        home: &home,
        api_socket: &api_socket,
        label: &profile.label,
        proxy: Arc::clone(&proxy) as Arc<dyn ProxyBinder>,
        hooks: Arc::clone(&hooks),
    })?;
    let config = Arc::new(Mutex::new((
        Instant::now(),
        Policy::from(&crate::config::Config::load().config.browser),
    )));
    let policy = move || {
        let mut cached = lock(&config);
        if cached.0.elapsed() >= Duration::from_secs(5) {
            *cached = (
                Instant::now(),
                Policy::from(&crate::config::Config::load().config.browser),
            );
        }
        cached.1
    };
    let on_link = {
        let forwards = Arc::clone(&forwards);
        move |up: bool| forwards.set_link(up)
    };
    let helper = Helper::new(Deps {
        label: crate::platform::hostname()
            .and_then(|name| name.split('.').next().map(str::to_owned))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "the user's Mac".into()),
        machine: profile.label.clone(),
        host_id: host_id(&crate::config::state_dir())?,
        herdr: env!("CARGO_PKG_VERSION").into(),
        companion,
        sidecar,
        transport: Arc::new(SshTransport(link)),
        policy: Arc::new(policy),
        clock: Arc::new(|| Duration::from_nanos(super::ssh::clock_ns().unwrap_or(0))),
        on_link: Arc::new(on_link),
        timing: Timing::default(),
    });
    hooks.bind(&helper);
    helper.deps.companion.recover();
    let served = helper.serve(&api_socket, &dir.join(format!("{key}.link")));
    forwards.close();
    proxy.unbind();
    cancelled.store(true, Ordering::Release);
    served
}

#[cfg(test)]
mod tests;
