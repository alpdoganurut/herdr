//! Fork (perf watch): the stall watchdog.
//!
//! A std thread polls the process's [`Heartbeat`]s every [`POLL`]. A loop
//! busy for longer than its budget is reported once per busy period
//! (`warn!(event = "perf.stall")`), and once more when that period ends
//! (`info!(event = "perf.stall_end")`, with the longest busy time seen).
//!
//! Fork (stall attribution): each report names its cause. While a stall is
//! open the watchdog reads the loop's [`super::step::StepMarker`] at every
//! poll: the end names the step that held the loop longest (its dwell, the
//! first and last step seen and how many step changes happened). The thread
//! wrapper adds what the pure check cannot see: machine pressure (load,
//! swap, memory pressure) and the loop thread's CPU time and the process's
//! major faults across the stall (a loop that burned CPU did our work; one
//! that barely ran was starved or paged out), and past [`SAMPLE_AFTER`] a
//! rate-limited stack sample (`/usr/bin/sample` on macOS, spawned and
//! reaped without waiting) whose file the end line names.
//!
//! Suspend guard: when the wall clock and the monotonic clock disagree by more
//! than [`CLOCK_JUMP`] between two polls (a sleep on a clock that stops in
//! suspend, or a wall-clock change), or the watchdog itself did not run for
//! [`WATCHDOG_GAP`] (a sleep on a clock that keeps counting), every busy
//! period open at that moment is dropped without a report: its length says
//! nothing about the program.
//!
//! The decision is [`Watchdog::check`], a pure function of the heartbeats and
//! the injected clocks; the thread is only the poll loop around it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::step::{changes_between, StepRecord};
use super::Heartbeat;

/// How often the watchdog looks.
pub(crate) const POLL: Duration = Duration::from_millis(100);
/// Wall vs monotonic disagreement between two polls that means a jump.
pub(crate) const CLOCK_JUMP: Duration = Duration::from_secs(1);
/// A gap between two polls that means the whole process (or machine) paused.
pub(crate) const WATCHDOG_GAP: Duration = Duration::from_secs(2);
/// A reported stall this long asks for a stack sample (once per stall).
pub(crate) const SAMPLE_AFTER: Duration = Duration::from_secs(1);

/// One report the watchdog makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StallEvent {
    /// A loop has been busy past its budget (once per busy period).
    Stalled {
        loop_name: &'static str,
        stalled_ms: u64,
        threshold_ms: u64,
        busy_since_ns: u64,
        /// The step the loop is in now.
        step: StepRecord,
    },
    /// A reported stall passed [`SAMPLE_AFTER`] (once per busy period).
    Long {
        loop_name: &'static str,
        stalled_ms: u64,
        busy_since_ns: u64,
    },
    /// A reported busy period ended; `stalled_ms` is the longest seen.
    Ended {
        loop_name: &'static str,
        stalled_ms: u64,
        busy_since_ns: u64,
        attribution: Attribution,
    },
}

/// The steps one reported stall was seen in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Attribution {
    /// The step at detection.
    pub(crate) first: StepRecord,
    /// The step that held the loop over the longest run of polls. A run
    /// continues while each poll sees its step as the current or the
    /// interrupted (outer) step, so an API request that handles many nested
    /// app events is one run. The full mark (with its nesting) when nothing
    /// changed during the run, else the run's step alone.
    pub(crate) dominant: StepRecord,
    /// How long `dominant` held (at least; poll accuracy).
    pub(crate) dominant_ms: u64,
    /// The step at the last poll before the end.
    pub(crate) last: StepRecord,
    /// Step changes between detection and the last poll.
    pub(crate) changes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Watch {
    /// The busy period (its busy-since mark) already reported or dropped.
    handled_since: u64,
    /// Whether `handled_since` was reported (so its end is reported too).
    reported: bool,
    /// Longest busy time seen in the reported period.
    longest_ms: u64,
    /// Whether the reported period passed [`SAMPLE_AFTER`].
    long: bool,
    attribution: Attribution,
    /// The current run: its step, when the polls first saw it, and whether
    /// the full mark stayed unchanged since.
    run_key: u64,
    run_since_ns: u64,
    run_unchanged: bool,
}

