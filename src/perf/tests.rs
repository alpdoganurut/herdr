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
            StallEvent::Long { .. } | StallEvent::Ended { .. } => None,
        })
        .collect()
}

/// The ended stalls as (loop, longest busy ms).
fn ended(events: &[StallEvent]) -> Vec<(&'static str, u64)> {
    events
        .iter()
        .filter_map(|event| match event {
            StallEvent::Ended {
                loop_name,
                stalled_ms,
                ..
            } => Some((*loop_name, *stalled_ms)),
            _ => None,
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
    assert_eq!(ended(&events), vec![("test_loop", 1_000)]);
    assert_eq!(events.len(), 1, "{events:?}");

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
    assert_eq!(ended(&events), vec![("test_loop", 300)]);
    assert_eq!(events.len(), 1, "{events:?}");
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
    assert_eq!(ended(&events), vec![("test_loop", 400)]);
    assert_eq!(events.len(), 1, "{events:?}");
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

/// Fork (stall attribution): a loop thread that blocks inside a step,
/// watched by a test watchdog on real clocks: the stall names the step at
/// detection and as the step that held the loop.
#[test]
fn fork_smoke_stall_attr_a_blocked_step_is_named_by_the_stall() {
    use super::step::{api_method_index, Step, NO_DETAIL};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static HEARTBEAT: Heartbeat = Heartbeat::new("test_loop", 100);
    static RUNNING: AtomicBool = AtomicBool::new(true);
    let method = crate::api::schema::Method::WorkspaceList(Default::default());
    let index = api_method_index(&method);

    let looping = std::thread::spawn(move || {
        HEARTBEAT.begin();
        HEARTBEAT.step.set(Step::LoopTop);
        HEARTBEAT.step.set(Step::ApiRequests);
        {
            let _api = HEARTBEAT.step.enter(Step::ApiRequest, index);
            std::thread::sleep(Duration::from_millis(500));
        }
        let _render = HEARTBEAT.step.enter(Step::FullRender, NO_DETAIL);
        HEARTBEAT.end();
        RUNNING.store(false, Ordering::Relaxed);
    });

    let heartbeats = [&HEARTBEAT];
    let mut watchdog = Watchdog::new(&heartbeats);
    let mut events = Vec::new();
    while RUNNING.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(20));
        watchdog.check(super::now_ns(), super::wall_ns(), |event| {
            events.push(event)
        });
    }
    looping.join().expect("loop thread");
    watchdog.check(super::now_ns(), super::wall_ns(), |event| {
        events.push(event)
    });

    let detected = events.iter().find_map(|event| match event {
        StallEvent::Stalled { step, .. } => Some(*step),
        _ => None,
    });
    assert_eq!(
        detected.map(|step| step.describe()).as_deref(),
        Some("api_request:workspace.list in api_requests"),
        "{events:?}"
    );
    let attribution = events.iter().find_map(|event| match event {
        StallEvent::Ended { attribution, .. } => Some(*attribution),
        _ => None,
    });
    let attribution = attribution.expect("the stall ended");
    assert_eq!(
        attribution.dominant.describe(),
        "api_request:workspace.list in api_requests"
    );
    assert_eq!(attribution.dominant.detail(), Some("workspace.list"));
    assert!(attribution.dominant_ms >= 200, "{attribution:?}");
}

/// Fork (stall attribution): with injected clocks, the step that held the
/// loop longest wins over the one seen first; changes are counted; a stall
/// past one second asks for a stack sample once.
#[test]
fn fork_smoke_stall_attr_the_longest_held_step_names_the_stall() {
    use super::step::{ScheduledTask, Step};
    use super::watchdog::SAMPLE_AFTER;

    let heartbeat = Heartbeat::new("test_loop", 250);
    let heartbeats = [&heartbeat];
    let mut watchdog = Watchdog::new(&heartbeats);

    heartbeat.begin_at(1_000 * MS);
    heartbeat.step.set(Step::RenderPrepare);
    let mut events = poll(&mut watchdog, 1_000, 1_400);
    // Many short steps, then one scheduled task that holds for two seconds.
    for _ in 0..5 {
        heartbeat.step.set(Step::LoopTop);
        heartbeat.step.set(Step::RenderPrepare);
    }
    heartbeat
        .step
        .set_detail(Step::Scheduled, ScheduledTask::SessionSave as u16);
    events.extend(poll(&mut watchdog, 1_500, 3_500));
    heartbeat.step.set(Step::NextDeadline);
    events.extend(poll(&mut watchdog, 3_600, 3_600));
    heartbeat.end();
    events.extend(poll(&mut watchdog, 3_700, 3_700));

    let long: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            StallEvent::Long { stalled_ms, .. } => Some(*stalled_ms),
            _ => None,
        })
        .collect();
    assert_eq!(long, vec![SAMPLE_AFTER.as_millis() as u64], "{events:?}");
    let Some(StallEvent::Ended {
        stalled_ms,
        attribution,
        ..
    }) = events.last().copied()
    else {
        panic!("the stall ended: {events:?}");
    };
    assert_eq!(stalled_ms, 2_600);
    assert_eq!(attribution.first.describe(), "render_prepare");
    assert_eq!(attribution.dominant.describe(), "scheduled:session_save");
    assert_eq!(attribution.dominant_ms, 2_000);
    assert_eq!(attribution.last.describe(), "next_deadline");
    assert_eq!(attribution.changes, 12);
}

