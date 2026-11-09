//! Fork (perf watch): the busy-since heartbeat and the watchdog's decisions,
//! with injected clocks.

use super::watchdog::{StallEvent, Watchdog, CLOCK_JUMP, POLL};
use super::{Counter, Heartbeat, MaxGauge};

const MS: u64 = 1_000_000;
/// An arbitrary wall-clock origin; the watchdog only compares gaps.
const WALL0: u64 = 1_700_000_000_000 * MS;

/// Polls `watchdog` every POLL from `from_ms` to `to_ms` (inclusive), with the
/// wall clock advancing in step, and collects its reports.
fn poll(watchdog: &mut Watchdog<'_>, from_ms: u64, to_ms: u64) -> Vec<StallEvent> {
    let step = POLL.as_millis() as u64;
    let mut events = Vec::new();
    let mut at = from_ms;
    while at <= to_ms {
        watchdog.check(at * MS, WALL0 + at * MS, |event| events.push(event));
        at += step;
    }
    events
}

fn stalled(events: &[StallEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|event| match event {
            StallEvent::Stalled { stalled_ms, .. } => Some(*stalled_ms),
            StallEvent::Ended { .. } => None,
        })
        .collect()
}

#[test]
fn fork_smoke_perf_a_long_busy_period_is_reported_once() {
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    heartbeat.begin_at(1_000 * MS);
    let events = poll(&mut watchdog, 1_000, 2_000);

    // Fires at the first poll past the budget, then stays quiet for the
    // rest of the same busy period.
    assert_eq!(stalled(&events), vec![300]);
    assert_eq!(heartbeat.stalls.get(), 1);
    assert_eq!(heartbeat.longest_stall_ms.get(), 1_000);

    // The end of the period is reported with the longest busy time seen.
    heartbeat.end();
    let events = poll(&mut watchdog, 2_100, 2_100);
    assert_eq!(
        events,
        vec![StallEvent::Ended {
            loop_name: "test_loop",
            stalled_ms: 1_000
        }]
    );

    // A new busy period past the budget is a new report.
    heartbeat.begin_at(3_000 * MS);
    let events = poll(&mut watchdog, 3_000, 3_500);
    assert_eq!(stalled(&events), vec![300]);
    assert_eq!(heartbeat.stalls.get(), 2);
}

#[test]
fn fork_smoke_perf_an_idle_loop_never_stalls() {
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    // Waiting for events for ten seconds: never busy.
    assert!(poll(&mut watchdog, 0, 10_000).is_empty());

    // Short busy periods with waits in between: each under budget.
    for start in (10_000..20_000).step_by(300) {
        heartbeat.begin_at(start * MS);
        watchdog.check((start + 200) * MS, WALL0 + (start + 200) * MS, |_| {
            panic!("a 200 ms busy period is under the 250 ms budget")
        });
        heartbeat.end();
        watchdog.check((start + 250) * MS, WALL0 + (start + 250) * MS, |_| {
            panic!("an idle loop is not stalled")
        });
    }
    assert_eq!(heartbeat.stalls.get(), 0);

    // After a reported stall, the wait that follows is not a new one, however
    // long it lasts.
    assert!(poll(&mut watchdog, 20_200, 29_900).is_empty());
    heartbeat.begin_at(30_000 * MS);
    assert_eq!(stalled(&poll(&mut watchdog, 30_000, 30_300)), vec![300]);
    heartbeat.end();
    let events = poll(&mut watchdog, 30_400, 45_000);
    assert_eq!(
        events,
        vec![StallEvent::Ended {
            loop_name: "test_loop",
            stalled_ms: 300
        }]
    );
    assert_eq!(heartbeat.stalls.get(), 1);
}