impl Watch {
    /// One poll's step read while the reported stall stays open.
    fn observe(&mut self, step: StepRecord, mono_ns: u64) {
        let attribution = &mut self.attribution;
        let changed = step.counter() != attribution.last.counter();
        if changed {
            attribution.changes += changes_between(attribution.last, step);
            attribution.last = step;
        }
        let in_run = step.current_mark().0 == self.run_key
            || step
                .outer_mark()
                .is_some_and(|outer| outer.0 == self.run_key);
        if !in_run {
            self.run_key = step.current_mark().0;
            self.run_since_ns = mono_ns;
            self.run_unchanged = true;
            return;
        }
        self.run_unchanged &= !changed;
        let held_ms = mono_ns.saturating_sub(self.run_since_ns) / 1_000_000;
        if held_ms > attribution.dominant_ms {
            attribution.dominant = if self.run_unchanged {
                step
            } else {
                StepRecord(self.run_key)
            };
            attribution.dominant_ms = held_ms;
        }
    }

    fn ended(&self, loop_name: &'static str) -> StallEvent {
        StallEvent::Ended {
            loop_name,
            stalled_ms: self.longest_ms,
            busy_since_ns: self.handled_since,
            attribution: self.attribution,
        }
    }
}

/// The watchdog's memory between polls.
pub(crate) struct Watchdog<'a> {
    heartbeats: &'a [&'a Heartbeat],
    watches: Vec<Watch>,
    last_mono_ns: Option<u64>,
    last_wall_ns: u64,
}

impl<'a> Watchdog<'a> {
    pub(crate) fn new(heartbeats: &'a [&'a Heartbeat]) -> Self {
        Self {
            heartbeats,
            watches: vec![Watch::default(); heartbeats.len()],
            last_mono_ns: None,
            last_wall_ns: 0,
        }
    }

    /// One poll at `mono_ns` ([`super::now_ns`]) / `wall_ns` ([`super::wall_ns`]).
    pub(crate) fn check(&mut self, mono_ns: u64, wall_ns: u64, mut report: impl FnMut(StallEvent)) {
        let jumped = self.last_mono_ns.is_some_and(|last_mono| {
            let mono_gap = mono_ns.saturating_sub(last_mono);
            let wall_gap = wall_ns.abs_diff(self.last_wall_ns);
            mono_gap.abs_diff(wall_gap) > duration_ns(CLOCK_JUMP)
                || mono_gap > duration_ns(WATCHDOG_GAP)
        });
        self.last_mono_ns = Some(mono_ns);
        self.last_wall_ns = wall_ns;

        for (heartbeat, watch) in self.heartbeats.iter().zip(self.watches.iter_mut()) {
            let busy_since = heartbeat.busy_since_ns();
            if watch.reported && busy_since != watch.handled_since {
                report(watch.ended(heartbeat.name()));
                watch.reported = false;
            }
            if busy_since == 0 || mono_ns < busy_since {
                continue;
            }
            let busy_ms = (mono_ns - busy_since) / 1_000_000;
            if jumped && busy_since != watch.handled_since {
                // Open across a jump: drop this period without a report.
                *watch = Watch {
                    handled_since: busy_since,
                    ..Watch::default()
                };
                continue;
            }
            if busy_since == watch.handled_since {
                if watch.reported && jumped {
                    // A reported stall still open across a jump: close it at
                    // the length seen before the pause, which never counts.
                    report(watch.ended(heartbeat.name()));
                    watch.reported = false;
                } else if watch.reported {
                    watch.longest_ms = watch.longest_ms.max(busy_ms);
                    heartbeat.longest_stall_ms.record(watch.longest_ms);
                    watch.observe(heartbeat.step.read(), mono_ns);
                    if !watch.long && busy_ms >= duration_ms(SAMPLE_AFTER) {
                        watch.long = true;
                        report(StallEvent::Long {
                            loop_name: heartbeat.name(),
                            stalled_ms: busy_ms,
                            busy_since_ns: busy_since,
                        });
                    }
                }
                continue;
            }
            if busy_ms >= heartbeat.threshold_ms() {
                let step = heartbeat.step.read();
                *watch = Watch {
                    handled_since: busy_since,
                    reported: true,
                    longest_ms: busy_ms,
                    long: false,
                    attribution: Attribution {
                        first: step,
                        dominant: step,
                        dominant_ms: 0,
                        last: step,
                        changes: 0,
                    },
                    run_key: step.current_mark().0,
                    run_since_ns: mono_ns,
                    run_unchanged: true,
                };
                heartbeat.stalls.inc();
                heartbeat.longest_stall_ms.record(busy_ms);
                report(StallEvent::Stalled {
                    loop_name: heartbeat.name(),
                    stalled_ms: busy_ms,
                    threshold_ms: heartbeat.threshold_ms(),
                    busy_since_ns: busy_since,
                    step,
                });
                if busy_ms >= duration_ms(SAMPLE_AFTER) {
                    watch.long = true;
                    report(StallEvent::Long {
                        loop_name: heartbeat.name(),
                        stalled_ms: busy_ms,
                        busy_since_ns: busy_since,
                    });
                }
            }
        }
    }
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// What the thread wrapper read when a stall was detected.
struct OpenStall {
    loop_name: &'static str,
    busy_since_ns: u64,
    detected_ms: u64,
    pressure: crate::platform::MachinePressure,
    loop_cpu_ns: Option<u64>,
    major_faults: Option<u64>,
    sample: Option<PathBuf>,
}

