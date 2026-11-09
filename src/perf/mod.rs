//! Fork (perf watch): always-on, lock-free performance counters and stall
//! heartbeats.
//!
//! Everything here is a static atomic touched with relaxed operations: no lock,
//! no allocation and at most one clock read on a hot path. Instrumented files
//! call one or two of these per loop iteration, render pass or sent frame;
//! nothing is called per byte or per pane.
//!
//! The statics are per process, so a live handoff (a new server process)
//! starts from zero; [`snapshot`] carries the process id, its start time and a
//! reset generation so a reader never mixes two processes' numbers.
//!
//! - [`Heartbeat`]: a loop marks when it stops waiting ([`Heartbeat::begin`])
//!   and when it goes back to waiting ([`Heartbeat::end`]); an idle loop
//!   blocked on events reads as not busy and never looks stalled. The
//!   [`watchdog`] thread reports a busy period longer than the loop's budget.
//! - [`Counter`] / [`MaxGauge`]: monotonic counts and window maxima.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) mod watchdog;

#[cfg(test)]
mod tests;

/// Server main loop budget before the watchdog reports a stall.
pub(crate) const SERVER_STALL_MS: u64 = 250;
/// Client frame loop budget before the watchdog reports a stall.
pub(crate) const CLIENT_STALL_MS: u64 = 500;
/// Per-client fanout slots; later clients share the overflow bucket.
pub(crate) const CLIENT_SLOTS: usize = 8;

static EPOCH: OnceLock<Instant> = OnceLock::new();
static STARTED_UNIX_MS: AtomicU64 = AtomicU64::new(0);
/// Reset seqlock: odd while a reset zeroes the counters; generation = value / 2.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static RESET_AT_NS: AtomicU64 = AtomicU64::new(0);

