//! Fork (stall attribution): machine pressure, the loop thread's CPU time and
//! (unsupported here) a stack sample, read by the perf watchdog thread.

use std::path::Path;
use std::process::Child;

use super::super::{MachinePressure, ThreadCpuClock};

/// Load average and swap use from /proc (no memory pressure level).
pub(crate) fn machine_pressure() -> MachinePressure {
    let mut pressure = MachinePressure::default();
    if let Ok(loadavg) = std::fs::read_to_string("/proc/loadavg") {
        pressure.load1_x100 = loadavg
            .split_whitespace()
            .next()
            .and_then(|load| load.parse::<f64>().ok())
            .map(|load| (load * 100.0).round().clamp(0.0, u32::MAX as f64) as u32);
    }
    if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
        let kib = |key: &str| {
            meminfo.lines().find_map(|line| {
                line.strip_prefix(key)?
                    .trim_start_matches(':')
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        };
        if let (Some(total), Some(free)) = (kib("SwapTotal"), kib("SwapFree")) {
            pressure.swap_total_mb = Some(total / 1024);
            pressure.swap_used_mb = Some(total.saturating_sub(free) / 1024);
        }
    }
    pressure
}

/// The calling thread's CPU clock (its kernel thread id).
pub(crate) fn current_thread_cpu_clock() -> Option<ThreadCpuClock> {
    // SAFETY: gettid has no preconditions.
    let tid = unsafe { libc::gettid() };
    u64::try_from(tid).ok().map(ThreadCpuClock)
}

/// CPU time the thread behind `clock` has run (schedstat's first field).
pub(crate) fn thread_cpu_ns(clock: ThreadCpuClock) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/self/task/{}/schedstat", clock.0))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Page faults that needed I/O for this process so far.
pub(crate) fn process_major_faults() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the rusage it is given.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    u64::try_from(usage.ru_majflt).ok()
}

/// No stack sampler without a debugger dependency on Linux.
pub(crate) fn spawn_stack_sample(_pid: u32, _seconds: u32, _path: &Path) -> std::io::Result<Child> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    /// Fork (stall attribution): the platform probes read real values here.
    #[test]
    fn fork_smoke_stall_attr_platform_probes_read_the_machine_and_the_thread() {
        let pressure = super::machine_pressure();
        assert!(pressure.load1_x100.is_some(), "{pressure:?}");
        assert!(pressure.swap_total_mb.is_some(), "{pressure:?}");

        let clock = super::current_thread_cpu_clock().expect("a thread clock");
        let before = super::thread_cpu_ns(clock).expect("cpu before");
        let started = std::time::Instant::now();
        let mut spin = 0u64;
        while started.elapsed() < std::time::Duration::from_millis(50) {
            spin = std::hint::black_box(spin.wrapping_add(1));
        }
        let after = super::thread_cpu_ns(clock).expect("cpu after");
        assert!(after > before, "{before} -> {after}");
        // Read from another thread, as the watchdog does.
        let other = std::thread::spawn(move || super::thread_cpu_ns(clock))
            .join()
            .expect("reader thread");
        assert!(other.is_some_and(|ns| ns >= after));
        // Page-ins (swap or disk reads); 0 is a real answer when everything the
        // test touched is in memory, so only the read itself is checked.
        assert!(super::process_major_faults().is_some());
    }
}