/// The production side of the watchdog thread: logs each [`StallEvent`]
/// with what the pure check cannot read (machine pressure, the loop
/// thread's CPU, major faults, a stack sample). Runs on the watchdog thread
/// only, never on a watched loop.
pub(crate) struct Reporter {
    /// The watched loop's thread (one loop per process role).
    loop_cpu: Option<crate::platform::ThreadCpuClock>,
    open: Vec<OpenStall>,
    sampler: Sampler,
}

impl Reporter {
    pub(crate) fn new(loop_cpu: Option<crate::platform::ThreadCpuClock>) -> Self {
        Self {
            loop_cpu,
            open: Vec::new(),
            sampler: Sampler::default(),
        }
    }

    fn loop_cpu_ns(&self) -> Option<u64> {
        self.loop_cpu.and_then(crate::platform::thread_cpu_ns)
    }

    pub(crate) fn report(&mut self, event: StallEvent) {
        match event {
            StallEvent::Stalled {
                loop_name,
                stalled_ms,
                threshold_ms,
                busy_since_ns,
                step,
            } => {
                let pressure = crate::platform::machine_pressure();
                tracing::warn!(
                    event = "perf.stall",
                    loop_name,
                    stalled_ms,
                    threshold_ms,
                    step = step_text(step),
                    load1 = pressure.load1_x100.map(load_value),
                    swap_used_mb = pressure.swap_used_mb,
                    swap_total_mb = pressure.swap_total_mb,
                    memory_pressure = pressure.memory_pressure,
                );
                self.open.retain(|open| open.loop_name != loop_name);
                self.open.push(OpenStall {
                    loop_name,
                    busy_since_ns,
                    detected_ms: stalled_ms,
                    pressure,
                    loop_cpu_ns: self.loop_cpu_ns(),
                    major_faults: crate::platform::process_major_faults(),
                    sample: None,
                });
            }
            StallEvent::Long {
                loop_name,
                stalled_ms,
                busy_since_ns,
            } => {
                let Some(path) = self.sampler.start(loop_name, super::now_ns()) else {
                    return;
                };
                tracing::info!(
                    event = "perf.stall_sample",
                    loop_name,
                    stalled_ms,
                    stack_sample = %path.display(),
                );
                if let Some(open) = self
                    .open
                    .iter_mut()
                    .find(|open| open.loop_name == loop_name && open.busy_since_ns == busy_since_ns)
                {
                    open.sample = Some(path);
                }
            }
            StallEvent::Ended {
                loop_name,
                stalled_ms,
                busy_since_ns,
                attribution,
            } => {
                let open = self
                    .open
                    .iter()
                    .position(|open| {
                        open.loop_name == loop_name && open.busy_since_ns == busy_since_ns
                    })
                    .map(|index| self.open.swap_remove(index));
                let pressure = open.as_ref().map(|open| open.pressure).unwrap_or_default();
                let loop_cpu_ms = open.as_ref().and_then(|open| {
                    Some(self.loop_cpu_ns()?.saturating_sub(open.loop_cpu_ns?) / 1_000_000)
                });
                let major_faults = open.as_ref().and_then(|open| {
                    Some(
                        crate::platform::process_major_faults()?.saturating_sub(open.major_faults?),
                    )
                });
                // The CPU and fault window: detection to the end.
                let window_ms = open
                    .as_ref()
                    .map(|open| stalled_ms.saturating_sub(open.detected_ms));
                let sample = open.and_then(|open| open.sample);
                tracing::info!(
                    event = "perf.stall_end",
                    loop_name,
                    stalled_ms,
                    step = step_text(attribution.dominant),
                    step_ms = attribution.dominant_ms,
                    first_step = step_text(attribution.first),
                    last_step = step_text(attribution.last),
                    step_changes = attribution.changes,
                    loop_cpu_ms,
                    cpu_window_ms = window_ms,
                    major_faults,
                    load1 = pressure.load1_x100.map(load_value),
                    swap_used_mb = pressure.swap_used_mb,
                    swap_total_mb = pressure.swap_total_mb,
                    memory_pressure = pressure.memory_pressure,
                    stack_sample = sample.as_ref().map(|path| path.display().to_string()),
                );
            }
        }
    }

    /// Reaps a finished sampler (each poll; never waits).
    pub(crate) fn tick(&mut self) {
        self.sampler.reap();
    }
}

