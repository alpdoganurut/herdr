//! Fork (stall attribution): machine pressure, the loop thread's CPU time and
//! a stack sample, read by the perf watchdog thread (never the loop).

use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::super::{MachinePressure, ThreadCpuClock};

/// Load average, swap use and the kernel's memory pressure level.
pub(crate) fn machine_pressure() -> MachinePressure {
    let mut pressure = MachinePressure::default();
    let mut loads = [0f64; 3];
    // SAFETY: getloadavg writes at most `nelem` doubles into the buffer.
    if unsafe { libc::getloadavg(loads.as_mut_ptr(), 1) } == 1 {
        pressure.load1_x100 = Some((loads[0] * 100.0).round().clamp(0.0, u32::MAX as f64) as u32);
    }
    let mut swap: libc::xsw_usage = unsafe { std::mem::zeroed() };
    if sysctl_value(c"vm.swapusage", &mut swap) {
        pressure.swap_used_mb = Some(swap.xsu_used / (1024 * 1024));
        pressure.swap_total_mb = Some(swap.xsu_total / (1024 * 1024));
    }
    let mut level: libc::c_int = 0;
    if sysctl_value(c"kern.memorystatus_vm_pressure_level", &mut level) {
        // DISPATCH_MEMORYPRESSURE_NORMAL / WARN / CRITICAL.
        pressure.memory_pressure = match level {
            1 => Some("normal"),
            2 => Some("warn"),
            4 => Some("critical"),
            _ => None,
        };
    }
    pressure
}

fn sysctl_value<T>(name: &std::ffi::CStr, value: &mut T) -> bool {
    let mut len = std::mem::size_of::<T>();
    // SAFETY: the out buffer is a valid `T` of `len` bytes; the kernel writes
    // at most `len` bytes and reports the size it wrote.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (value as *mut T).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    result == 0 && len == std::mem::size_of::<T>()
}

/// The calling thread's CPU clock (its Mach thread port; no extra right).
pub(crate) fn current_thread_cpu_clock() -> Option<ThreadCpuClock> {
    // SAFETY: pthread_self is always valid for the calling thread;
    // pthread_mach_thread_np returns its port without adding a reference.
    let port = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
    (port != 0).then_some(ThreadCpuClock(u64::from(port)))
}

/// User + system CPU time the thread behind `clock` has used.
pub(crate) fn thread_cpu_ns(clock: ThreadCpuClock) -> Option<u64> {
    let port = libc::mach_port_t::try_from(clock.0).ok()?;
    let mut info: libc::thread_basic_info = unsafe { std::mem::zeroed() };
    let mut count = libc::THREAD_BASIC_INFO_COUNT;
    // SAFETY: `info` is a thread_basic_info and `count` its size in
    // integer_t units, as THREAD_BASIC_INFO requires.
    let result = unsafe {
        libc::thread_info(
            port,
            libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
            (&mut info as *mut libc::thread_basic_info).cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS {
        return None;
    }
    let micros = |time: libc::time_value_t| {
        u64::try_from(time.seconds).unwrap_or(0) * 1_000_000
            + u64::try_from(time.microseconds).unwrap_or(0)
    };
    Some((micros(info.user_time) + micros(info.system_time)) * 1_000)
}

/// Page faults that needed I/O (pages read back in) for this process so far.
pub(crate) fn process_major_faults() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the rusage it is given.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return None;
    }
    u64::try_from(usage.ru_majflt).ok()
}

/// Starts `/usr/bin/sample` on `pid` for `seconds`, writing its call graph
/// to `path`. The caller reaps the child; it never waits on it.
pub(crate) fn spawn_stack_sample(pid: u32, seconds: u32, path: &Path) -> std::io::Result<Child> {
    Command::new("/usr/bin/sample")
        .arg(pid.to_string())
        .arg(seconds.to_string())
        .arg("-mayDie")
        .arg("-file")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}
