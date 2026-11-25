//! The remote browser's link, the server's side (fork): `browser.link_open`
//! turns its API connection into the link ([`super::link_proto`] has the
//! frames) to the Mac that runs this server's browser.
//!
//! One link per server ([`LinkSlot`]): the same Mac (`host_id`) replaces its
//! own, another one is refused. The connection's thread reads (a 250 ms
//! timeout, never the API's 100 ms poll), a writer thread of its own writes
//! from a bounded queue in the order of [`WriteLane`], so no caller ever
//! waits for the socket. What the frames mean is the [`LinkHandler`]'s.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;
use interprocess::TryClone as _;

use super::link_proto::{
    bye, code, text, ApiReply, ApiRequest, BlobAck, BlobChunk, BlobName, BrowserLinkInfo,
    BrowserLinkOpenParams, BrowserLinkReady, Bye, EnsureReply, FileReply, FileRequest, FrameError,
    HelperFrame, HostUp, LineBuffer, LinkError, LinkReply, ServerFrame, WriteLane, BLOB_MAX_BYTES,
    FILE_PULLS, FILE_WINDOW, LINK_FEATURES, LINK_PROTOCOL, OPS_IN_FLIGHT, PING_MS, QUEUE_BYTES,
    READ_POLL_MS, TTL_MS,
};
use super::session::random_hex;
use super::BrowserError;
use crate::api::schema::ResponseResult;
use crate::ipc::{is_connection_closed_error, LocalStream};

/// Whether this platform's server takes a link; elsewhere `browser.link_open`
/// answers `link_disabled` and the endpoint capability is absent.
pub const LINK_SUPPORTED: bool = cfg!(unix);

/// The bytes that may wait in all: answers to the helper get as much again as
/// what the server asks ([`QUEUE_BYTES`]), so a queue full of ops never costs
/// a `pong`.
const ANSWER_BYTES: usize = 2 * QUEUE_BYTES;
/// How long the writer may still take for a `bye` once the link ended.
const BYE_GRACE: Duration = Duration::from_secs(1);
/// The blobs that may be under way: two pictures per op.
const BLOBS_OPEN: usize = 2 * OPS_IN_FLIGHT;
/// The longest name of the Mac or version, for texts.
const LABEL_MAX_CHARS: usize = 64;
/// The Mac's name when it gave none.
const NO_LABEL: &str = "the user's Mac";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why a link ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEnd {
    /// The connection closed or broke.
    Lost(String),
    /// The helper said goodbye.
    Bye(Bye),
    /// Nothing arrived for [`TTL_MS`].
    Silent,
    /// The helper broke the protocol.
    Protocol(String),
    /// The helper does not read: [`ANSWER_BYTES`] wait for it.
    Backlog,
    /// The same Mac opened a newer link.
    Replaced,
    /// The server stops or hands off.
    Stopping,
}

impl LinkEnd {
    /// The goodbye the helper gets, when there is something to say.
    fn bye(&self) -> Option<Bye> {
        let (code, message) = match self {
            LinkEnd::Protocol(why) => (bye::PROTOCOL, why.clone()),
            LinkEnd::Replaced => (bye::REPLACED, "this Mac opened a newer link".into()),
            LinkEnd::Stopping => (bye::STOPPING, "herdr stops there".into()),
            LinkEnd::Lost(_) | LinkEnd::Bye(_) | LinkEnd::Silent | LinkEnd::Backlog => return None,
        };
        Some(Bye {
            code: code.into(),
            message,
        })
    }
}

impl std::fmt::Display for LinkEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkEnd::Lost(why) => write!(f, "lost: {why}"),
            LinkEnd::Bye(bye) => write!(f, "bye {}: {}", bye.code, bye.message),
            LinkEnd::Silent => write!(f, "silent for {} s", TTL_MS / 1000),
            LinkEnd::Protocol(why) => write!(f, "protocol: {why}"),
            LinkEnd::Backlog => f.write_str("the helper does not read"),
            LinkEnd::Replaced => f.write_str("replaced"),
            LinkEnd::Stopping => f.write_str("the server stops"),
        }
    }
}

// ---------------------------------------------------------------------------
// The writer's queue

#[derive(Default)]
struct QueueState {
    /// Waiting lines by [`WriteLane`].
    lanes: [VecDeque<String>; 3],
    bytes: usize,
    /// The goodbye, after everything else was dropped.
    bye: Option<String>,
    ended: Option<LinkEnd>,
    ended_at: Option<Instant>,
}