/// Fork (stall attribution): an API request that handles a nested app
/// event between every poll is still one step holding the loop: the end
/// names the request, not a zero-length run.
#[test]
fn fork_smoke_stall_attr_nested_churn_keeps_the_outer_step_holding() {
    use super::step::{api_method_index, Step, NO_DETAIL};

    static HEARTBEAT: Heartbeat = Heartbeat::new("test_loop", 250);
    let method = crate::api::schema::Method::WorkspaceList(Default::default());
    let heartbeats = [&HEARTBEAT];
    let mut watchdog = Watchdog::new(&heartbeats);

    HEARTBEAT.begin_at(1_000 * MS);
    HEARTBEAT.step.set(Step::ApiRequests);
    let api = HEARTBEAT
        .step
        .enter(Step::ApiRequest, api_method_index(&method));
    let mut events = Vec::new();
    for at in (1_000..=3_000).step_by(100) {
        drop(HEARTBEAT.step.enter(Step::AppEvent, 0));
        drop(HEARTBEAT.step.enter(Step::FullRender, NO_DETAIL));
        watchdog.check(at * MS, WALL0 + at * MS, |event| events.push(event));
    }
    drop(api);
    HEARTBEAT.end();
    watchdog.check(3_100 * MS, WALL0 + 3_100 * MS, |event| events.push(event));

    let Some(StallEvent::Ended { attribution, .. }) = events.last().copied() else {
        panic!("the stall ended: {events:?}");
    };
    assert_eq!(
        attribution.dominant.describe(),
        "api_request:workspace.list"
    );
    assert_eq!(attribution.dominant_ms, 1_700);
    assert!(attribution.changes >= 4 * 17, "{attribution:?}");
}

/// Fork (stall attribution): a nested step names the step it interrupted
/// and puts it back; re-entering the same step keeps the outer one.
#[test]
fn fork_smoke_stall_attr_nested_steps_restore_the_step_they_interrupt() {
    use super::step::{api_method_index, app_event_kind, Step};

    static HEARTBEAT: Heartbeat = Heartbeat::new("test_loop", 250);
    let method = crate::api::schema::Method::WorkspaceList(Default::default());
    let index = api_method_index(&method);
    let event = crate::events::AppEvent::TerminalBell {
        pane_id: crate::layout::PaneId::from_raw(1),
        count: 1,
    };

    HEARTBEAT.step.set(Step::ServerEvents);
    {
        let _outer = HEARTBEAT.step.enter(Step::ApiRequest, index);
        {
            let _same = HEARTBEAT.step.enter(Step::ApiRequest, index);
            assert_eq!(
                HEARTBEAT.step.read().describe(),
                "api_request:workspace.list in server_events"
            );
            let _inner = HEARTBEAT.step.enter(Step::AppEvent, app_event_kind(&event));
            assert_eq!(
                HEARTBEAT.step.read().describe(),
                "app_event:terminal_bell in api_request:workspace.list"
            );
        }
        assert_eq!(
            HEARTBEAT.step.read().describe(),
            "api_request:workspace.list in server_events"
        );
    }
    assert_eq!(HEARTBEAT.step.read().describe(), "server_events");
    assert_eq!(HEARTBEAT.step.read().detail(), None);
}

#[test]
fn fork_smoke_stall_attr_every_api_method_has_a_detail_index() {
    use super::step::{Step, StepMarker, NO_DETAIL};
    static MARKER: StepMarker = StepMarker::new();
    for (index, operation) in crate::api::schema::Method::OPERATIONS.iter().enumerate() {
        let index = u16::try_from(index).expect("fits");
        assert_ne!(index, NO_DETAIL);
        MARKER.set(Step::Dispatch);
        let _guard = MARKER.enter(Step::ApiRequest, index);
        assert_eq!(MARKER.read().detail(), Some(operation.name));
    }
}

