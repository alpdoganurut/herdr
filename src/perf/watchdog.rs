//! Fork (perf watch): the stall watchdog.
//!
//! A std thread polls the process's [`Heartbeat`]s every [`POLL`]. A loop
//! busy for longer than its budget is reported once per busy period
//! (`warn!(event = "perf.stall")`), and once more when that period ends
//! (`info!(event = "perf.stall_end")`, with the longest busy time seen).
//! Logging only: no stack capture yet.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::Heartbeat;

/// How often the watchdog looks.
pub(crate) const POLL: Duration = Duration::from_millis(100);
/// Wall vs monotonic disagreement between two polls that means a jump.
pub(crate) const CLOCK_JUMP: Duration = Duration::from_secs(1);
/// A gap between two polls that means the whole process (or machine) paused.
pub(crate) const WATCHDOG_GAP: Duration = Duration::from_secs(2);

/// One report the watchdog makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StallEvent {
    /// A loop has been busy past its budget (once per busy period).
    Stalled {
        loop_name: &'static str,
        stalled_ms: u64,
        threshold_ms: u64,
    },
    /// A reported busy period ended; `stalled_ms` is the longest seen.
    Ended {
        loop_name: &'static str,
        stalled_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct Watch {
    /// The busy period (its busy-since mark) already reported or dropped.
    handled_since: u64,
    /// Whether `handled_since` was reported (so its end is reported too).
    reported: bool,
    /// Longest busy time seen in the reported period.
    longest_ms: u64,
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
                report(StallEvent::Ended {
                    loop_name: heartbeat.name(),
                    stalled_ms: watch.longest_ms,
                });
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
                    reported: false,
                    longest_ms: 0,
                };
                continue;
            }
            if busy_since == watch.handled_since {
                if watch.reported && jumped {
                    // A reported stall still open across a jump: close it at
                    // the length seen before the pause, which never counts.
                    report(StallEvent::Ended {
                        loop_name: heartbeat.name(),
                        stalled_ms: watch.longest_ms,
                    });
                    watch.reported = false;
                } else if watch.reported {
                    watch.longest_ms = watch.longest_ms.max(busy_ms);
                    heartbeat.longest_stall_ms.record(watch.longest_ms);
                }
                continue;
            }
            if busy_ms >= heartbeat.threshold_ms() {
                *watch = Watch {
                    handled_since: busy_since,
                    reported: true,
                    longest_ms: busy_ms,
                };
                heartbeat.stalls.inc();
                heartbeat.longest_stall_ms.record(busy_ms);
                report(StallEvent::Stalled {
                    loop_name: heartbeat.name(),
                    stalled_ms: busy_ms,
                    threshold_ms: heartbeat.threshold_ms(),
                });
            }
        }
    }
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn log_event(event: StallEvent) {
    match event {
        StallEvent::Stalled {
            loop_name,
            stalled_ms,
            threshold_ms,
        } => tracing::warn!(event = "perf.stall", loop_name, stalled_ms, threshold_ms),
        StallEvent::Ended {
            loop_name,
            stalled_ms,
        } => tracing::info!(event = "perf.stall_end", loop_name, stalled_ms),
    }
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
    let spawned = crate::thread_spawn::spawn_named("herdr-perf-watchdog", move || {
        let mut watchdog = Watchdog::new(heartbeats);
        loop {
            std::thread::sleep(POLL);
            watchdog.check(super::now_ns(), super::wall_ns(), log_event);
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