/// The lines waiting for the writer thread.
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

/// Why a line was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotQueued {
    Ended,
    /// As much waits already as may.
    Full,
}

impl WriteQueue {
    /// Queue a line unless more than `most` bytes would wait.
    fn push(&self, lane: WriteLane, line: String, most: usize) -> Result<(), NotQueued> {
        let mut state = lock(&self.state);
        if state.ended.is_some() {
            return Err(NotQueued::Ended);
        }
        if state.bytes + line.len() > most {
            return Err(NotQueued::Full);
        }
        state.bytes += line.len();
        state.lanes[lane_index(lane)].push_back(line);
        self.changed.notify_all();
        Ok(())
    }

    /// Wait until fewer than `most` lines wait in `lane`; false once the link
    /// ended.
    fn wait_for_room(&self, lane: WriteLane, most: usize) -> bool {
        let mut state = lock(&self.state);
        loop {
            if state.ended.is_some() {
                return false;
            }
            if state.lanes[lane_index(lane)].len() < most {
                return true;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The next line in lane order; `None` once the link ended and its
    /// goodbye, if any, was taken.
    fn next(&self) -> Option<String> {
        let mut state = lock(&self.state);
        loop {
            if let Some(line) = state.lanes.iter_mut().find_map(VecDeque::pop_front) {
                state.bytes -= line.len();
                self.changed.notify_all();
                return Some(line);
            }
            if let Some(bye) = state.bye.take() {
                return Some(bye);
            }
            if state.ended.is_some() {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// The link ends now (the first reason stays): what waits is dropped,
    /// the goodbye goes out.
    fn end(&self, end: LinkEnd) {
        let mut state = lock(&self.state);
        if state.ended.is_some() {
            return;
        }
        for lane in &mut state.lanes {
            lane.clear();
        }
        state.bytes = 0;
        state.bye = end
            .bye()
            .and_then(|bye| ServerFrame::Bye(bye).encode().ok());
        state.ended = Some(end);
        state.ended_at = Some(Instant::now());
        self.changed.notify_all();
    }

    fn ended(&self) -> Option<LinkEnd> {
        lock(&self.state).ended.clone()
    }

    /// The link ended longer ago than [`BYE_GRACE`]: the writer gives up.
    fn grace_over(&self) -> bool {
        lock(&self.state)
            .ended_at
            .is_some_and(|at| at.elapsed() >= BYE_GRACE)
    }
}

// ---------------------------------------------------------------------------
// The link and its slot

/// One open link.
pub struct Link {
    pub link_id: String,
    pub epoch: u64,
    /// The Mac; the same one replaces its own link.
    pub host_id: String,
    /// The Mac's name, for texts.
    pub label: String,
    /// When it opened, the server's local time (`14:02`).
    since: String,
    features: Vec<String>,
    queue: WriteQueue,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("link_id", &self.link_id)
            .field("epoch", &self.epoch)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Link {
    /// Queue an answer to something the helper asked. A helper that asks but
    /// does not read the answers ends its link.
    pub fn answer(&self, frame: &ServerFrame) {
        match frame.encode() {
            Ok(line) => {
                if self.queue.push(frame.lane(), line, ANSWER_BYTES) == Err(NotQueued::Full) {
                    self.end(LinkEnd::Backlog);
                }
            }
            Err(err) => {
                tracing::warn!(event = "browser.link.encode", epoch = self.epoch, %err, "an answer is no frame; dropped")
            }
        }
    }

    /// End the link now; the first reason stays.
    pub fn end(&self, end: LinkEnd) {
        self.queue.end(end);
    }

    /// Why it ended, once it did.
    pub fn ended(&self) -> Option<LinkEnd> {
        self.queue.ended()
    }

    fn info(&self, boot_id: &str) -> BrowserLinkInfo {
        BrowserLinkInfo {
            link_id: self.link_id.clone(),
            epoch: self.epoch,
            boot_id: boot_id.to_string(),
            link_protocol: LINK_PROTOCOL,
            features: self.features.clone(),
            ping_ms: PING_MS,
            ttl_ms: TTL_MS,
        }
    }
}

/// What decides a `browser.link_open` besides its parameters.
#[derive(Debug, Clone, Default)]
pub struct OpenFacts {
    /// `[browser] enabled`.
    pub enabled: bool,
    /// [`LINK_SUPPORTED`].
    pub supported: bool,
    /// A companion on this machine holds the browser's lease.
    pub local_host: bool,
    /// This machine's name.
    pub machine: String,
    /// The local time now (`14:02`).
    pub now: String,
}

#[derive(Default)]
struct SlotState {
    epoch: u64,
    live: Option<Arc<Link>>,
}

/// The server's one link.
pub struct LinkSlot {
    boot_id: String,
    state: Mutex<SlotState>,
}

impl Default for LinkSlot {
    fn default() -> Self {
        Self {
            boot_id: random_hex(16),
            state: Mutex::default(),
        }
    }
}

impl LinkSlot {
    /// `browser.link_open`: refuse it, or make it the link (decided and
    /// stored under one lock). The same Mac's older link ends.
    fn open(
        &self,
        params: &BrowserLinkOpenParams,
        facts: &OpenFacts,
    ) -> Result<Arc<Link>, BrowserError> {
        if !facts.enabled {
            return Err(BrowserError::disabled());
        }
        if !facts.supported {
            return Err(BrowserError::new(
                code::LINK_DISABLED,
                "this herdr takes no browser link",
            ));
        }
        if params.link_protocol != LINK_PROTOCOL {
            return Err(BrowserError::new(
                code::LINK_PROTOCOL_UNSUPPORTED,
                format!(
                    "browser link protocol {} is not supported; this herdr speaks {LINK_PROTOCOL}",
                    params.link_protocol
                ),
            ));
        }
        let host_id = params.host_id.trim();
        if host_id.is_empty() || params.instance.trim().is_empty() {
            return Err(BrowserError::new(
                code::INVALID_REQUEST,
                "host_id and instance are required",
            ));
        }
        let mut state = lock(&self.state);
        if facts.local_host {
            return Err(BrowserError::new(
                code::LINK_IN_USE,
                local_host_text(&facts.machine),
            ));
        }
        if let Some(live) = state.live.as_ref() {
            if live.host_id != host_id {
                return Err(BrowserError::new(
                    code::LINK_IN_USE,
                    text::link_in_use(&facts.machine, &live.label, &live.since),
                ));
            }
            live.end(LinkEnd::Replaced);
        }
        state.epoch += 1;
        let label = clip(&params.label);
        let link = Arc::new(Link {
            link_id: format!("K{}", random_hex(24)),
            epoch: state.epoch,
            host_id: host_id.to_string(),
            label: if label.is_empty() {
                NO_LABEL.into()
            } else {
                label
            },
            since: facts.now.clone(),
            features: LINK_FEATURES
                .iter()
                .filter(|feature| params.features.iter().any(|f| f == *feature))
                .map(|feature| (*feature).to_string())
                .collect(),
            queue: WriteQueue::default(),
        });
        state.live = Some(Arc::clone(&link));
        Ok(link)
    }

    /// `link` is over: the slot is free, unless a newer link has it.
    fn close(&self, link: &Link) {
        let mut state = lock(&self.state);
        if state
            .live
            .as_ref()
            .is_some_and(|live| live.epoch == link.epoch)
        {
            state.live = None;
        }
    }
}

/// `link_in_use` while a companion on this machine serves the browser.
fn local_host_text(machine: &str) -> String {
    format!("the browser of {machine} is the companion that runs there; stop it first (herdr browser companion stop)")
}

/// A name from the helper, for texts: one line, at most [`LABEL_MAX_CHARS`].
fn clip(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(LABEL_MAX_CHARS)
        .collect()
}

/// This machine's name for texts (`buildbox`, without its domain).
fn machine_name() -> String {
    crate::platform::hostname()
        .and_then(|name| name.split('.').next().map(str::to_string))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// The local time (`14:02`).
fn local_hhmm() -> String {
    crate::platform::local_datetime()
        .map(|now| format!("{:02}:{:02}", now.hour(), now.minute()))
        .unwrap_or_else(|| "now".into())
}

// ---------------------------------------------------------------------------
// The frames

/// What the helper's frames do. Called on the link's reading thread, so it
/// must not wait, except [`LinkHandler::file`], which has a thread of its
/// own. The defaults serve nothing.
pub trait LinkHandler: Send + Sync {
    fn host_up(&self, _link: &Arc<Link>, _up: HostUp) {}
    fn host_down(&self, _link: &Arc<Link>, _reason: &str) {}
    fn ensure_reply(&self, _link: &Arc<Link>, _reply: EnsureReply) {}
    fn reply(&self, _link: &Arc<Link>, _reply: LinkReply) {}
    fn event(&self, _link: &Arc<Link>, _event: serde_json::Value) {}
    /// A chunk in its order; it is acknowledged when this returns.
    fn blob(&self, _link: &Arc<Link>, _chunk: &BlobChunk) {}
    fn file(&self, _link: &Arc<Link>, request: &FileRequest) -> FileReply {
        no_file(request.id)
    }
    fn api(&self, link: &Arc<Link>, request: ApiRequest) {
        link.answer(&ServerFrame::ApiRe(ApiReply {
            id: request.id,
            ok: false,
            result: serde_json::Value::Null,
            error: Some(LinkError::new(
                code::INVALID_REQUEST,
                format!(
                    "{} is not available over the browser link",
                    clip(&request.method)
                ),
            )),
        }));
    }
    /// The link is over; the slot is free again.
    fn closed(&self, _link: &Arc<Link>, _end: &LinkEnd) {}
}

/// No browser behind the link yet: nothing is served.
struct Unserved;

impl LinkHandler for Unserved {}

fn no_file(id: u64) -> FileReply {
    FileReply {
        id,
        status: 404,
        mime: String::new(),
        size: 0,
        data: Vec::new(),
        eof: true,
    }
}

/// The blobs under way: each chunk in its order, none past its size.
#[derive(Default)]
struct Blobs {
    open: HashMap<(u64, BlobName), BlobAt>,
}

#[derive(Debug, Clone, Copy)]
struct BlobAt {
    next: u64,
    got: u64,
    size: u64,
}

impl Blobs {
    fn check(&mut self, chunk: &BlobChunk) -> Result<(), String> {
        let wrong = |what: &str| {
            Err(format!(
                "blob of op {}, chunk {}: {what}",
                chunk.op, chunk.seq
            ))
        };
        if chunk.size > BLOB_MAX_BYTES {
            return wrong("too large");
        }
        let key = (chunk.op, chunk.name);
        let mut at = match self.open.get(&key) {
            Some(at) => *at,
            None if self.open.len() >= BLOBS_OPEN => return wrong("too many at once"),
            None => BlobAt {
                next: 0,
                got: 0,
                size: chunk.size,
            },
        };
        if chunk.seq != at.next {
            return wrong("out of order");
        }
        at.next += 1;
        at.got += chunk.data.len() as u64;
        let whole = at.got == at.size;
        if chunk.size != at.size || at.got > at.size || (chunk.last && !whole) {
            return wrong("not its size");
        }
        if chunk.last {
            self.open.remove(&key);
        } else {
            self.open.insert(key, at);
        }
        Ok(())
    }

    /// The op was answered: nothing of it follows.
    fn answered(&mut self, op: u64) {
        self.open.retain(|(id, _), _| *id != op);
    }
}

/// The reading side of one link.
struct LinkReader {
    link: Arc<Link>,
    handler: Arc<dyn LinkHandler>,
    heard: Instant,
    lines: LineBuffer,
    blobs: Blobs,
    /// The thread that reads shown files, once one was asked for.
    files: Option<SyncSender<FileRequest>>,
}

impl LinkReader {
    fn new(link: Arc<Link>, handler: Arc<dyn LinkHandler>, now: Instant) -> Self {
        Self {
            link,
            handler,
            heard: now,
            lines: LineBuffer::default(),
            blobs: Blobs::default(),
            files: None,
        }
    }

    /// Read until the link ends.
    fn run(
        &mut self,
        source: &mut impl Read,
        now: impl Fn() -> Instant,
        stopping: impl Fn() -> bool,
    ) -> LinkEnd {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match source.read(&mut buf) {
                Ok(0) => return LinkEnd::Lost("the connection closed".into()),
                Ok(read) => {
                    self.heard = now();
                    if let Some(end) = self.bytes(&buf[..read]) {
                        return end;
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return LinkEnd::Lost(err.to_string()),
            }
            if let Some(end) = self.link.ended() {
                return end;
            }
            if stopping() {
                return LinkEnd::Stopping;
            }
            if now().saturating_duration_since(self.heard) >= Duration::from_millis(TTL_MS) {
                return LinkEnd::Silent;
            }
        }
    }

    fn bytes(&mut self, bytes: &[u8]) -> Option<LinkEnd> {
        self.lines.push(bytes);
        loop {
            // a replaced link's frames do nothing any more
            if let Some(end) = self.link.ended() {
                return Some(end);
            }
            let frame = match self.lines.next_line() {
                Ok(Some(line)) => HelperFrame::decode(&line),
                Ok(None) => return None,
                Err(err) => Err(err),
            };
            let end = match frame {
                Ok(frame) => self.frame(frame),
                Err(err) => Some(LinkEnd::Protocol(protocol_text(&err))),
            };
            if end.is_some() {
                return end;
            }
        }
    }

    fn frame(&mut self, frame: HelperFrame) -> Option<LinkEnd> {
        let link = &self.link;
        match frame {
            HelperFrame::Ping { n } => link.answer(&ServerFrame::Pong { n }),
            HelperFrame::EnsureRe(reply) => self.handler.ensure_reply(link, reply),
            HelperFrame::HostUp(up) => self.handler.host_up(link, up),
            HelperFrame::HostDown { reason } => self.handler.host_down(link, &reason),
            HelperFrame::Re(reply) => {
                self.blobs.answered(reply.id);
                self.handler.reply(link, reply);
            }
            HelperFrame::Ev { event } => self.handler.event(link, event),
            HelperFrame::Blob(chunk) => {
                // a newer helper's picture is acknowledged and dropped
                if chunk.name != BlobName::Unknown {
                    if let Err(why) = self.blobs.check(&chunk) {
                        return Some(LinkEnd::Protocol(why));
                    }
                    self.handler.blob(link, &chunk);
                }
                link.answer(&ServerFrame::BlobAck(BlobAck {
                    op: chunk.op,
                    name: chunk.name,
                    seq: chunk.seq,
                }));
            }
            HelperFrame::File(request) => return self.file(request),
            HelperFrame::Api(request) => self.handler.api(link, request),
            HelperFrame::Bye(bye) => return Some(LinkEnd::Bye(bye)),
            HelperFrame::Unknown => {}
        }
        None
    }

    /// Hand a file request to the files thread. The helper keeps
    /// [`FILE_WINDOW`] on the link; more than [`FILE_PULLS`] waiting is none
    /// of its kind.
    fn file(&mut self, request: FileRequest) -> Option<LinkEnd> {
        let files = match &self.files {
            Some(files) => files,
            None => {
                let (tx, rx) = sync_channel::<FileRequest>(FILE_PULLS);
                let link = Arc::clone(&self.link);
                let handler = Arc::clone(&self.handler);
                let spawned =
                    crate::thread_spawn::spawn_named("herdr-browser-link-files", move || {
                        for request in rx {
                            if !link.queue.wait_for_room(WriteLane::Bulk, FILE_WINDOW) {
                                return;
                            }
                            let mut reply = handler.file(&link, &request);
                            reply.id = request.id;
                            link.answer(&ServerFrame::FileRe(reply));
                        }
                    });
                if let Err(err) = spawned {
                    tracing::warn!(event = "browser.link.files", %err, "no thread for a shown file");
                    self.link.answer(&ServerFrame::FileRe(no_file(request.id)));
                    return None;
                }
                self.files.insert(tx)
            }
        };
        match files.try_send(request) {
            Ok(()) => None,
            Err(TrySendError::Full(_)) => Some(LinkEnd::Protocol("too many file requests".into())),
            Err(TrySendError::Disconnected(_)) => {
                Some(LinkEnd::Lost("the files thread ended".into()))
            }
        }
    }
}

fn protocol_text(err: &FrameError) -> String {
    clip(&err.to_string())
}

// ---------------------------------------------------------------------------
// The connection

/// Write every line of the queue until the link ends; a line that is under
/// way then, and the goodbye, get [`BYE_GRACE`].
fn write_until_end(sink: &mut impl Write, link: &Link) {
    while let Some(line) = link.queue.next() {
        if let Err(err) = write_line(sink, line.as_bytes(), link) {
            link.end(LinkEnd::Lost(err.to_string()));
            return;
        }
    }
}

/// The socket's send timeout only lets this look whether the link ended.
fn write_line(sink: &mut impl Write, mut bytes: &[u8], link: &Link) -> io::Result<()> {
    while !bytes.is_empty() {
        match sink.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if link.queue.grace_over() {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
    }
    sink.flush()
}

/// The server process's link.
fn slot() -> &'static LinkSlot {
    static SLOT: OnceLock<LinkSlot> = OnceLock::new();
    SLOT.get_or_init(LinkSlot::default)
}

fn open_facts() -> OpenFacts {
    let hub = super::hub();
    OpenFacts {
        enabled: hub.config().enabled,
        supported: LINK_SUPPORTED,
        local_host: hub.companion_live(),
        machine: machine_name(),
        now: local_hhmm(),
    }
}

/// `browser.link_open` on its connection thread, which is the link's until
/// it ends. Answers what the API log says.
pub fn serve(
    stream: LocalStream,
    request_id: &str,
    params: &BrowserLinkOpenParams,
    stopping: impl Fn() -> bool,
) -> io::Result<&'static str> {
    serve_with(
        stream,
        request_id,
        params,
        slot(),
        &open_facts(),
        Arc::new(Unserved),
        stopping,
    )
}

fn serve_with(
    mut stream: LocalStream,
    request_id: &str,
    params: &BrowserLinkOpenParams,
    slot: &LinkSlot,
    facts: &OpenFacts,
    handler: Arc<dyn LinkHandler>,
    stopping: impl Fn() -> bool,
) -> io::Result<&'static str> {
    let link = match slot.open(params, facts) {
        Ok(link) => link,
        Err(err) => {
            tracing::info!(event = "browser.link.refused", code = %err.code, label = %clip(&params.label), "browser link refused");
            let line =
                super::serve::encode_error(request_id.to_string(), err.code(), err.message());
            return write_answer(&mut stream, &line).map(|()| "error");
        }
    };
    tracing::info!(event = "browser.link.open", epoch = link.epoch, label = %link.label, herdr = %clip(&params.herdr), ready = ready_text(params.ready), "browser link open");
    // also frees the slot should a handler panic
    let live = LiveLink {
        slot,
        link: Arc::clone(&link),
    };
    let (end, writer) = match run_link(&mut stream, request_id, &live, &handler, stopping) {
        Ok((end, writer)) => (end, Some(writer)),
        Err(err) => (LinkEnd::Lost(err.to_string()), None),
    };
    link.end(end.clone());
    drop(live);
    handler.closed(&link, &end);
    tracing::info!(event = "browser.link.closed", epoch = link.epoch, label = %link.label, %end, "browser link closed");
    if let Some(writer) = writer.filter(|_| end.bye().is_some()) {
        // only for the goodbye: BYE_GRACE and one send timeout at most
        let _ = writer.join();
    }
    Ok("link_closed")
}

/// The slot's link while its connection is served.
struct LiveLink<'a> {
    slot: &'a LinkSlot,
    link: Arc<Link>,
}

impl Drop for LiveLink<'_> {
    fn drop(&mut self) {
        self.link.end(LinkEnd::Lost("its connection ended".into()));
        self.slot.close(&self.link);
    }
}

/// Answer, then read and write until the link ends; the writer may still
/// have a goodbye to write.
fn run_link(
    stream: &mut LocalStream,
    request_id: &str,
    live: &LiveLink<'_>,
    handler: &Arc<dyn LinkHandler>,
    stopping: impl Fn() -> bool,
) -> io::Result<(LinkEnd, std::thread::JoinHandle<()>)> {
    let link = &live.link;
    let poll = Some(Duration::from_millis(READ_POLL_MS));
    stream.set_recv_timeout(poll)?;
    let mut sink = stream.try_clone()?;
    // the answer is the first line: no writer runs yet
    let answer = super::serve::encode_success(
        request_id.to_string(),
        ResponseResult::BrowserLinkOpened {
            link: link.info(&live.slot.boot_id),
        },
    );
    write_answer(stream, &answer)?;
    stream.set_send_timeout(poll)?;
    let writer = {
        let link = Arc::clone(link);
        crate::thread_spawn::spawn_named("herdr-browser-link-writer", move || {
            write_until_end(&mut sink, &link);
        })?
    };
    let mut reader = LinkReader::new(Arc::clone(link), Arc::clone(handler), Instant::now());
    Ok((reader.run(stream, Instant::now, stopping), writer))
}

fn write_answer(stream: &mut LocalStream, line: &str) -> io::Result<()> {
    let result = stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .and_then(|()| stream.flush());
    match result {
        Err(err) if is_connection_closed_error(&err) => Ok(()),
        result => result,
    }
}

fn ready_text(ready: BrowserLinkReady) -> &'static str {
    match ready {
        BrowserLinkReady::Ready => "ready",
        BrowserLinkReady::NotInstalled => "not_installed",
        BrowserLinkReady::Failed | BrowserLinkReady::Unknown => "failed",
    }
}

#[cfg(test)]
mod tests;