/// Monotonic nanoseconds since this process first asked (never 0 for a
/// caller that stores it as "busy", see [`Heartbeat::begin`]).
#[inline]
pub(crate) fn now_ns() -> u64 {
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Wall-clock nanoseconds since the Unix epoch (0 before it).
pub(crate) fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Records this process's start for [`snapshot`] (first call wins).
pub(crate) fn mark_process_start() {
    let _ = now_ns();
    let started = wall_ns() / 1_000_000;
    let _ = STARTED_UNIX_MS.compare_exchange(0, started, Ordering::Relaxed, Ordering::Relaxed);
}

/// A monotonic event count.
pub(crate) struct Counter(AtomicU64);

impl Counter {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    #[inline]
    pub(crate) fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn add(&self, value: u64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

/// The largest value recorded since the last [`MaxGauge::take`] or reset.
pub(crate) struct MaxGauge(AtomicU64);

impl MaxGauge {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    #[inline]
    pub(crate) fn record(&self, value: u64) {
        self.0.fetch_max(value, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Reads and clears the window maximum.
    pub(crate) fn take(&self) -> u64 {
        self.0.swap(0, Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

/// A timed section's count, total and maximum duration.
pub(crate) struct Timing {
    pub(crate) count: Counter,
    pub(crate) total_ns: Counter,
    pub(crate) max_ns: MaxGauge,
}

impl Timing {
    pub(crate) const fn new() -> Self {
        Self {
            count: Counter::new(),
            total_ns: Counter::new(),
            max_ns: MaxGauge::new(),
        }
    }

    /// Starts one timed section; the guard records it when dropped, so an
    /// early return is counted too.
    #[inline]
    pub(crate) fn start(&'static self) -> TimingGuard {
        TimingGuard {
            timing: self,
            started_ns: now_ns(),
        }
    }

    #[inline]
    pub(crate) fn record_ns(&self, elapsed_ns: u64) {
        self.count.inc();
        self.total_ns.add(elapsed_ns);
        self.max_ns.record(elapsed_ns);
    }

    fn reset(&self) {
        self.count.reset();
        self.total_ns.reset();
        self.max_ns.reset();
    }
}

#[must_use = "the section is recorded when the guard drops"]
pub(crate) struct TimingGuard {
    timing: &'static Timing,
    started_ns: u64,
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        self.timing
            .record_ns(now_ns().saturating_sub(self.started_ns));
    }
}

/// A loop's busy-since heartbeat. One thread (the loop) writes it; the
/// watchdog only reads.
pub(crate) struct Heartbeat {
    name: &'static str,
    threshold_ms: u64,
    /// [`now_ns`] when the loop stopped waiting; 0 while it waits.
    busy_since_ns: AtomicU64,
    /// Stall episodes the watchdog reported.
    pub(crate) stalls: Counter,
    /// The longest busy period the watchdog observed in a reported episode.
    pub(crate) longest_stall_ms: MaxGauge,
}

impl Heartbeat {
    pub(crate) const fn new(name: &'static str, threshold_ms: u64) -> Self {
        Self {
            name,
            threshold_ms,
            busy_since_ns: AtomicU64::new(0),
            stalls: Counter::new(),
            longest_stall_ms: MaxGauge::new(),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn threshold_ms(&self) -> u64 {
        self.threshold_ms
    }

    /// The loop stopped waiting. Keeps the earlier mark while the loop is
    /// still busy (an iteration that `continue`s back to the top without
    /// waiting is the same busy period), so it costs one load then.
    #[inline]
    pub(crate) fn begin(&self) {
        if self.busy_since_ns.load(Ordering::Relaxed) == 0 {
            self.begin_at(now_ns());
        }
    }

    /// [`Heartbeat::begin`] with an injected clock (tests and the watchdog's
    /// own unit tests).
    #[inline]
    pub(crate) fn begin_at(&self, now_ns: u64) {
        if self.busy_since_ns.load(Ordering::Relaxed) == 0 {
            self.busy_since_ns.store(now_ns.max(1), Ordering::Relaxed);
        }
    }

    /// The loop is about to wait for its next event.
    #[inline]
    pub(crate) fn end(&self) {
        self.busy_since_ns.store(0, Ordering::Relaxed);
    }

    /// When the current busy period started, or 0 while the loop waits.
    pub(crate) fn busy_since_ns(&self) -> u64 {
        self.busy_since_ns.load(Ordering::Relaxed)
    }

    fn reset_counts(&self) {
        self.stalls.reset();
        self.longest_stall_ms.reset();
    }
}

/// One fanout slot: frames and bytes queued to one client.
pub(crate) struct ClientSlot {
    /// client id + 1; 0 while free.
    key: AtomicU64,
    frames: Counter,
    bytes: Counter,
}

impl ClientSlot {
    const fn new() -> Self {
        Self {
            key: AtomicU64::new(0),
            frames: Counter::new(),
            bytes: Counter::new(),
        }
    }
}

/// Server-side counters (the headless server process).
pub(crate) mod server {
    use super::{ClientSlot, Counter, Heartbeat, Timing, CLIENT_SLOTS, SERVER_STALL_MS};

    /// The headless server's main loop.
    pub(crate) static LOOP: Heartbeat = Heartbeat::new("server_loop", SERVER_STALL_MS);
    /// Frames accepted by a client's render channel (full and retained).
    pub(crate) static FRAMES_SENT: Counter = Counter::new();
    /// Bytes of those frames.
    pub(crate) static FRAME_BYTES: Counter = Counter::new();
    /// Frames a full client channel deferred (backpressure).
    pub(crate) static FRAMES_DEFERRED: Counter = Counter::new();
    /// Complete render passes (`render_and_stream`), with their duration.
    pub(crate) static FULL_RENDERS: Timing = Timing::new();
    /// Retained pane-surface passes that presented without a full render.
    pub(crate) static RETAINED_RENDERS: Counter = Counter::new();
    /// Retained pane-surface attempts that fell back to a full render.
    pub(crate) static RETAINED_FALLBACKS: Counter = Counter::new();
    /// Synchronized-output frames held for whole parsing.
    pub(crate) static SYNC_HOLDS: Counter = Counter::new();
    /// Holds released by their deadline instead of the closing sequence.
    pub(crate) static SYNC_HOLD_TIMEOUTS: Counter = Counter::new();

    pub(crate) static CLIENTS: [ClientSlot; CLIENT_SLOTS] =
        [const { ClientSlot::new() }; CLIENT_SLOTS];
    /// Clients past [`CLIENT_SLOTS`] share this bucket.
    pub(crate) static OTHER_CLIENTS: ClientSlot = ClientSlot::new();
}

/// Client-local counters (a TUI client process); never server fields.
pub(crate) mod client {
    use super::{Counter, Heartbeat, MaxGauge, CLIENT_STALL_MS};

    /// The client's frame loop.
    pub(crate) static LOOP: Heartbeat = Heartbeat::new("client_loop", CLIENT_STALL_MS);
    /// Frames and surface patches written to the host terminal.
    pub(crate) static FRAMES_PRESENTED: Counter = Counter::new();
    /// Total encode + write time of those frames.
    pub(crate) static FRAME_NS_TOTAL: Counter = Counter::new();
    /// Longest encode + write since the readout last looked.
    pub(crate) static FRAME_NS_MAX: MaxGauge = MaxGauge::new();
}

/// Counts one frame accepted by `client_id`'s render channel.
#[inline]
pub(crate) fn record_frame_sent(client_id: u64, bytes: usize) {
    let bytes = bytes as u64;
    server::FRAMES_SENT.inc();
    server::FRAME_BYTES.add(bytes);
    let slot = client_slot(client_id);
    slot.frames.inc();
    slot.bytes.add(bytes);
}

/// Counts one client frame (encode + write) that took `elapsed_ns`.
#[inline]
pub(crate) fn record_client_frame(elapsed_ns: u64) {
    client::FRAMES_PRESENTED.inc();
    client::FRAME_NS_TOTAL.add(elapsed_ns);
    client::FRAME_NS_MAX.record(elapsed_ns);
}

/// The slot that counts `client_id`: its own, a free one it claims, or the
/// overflow bucket. A scan of [`CLIENT_SLOTS`] relaxed loads per sent frame.
fn client_slot(client_id: u64) -> &'static ClientSlot {
    let key = client_id.wrapping_add(1).max(1);
    let slots = &server::CLIENTS;
    if let Some(slot) = slots
        .iter()
        .find(|slot| slot.key.load(Ordering::Relaxed) == key)
    {
        return slot;
    }
    for slot in slots {
        if slot
            .key
            .compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            slot.frames.reset();
            slot.bytes.reset();
            return slot;
        }
    }
    &server::OTHER_CLIENTS
}

/// Frees `client_id`'s fanout slot when the client leaves.
pub(crate) fn forget_client(client_id: u64) {
    let key = client_id.wrapping_add(1).max(1);
    for slot in &server::CLIENTS {
        if slot.key.load(Ordering::Relaxed) == key {
            slot.key.store(0, Ordering::Relaxed);
        }
    }
}

/// Per-client fanout totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClientFanout {
    /// `None` for the shared overflow bucket.
    pub(crate) client_id: Option<u64>,
    pub(crate) frames: u64,
    pub(crate) bytes: u64,
}

/// A loop's heartbeat state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoopSnapshot {
    pub(crate) name: &'static str,
    pub(crate) threshold_ms: u64,
    pub(crate) stalls: u64,
    pub(crate) longest_stall_ms: u64,
    /// How long the loop has been busy right now (0 while it waits).
    pub(crate) busy_ms: u64,
}

impl LoopSnapshot {
    fn of(heartbeat: &Heartbeat, now_ns: u64) -> Self {
        let busy_since = heartbeat.busy_since_ns();
        Self {
            name: heartbeat.name(),
            threshold_ms: heartbeat.threshold_ms(),
            stalls: heartbeat.stalls.get(),
            longest_stall_ms: heartbeat.longest_stall_ms.get(),
            busy_ms: if busy_since == 0 {
                0
            } else {
                now_ns.saturating_sub(busy_since) / 1_000_000
            },
        }
    }
}

/// The server process's counters at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) pid: u32,
    pub(crate) started_unix_ms: u64,
    pub(crate) generation: u64,
    /// Milliseconds the counters cover (since start or the last reset).
    pub(crate) window_ms: u64,
    pub(crate) frames_sent: u64,
    pub(crate) frame_bytes: u64,
    pub(crate) frames_deferred: u64,
    pub(crate) full_renders: u64,
    pub(crate) full_render_total_us: u64,
    pub(crate) full_render_max_us: u64,
    pub(crate) retained_renders: u64,
    pub(crate) retained_fallbacks: u64,
    pub(crate) sync_holds: u64,
    pub(crate) sync_hold_timeouts: u64,
    pub(crate) loops: Vec<LoopSnapshot>,
    pub(crate) clients: Vec<ClientFanout>,
}

/// Reads the server counters (allocates; only for the API, never per frame).
pub(crate) fn snapshot() -> Snapshot {
    mark_process_start();
    // A reset between two reads would mix old and zeroed counters: read
    // again until no reset ran meanwhile (seqlock on GENERATION).
    loop {
        let before = GENERATION.load(Ordering::Acquire);
        if !before.is_multiple_of(2) {
            std::thread::yield_now();
            continue;
        }
        let snapshot = read_snapshot(before / 2);
        std::sync::atomic::fence(Ordering::Acquire);
        if GENERATION.load(Ordering::Relaxed) == before {
            return snapshot;
        }
    }
}

fn read_snapshot(generation: u64) -> Snapshot {
    let now = now_ns();
    let full = &server::FULL_RENDERS;
    let mut clients: Vec<ClientFanout> = server::CLIENTS
        .iter()
        .filter_map(|slot| {
            let key = slot.key.load(Ordering::Relaxed);
            (key != 0).then(|| ClientFanout {
                client_id: Some(key - 1),
                frames: slot.frames.get(),
                bytes: slot.bytes.get(),
            })
        })
        .collect();
    clients.sort_by_key(|client| client.client_id);
    let other = &server::OTHER_CLIENTS;
    if other.frames.get() > 0 {
        clients.push(ClientFanout {
            client_id: None,
            frames: other.frames.get(),
            bytes: other.bytes.get(),
        });
    }
    Snapshot {
        pid: std::process::id(),
        started_unix_ms: STARTED_UNIX_MS.load(Ordering::Relaxed),
        generation,
        window_ms: now.saturating_sub(RESET_AT_NS.load(Ordering::Relaxed)) / 1_000_000,
        frames_sent: server::FRAMES_SENT.get(),
        frame_bytes: server::FRAME_BYTES.get(),
        frames_deferred: server::FRAMES_DEFERRED.get(),
        full_renders: full.count.get(),
        full_render_total_us: full.total_ns.get() / 1_000,
        full_render_max_us: full.max_ns.get() / 1_000,
        retained_renders: server::RETAINED_RENDERS.get(),
        retained_fallbacks: server::RETAINED_FALLBACKS.get(),
        sync_holds: server::SYNC_HOLDS.get(),
        sync_hold_timeouts: server::SYNC_HOLD_TIMEOUTS.get(),
        loops: vec![LoopSnapshot::of(&server::LOOP, now)],
        clients,
    }
}

/// Zeroes the server counters and starts a new generation. Heartbeats keep
/// their busy state; only their stall counts reset.
pub(crate) fn reset() {
    let token = begin_reset();
    server::FRAMES_SENT.reset();
    server::FRAME_BYTES.reset();
    server::FRAMES_DEFERRED.reset();
    server::FULL_RENDERS.reset();
    server::RETAINED_RENDERS.reset();
    server::RETAINED_FALLBACKS.reset();
    server::SYNC_HOLDS.reset();
    server::SYNC_HOLD_TIMEOUTS.reset();
    for slot in server::CLIENTS
        .iter()
        .chain(std::iter::once(&server::OTHER_CLIENTS))
    {
        slot.frames.reset();
        slot.bytes.reset();
    }
    server::LOOP.reset_counts();
    RESET_AT_NS.store(now_ns(), Ordering::Relaxed);
    finish_reset(token);
}

/// Marks a reset in progress (odd generation); one reset at a time.
fn begin_reset() -> u64 {
    loop {
        let current = GENERATION.load(Ordering::Relaxed);
        if current.is_multiple_of(2)
            && GENERATION
                .compare_exchange_weak(current, current + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        {
            return current;
        }
        std::thread::yield_now();
    }
}

fn finish_reset(token: u64) {
    GENERATION.store(token + 2, Ordering::Release);
}

#[cfg(test)]
pub(crate) fn begin_reset_for_test() -> u64 {
    begin_reset()
}

#[cfg(test)]
pub(crate) fn finish_reset_for_test(token: u64) {
    finish_reset(token);
}