/// The step as `step[:detail][ in outer[:detail]]`; none for a loop that
/// never marks steps (the client loop).
fn step_text(step: StepRecord) -> Option<String> {
    step.is_marked().then(|| step.describe())
}

fn load_value(load1_x100: u32) -> f64 {
    f64::from(load1_x100) / 100.0
}

/// At most one stack sample at a time, one per [`SAMPLE_INTERVAL`], the
/// newest [`SAMPLE_KEEP`] files kept.
#[derive(Default)]
struct Sampler {
    running: Option<std::process::Child>,
    last_started_ns: Option<u64>,
    /// The OS has no sampler: stop trying.
    disabled: bool,
}

/// The shortest gap between two stack samples.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Stack sample files kept under `<data dir>/perf-samples`.
pub(crate) const SAMPLE_KEEP: usize = 20;
const SAMPLE_SECONDS: u32 = 1;

impl Sampler {
    fn start(&mut self, loop_name: &str, now_ns: u64) -> Option<PathBuf> {
        self.reap();
        if self.disabled || self.running.is_some() || !sampling_enabled() {
            return None;
        }
        if self
            .last_started_ns
            .is_some_and(|last| now_ns.saturating_sub(last) < duration_ns(SAMPLE_INTERVAL))
        {
            return None;
        }
        let dir = crate::session::data_dir().join("perf-samples");
        if let Err(error) = std::fs::create_dir_all(&dir) {
            tracing::warn!(event = "perf.stall_sample_failed", %error, "no sample directory");
            // Transient (a full disk, memory pressure): try again next interval.
            self.last_started_ns = Some(now_ns);
            return None;
        }
        prune_samples(&dir, SAMPLE_KEEP.saturating_sub(1));
        let path = dir.join(format!(
            "stall-{:013}-{loop_name}.txt",
            super::wall_ns() / 1_000_000
        ));
        match crate::platform::spawn_stack_sample(std::process::id(), SAMPLE_SECONDS, &path) {
            Ok(child) => {
                self.running = Some(child);
                self.last_started_ns = Some(now_ns);
                Some(path)
            }
            Err(error) => {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::NotFound
                ) {
                    // No sampler on this OS or machine: stop trying.
                    self.disabled = true;
                } else {
                    tracing::warn!(event = "perf.stall_sample_failed", %error, "stack sampler did not start");
                    // Transient (fork refused under pressure): back off.
                    self.last_started_ns = Some(now_ns);
                }
                None
            }
        }
    }

    fn reap(&mut self) {
        if let Some(child) = self.running.as_mut() {
            if !matches!(child.try_wait(), Ok(None)) {
                self.running = None;
            }
        }
    }
}

/// Removes the oldest `stall-*.txt` files beyond `keep`.
fn prune_samples(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut samples: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("stall-") && name.ends_with(".txt"))
        })
        .collect();
    if samples.len() <= keep {
        return;
    }
    samples.sort();
    let excess = samples.len() - keep;
    for path in samples.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// Tests drive real server loops (and their watchdog) but never run the
/// OS stack sampler.
#[cfg(not(test))]
fn sampling_enabled() -> bool {
    true
}

#[cfg(test)]
fn sampling_enabled() -> bool {
    false
}

static SERVER_HEARTBEATS: [&Heartbeat; 1] = [&super::server::LOOP];
static CLIENT_HEARTBEATS: [&Heartbeat; 1] = [&super::client::LOOP];
static STARTED: AtomicBool = AtomicBool::new(false);

/// Which process's loops to watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Server,
    Client,
}

/// Starts the watchdog thread once per process. A thread the OS refuses is
/// logged; the program runs on unwatched.
pub(crate) fn start(role: Role) {
    super::mark_process_start();
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let heartbeats: &'static [&'static Heartbeat] = match role {
        Role::Server => &SERVER_HEARTBEATS,
        Role::Client => &CLIENT_HEARTBEATS,
    };
    // Called on the watched loop's own thread: its CPU clock.
    let loop_cpu = crate::platform::current_thread_cpu_clock();
    let spawned = crate::thread_spawn::spawn_named("herdr-perf-watchdog", move || {
        let mut watchdog = Watchdog::new(heartbeats);
        let mut reporter = Reporter::new(loop_cpu);
        loop {
            std::thread::sleep(POLL);
            watchdog.check(super::now_ns(), super::wall_ns(), |event| {
                reporter.report(event)
            });
            reporter.tick();
        }
    });
    if let Err(error) = spawned {
        STARTED.store(false, Ordering::Relaxed);
        tracing::warn!(
            event = "perf.watchdog_start_failed",
            %error,
            "perf watchdog thread could not start; stalls go unreported"
        );
    }
}
