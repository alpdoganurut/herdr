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
fn fork_smoke_perf_a_reported_stall_open_across_a_jump_ends_at_its_pre_jump_length() {
    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    heartbeat.begin_at(1_000 * MS);
    let events = poll(&mut watchdog, 1_000, 1_400);
    assert_eq!(stalled(&events), vec![300]);

    // Still busy when the machine sleeps for an hour (wall clock only).
    let hour = 3_600_000;
    let mut events = Vec::new();
    watchdog.check(1_500 * MS, WALL0 + (hour + 1_500) * MS, |event| {
        events.push(event)
    });
    assert_eq!(
        events,
        vec![StallEvent::Ended {
            loop_name: "test_loop",
            stalled_ms: 400
        }]
    );
    // The same period is neither reported again nor lengthened by the pause,
    // and its real end reports nothing more.
    for at in (1_600..=5_000).step_by(100) {
        watchdog.check(at * MS, WALL0 + (hour + at) * MS, |event| {
            events.push(event)
        });
    }
    heartbeat.end();
    watchdog.check(5_100 * MS, WALL0 + (hour + 5_100) * MS, |event| {
        events.push(event)
    });
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(heartbeat.stalls.get(), 1);
    assert_eq!(heartbeat.longest_stall_ms.get(), 400);
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

#[test]
fn fork_smoke_perf_frames_are_counted_per_client_and_a_reset_starts_a_generation() {
    use super::{forget_client, record_frame_sent, reset, snapshot, CLIENT_SLOTS};

    record_frame_sent(7, 100);
    record_frame_sent(7, 50);
    record_frame_sent(9, 10);
    let before = snapshot();
    assert_eq!(before.pid, std::process::id());
    assert!(before.started_unix_ms > 0);
    let seven = before
        .clients
        .iter()
        .find(|client| client.client_id == Some(7))
        .copied();
    assert_eq!(
        seven.map(|client| (client.frames, client.bytes)),
        Some((2, 150))
    );
    assert!(before.frames_sent >= 3 && before.frame_bytes >= 160);

    // Clients past the slots share one bucket; a leaving client frees its slot.
    for client_id in 100..100 + CLIENT_SLOTS as u64 {
        record_frame_sent(client_id, 1);
    }
    assert!(snapshot()
        .clients
        .iter()
        .any(|client| client.client_id.is_none()));
    forget_client(7);
    assert!(!snapshot()
        .clients
        .iter()
        .any(|client| client.client_id == Some(7)));

    reset();
    let after = snapshot();
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(after.pid, before.pid);
    assert_eq!(after.started_unix_ms, before.started_unix_ms);
    assert_eq!((after.frames_sent, after.frame_bytes), (0, 0));
}

#[test]
fn fork_smoke_perf_a_snapshot_never_reads_a_half_done_reset() {
    use super::{begin_reset_for_test, finish_reset_for_test, record_frame_sent, server, snapshot};

    record_frame_sent(3, 100);
    let before = snapshot().generation;
    // A reset is part way through: one counter zeroed, the next not yet.
    let token = begin_reset_for_test();
    server::FRAMES_SENT.reset();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = tx.send(snapshot());
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "a snapshot waits for the reset to finish"
    );
    server::FRAME_BYTES.reset();
    finish_reset_for_test(token);
    let after = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the snapshot finishes once the reset does");
    reader.join().expect("reader thread");
    assert_eq!(after.generation, before + 1);
    assert_eq!((after.frames_sent, after.frame_bytes), (0, 0));
}

/// Manual: what each hot-path touch costs (`cargo test --release --bin herdr
/// perf_hot_path_cost_profile -- --ignored --nocapture`).
#[test]
#[ignore = "manual perf counter cost profile"]
fn perf_hot_path_cost_profile() {
    use std::hint::black_box;
    use std::time::Instant;
    const N: u64 = 2_000_000;
    let heartbeat = Heartbeat::new("bench", 250);
    let time = |label: &str, mut op: Box<dyn FnMut(u64)>| {
        for i in 0..N / 10 {
            op(i);
        }
        let started = Instant::now();
        for i in 0..N {
            op(i);
        }
        let ns = started.elapsed().as_nanos() as f64 / N as f64;
        println!("{label}: {ns:.1} ns/op");
    };
    time(
        "loop heartbeat begin+end (one server/client loop iteration)",
        Box::new(|_| {
            heartbeat.begin();
            heartbeat.end();
        }),
    );
    time(
        "loop heartbeat begin while already busy (a `continue`)",
        Box::new(|_| {
            heartbeat.begin();
        }),
    );
    time(
        "full render timing guard (one full render pass)",
        Box::new(|_| {
            drop(black_box(super::server::FULL_RENDERS.start()));
        }),
    );
    time(
        "record_frame_sent, 2 clients (one sent frame)",
        Box::new(|i| super::record_frame_sent(black_box(i & 1), 2048)),
    );
    time(
        "record_frame_sent, 8 clients (one sent frame)",
        Box::new(|i| super::record_frame_sent(black_box(i & 7), 2048)),
    );
    time(
        "client frame: now_ns + record_client_frame (one presented frame)",
        Box::new(|_| {
            let started = super::now_ns();
            super::record_client_frame(super::now_ns().saturating_sub(started));
        }),
    );
    time(
        "counter inc (retained render / fallback / sync hold)",
        Box::new(|_| super::server::RETAINED_RENDERS.inc()),
    );
}