#[test]
fn fork_smoke_perf_begin_keeps_the_first_mark_until_the_loop_waits() {
    // The server loop `continue`s back to its top without waiting; that is
    // the same busy period, so a second begin must not restart the clock.
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    heartbeat.begin_at(1_000 * MS);
    watchdog.check(1_100 * MS, WALL0 + 1_100 * MS, |_| {});
    heartbeat.begin_at(1_200 * MS);
    heartbeat.begin_at(1_250 * MS);
    let mut events = Vec::new();
    watchdog.check(1_300 * MS, WALL0 + 1_300 * MS, |event| events.push(event));

    assert_eq!(heartbeat.busy_since_ns(), 1_000 * MS);
    assert_eq!(stalled(&events), vec![300]);

    // After a wait, begin marks a fresh period.
    heartbeat.end();
    assert_eq!(heartbeat.busy_since_ns(), 0);
    heartbeat.begin_at(5_000 * MS);
    assert_eq!(heartbeat.busy_since_ns(), 5_000 * MS);
}

#[test]
fn fork_smoke_perf_a_wall_clock_jump_drops_the_open_busy_period() {
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    watchdog.check(900 * MS, WALL0 + 900 * MS, |_| {});
    heartbeat.begin_at(1_000 * MS);
    watchdog.check(1_000 * MS, WALL0 + 1_000 * MS, |_| {});

    // Suspend on a clock that stops in sleep: 100 ms of monotonic time,
    // one hour of wall time. The period open across it is not reported, not
    // even later while it stays open.
    let hour = 3_600_000;
    let mut events = Vec::new();
    for at in [1_100, 1_400, 2_000] {
        watchdog.check(at * MS, WALL0 + (hour + at) * MS, |event| {
            events.push(event)
        });
    }
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(heartbeat.stalls.get(), 0);

    // A later busy period is watched again.
    heartbeat.end();
    heartbeat.begin_at(3_000 * MS);
    for at in (3_000..=3_300).step_by(100) {
        watchdog.check(at * MS, WALL0 + (hour + at) * MS, |event| {
            events.push(event)
        });
    }
    assert_eq!(stalled(&events), vec![300]);
}

#[test]
fn fork_smoke_perf_a_paused_watchdog_drops_the_open_busy_period() {
    // Suspend on a clock that keeps counting in sleep: both clocks jump by
    // the same hour, but the watchdog itself did not run in between.
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    heartbeat.begin_at(1_000 * MS);
    watchdog.check(1_100 * MS, WALL0 + 1_100 * MS, |_| {});
    let later = 1_100 + 3_600_000;
    let mut events = Vec::new();
    watchdog.check(later * MS, WALL0 + later * MS, |event| events.push(event));
    watchdog.check((later + 100) * MS, WALL0 + (later + 100) * MS, |event| {
        events.push(event)
    });
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn fork_smoke_perf_small_wall_clock_corrections_do_not_hide_a_stall() {
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);
    let slew = CLOCK_JUMP.as_millis() as u64 / 2;

    heartbeat.begin_at(1_000 * MS);
    watchdog.check(1_000 * MS, WALL0 + 1_000 * MS, |_| {});
    let mut events = Vec::new();
    // NTP steps the wall clock back by half the jump threshold mid-stall.
    watchdog.check(1_100 * MS, WALL0 + (1_100 - slew) * MS, |event| {
        events.push(event)
    });
    watchdog.check(1_300 * MS, WALL0 + (1_300 - slew) * MS, |event| {
        events.push(event)
    });
    assert_eq!(stalled(&events), vec![300]);
}

#[test]
fn fork_smoke_perf_counters_and_gauges_are_plain_atomics() {
    let counter = Counter::new();
    counter.inc();
    counter.add(41);
    assert_eq!(counter.get(), 42);
    counter.reset();
    assert_eq!(counter.get(), 0);

    let gauge = MaxGauge::new();
    gauge.record(7);
    gauge.record(3);
    assert_eq!(gauge.take(), 7);
    assert_eq!(gauge.get(), 0);
}