/// Fork (stall attribution): the production reporter's log lines carry the
/// step, the loop thread's CPU and the machine's pressure (the sampler is
/// off in tests).
#[test]
fn fork_smoke_stall_attr_the_end_line_names_step_cpu_and_pressure() {
    use super::step::{Step, StepRecord};
    use super::watchdog::{Attribution, Reporter};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();

    static MARKER: super::step::StepMarker = super::step::StepMarker::new();
    MARKER.set(Step::ServerEvents);
    let step = {
        let _guard = MARKER.enter(
            Step::ServerEvent,
            super::step::SERVER_EVENT_KINDS
                .iter()
                .position(|kind| *kind == "client_connected")
                .unwrap() as u16,
        );
        MARKER.read()
    };
    tracing::subscriber::with_default(subscriber, || {
        let mut reporter = Reporter::new(crate::platform::current_thread_cpu_clock());
        reporter.report(StallEvent::Stalled {
            loop_name: "server_loop",
            stalled_ms: 300,
            threshold_ms: 250,
            busy_since_ns: 7,
            step,
        });
        // Burn a little CPU on this (the "loop") thread.
        let started = std::time::Instant::now();
        let mut spin = 0u64;
        while started.elapsed() < std::time::Duration::from_millis(30) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        reporter.report(StallEvent::Ended {
            loop_name: "server_loop",
            stalled_ms: 12_561,
            busy_since_ns: 7,
            attribution: Attribution {
                first: step,
                dominant: step,
                dominant_ms: 12_000,
                last: StepRecord::default(),
                changes: 3,
            },
        });
    });
    let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    println!("{text}");
    let end = text
        .lines()
        .find(|line| line.contains("perf.stall_end"))
        .expect("an end line");
    assert!(
        end.contains(r#"step="server_event:client_connected in server_events""#),
        "{end}"
    );
    assert!(end.contains("step_ms=12000"), "{end}");
    assert!(end.contains("step_changes=3"), "{end}");
    assert!(
        !end.contains("last_step"),
        "an unmarked step is left out: {end}"
    );
    assert!(!end.contains("stack_sample"), "no sampler in tests: {end}");
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        assert!(end.contains("loop_cpu_ms="), "{end}");
        assert!(end.contains("load1="), "{end}");
        assert!(end.contains("swap_total_mb="), "{end}");
    }
    let stall = text
        .lines()
        .find(|line| line.contains(r#"event="perf.stall""#))
        .expect("a stall line");
    assert!(stall.contains("step="), "{stall}");
}

/// Fork (stall attribution): the platform probes read real values here.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn fork_smoke_stall_attr_platform_probes_read_the_machine_and_the_thread() {
    let pressure = crate::platform::machine_pressure();
    assert!(pressure.load1_x100.is_some(), "{pressure:?}");
    assert!(pressure.swap_total_mb.is_some(), "{pressure:?}");
    #[cfg(target_os = "macos")]
    assert!(pressure.memory_pressure.is_some(), "{pressure:?}");

    let clock = crate::platform::current_thread_cpu_clock().expect("a thread clock");
    let before = crate::platform::thread_cpu_ns(clock).expect("cpu before");
    let started = std::time::Instant::now();
    let mut spin = 0u64;
    while started.elapsed() < std::time::Duration::from_millis(50) {
        spin = std::hint::black_box(spin.wrapping_add(1));
    }
    let after = crate::platform::thread_cpu_ns(clock).expect("cpu after");
    assert!(after > before, "{before} -> {after}");
    // Read from another thread, as the watchdog does.
    let other = std::thread::spawn(move || crate::platform::thread_cpu_ns(clock))
        .join()
        .expect("reader thread");
    assert!(other.is_some_and(|ns| ns >= after));
    // Page-ins (swap or disk reads); 0 is a real answer when everything the
    // test touched is in memory, so only the read itself is checked.
    assert!(crate::platform::process_major_faults().is_some());
}

/// Manual (macOS): the stack sampler writes a call graph of this process
/// naming this thread (`cargo nextest run --run-ignored only
/// stall_attr_the_stack_sampler`).
#[cfg(target_os = "macos")]
#[test]
#[ignore = "manual: runs /usr/bin/sample on the test process for about two seconds"]
fn fork_smoke_stall_attr_the_stack_sampler_writes_a_call_graph() {
    let path = std::env::temp_dir().join(format!("herdr-stall-sample-{}.txt", std::process::id()));
    let mut child =
        crate::platform::spawn_stack_sample(std::process::id(), 1, &path).expect("sample starts");
    // A 100 ms poller (the watchdog's cadence) keeps running while the
    // sampler suspends threads to sample them: its gaps stay well under the
    // watchdog's pause guard, so a sample never drops the stall it samples.
    let started = std::time::Instant::now();
    let mut last = started;
    let mut largest_gap = std::time::Duration::ZERO;
    while child.try_wait().expect("try_wait").is_none() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "sample hangs"
        );
        std::thread::sleep(super::watchdog::POLL);
        largest_gap = largest_gap.max(last.elapsed());
        last = std::time::Instant::now();
    }
    assert!(
        largest_gap < super::watchdog::WATCHDOG_GAP / 2,
        "{largest_gap:?}"
    );
    let text = std::fs::read_to_string(&path).expect("a sample file");
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("Call graph:"), "{text}");
    assert!(
        text.contains("fork_smoke_stall_attr_the_stack_sampler"),
        "{text}"
    );
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
    // Fork (stall attribution): about 15 sets per loop iteration, one
    // enter + drop per handled event or render pass.
    static STEP: super::step::StepMarker = super::step::StepMarker::new();
    time(
        "step set (one server loop phase)",
        Box::new(|_| STEP.set(black_box(super::step::Step::ClientSync))),
    );
    time(
        "step enter + guard drop (one handled event / render pass)",
        Box::new(|i| {
            drop(black_box(
                STEP.enter(super::step::Step::AppEvent, black_box(i as u16 & 7)),
            ))
        }),
    );
}
