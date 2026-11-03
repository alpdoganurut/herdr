//! The launcher: the state machine on explicit clocks, and the hub's side
//! against a fake sidecar (`browser::tests::fake_host`) and a fake spawner
//! (no Electron, no processes, no signals).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::api::schema::{
    BrowserActor, BrowserHostCdp, BrowserHostKind, BrowserHostRegisterParams, BrowserOp,
    BrowserRunParams,
};
use crate::browser::companion_install::{self, CompanionSetupOptions};
use crate::browser::setup::SetupEnv;
use crate::config::BrowserConfig;

// ----- the state machine ---------------------------------------------------

fn record(pid: u32, token: &str) -> CompanionRunRecord {
    CompanionRunRecord {
        pid,
        launch_token: token.into(),
        app: "0.2.0".into(),
        app_sha256: "a".repeat(64),
        ..Default::default()
    }
}

fn crashed() -> ExitInfo {
    ExitInfo {
        code: Some(1),
        signal: None,
        known: true,
    }
}

/// Spawn and record a process on `core` at `now`.
fn run_one(core: &mut Core, reason: StartReason, pid: u32, now: Instant) -> SpawnTicket {
    let ticket = core.begin_spawn(reason, now, 0).expect("a launch");
    assert!(core.spawned(ticket.generation, pid, record(pid, &ticket.token), true, 0));
    ticket
}

#[test]
fn fork_smoke_crashes_back_off_1_2_4_8_16_s_and_give_up_after_five_in_five_minutes() {
    assert_eq!(BACKOFF_SECS, [1, 2, 4, 8, 16]);
    let t0 = Instant::now();
    let mut core = Core::new(0);
    run_one(&mut core, StartReason::OnDemand, 100, t0);
    let mut now = t0;
    let mut gaps = Vec::new();
    for n in 1..=4u32 {
        let generation = core.generation;
        assert!(core.exited(generation, &crashed(), &["boom".into()], now, 0));
        assert_eq!(core.phase, Phase::Crashed, "crash {n}");
        assert_eq!(core.crashes_in_window(now), n);
        let due = core.next_retry.expect("a restart is scheduled");
        gaps.push(due.duration_since(now));
        assert!(
            core.due(due - Duration::from_millis(1), false).is_empty(),
            "not before its time"
        );
        assert_eq!(core.due(due, false), vec![Action::Respawn]);
        now = due;
        let ticket = run_one(&mut core, StartReason::CrashRestart, 100 + n, now);
        assert!(ticket.background, "a crash restart never shows the window");
    }
    assert_eq!(
        gaps,
        [1, 2, 4, 8].map(Duration::from_secs).to_vec(),
        "1, 2, 4, 8 s between crash restarts"
    );
    // the fifth crash within five minutes: no more restarts
    let generation = core.generation;
    core.exited(generation, &crashed(), &["the last words".into()], now, 0);
    assert_eq!(core.phase, Phase::Failed);
    assert!(core.due(now + Duration::from_secs(3600), false).is_empty());
    assert!(
        core.begin_spawn(StartReason::OnDemand, now, 0).is_none(),
        "failed: an open does not relaunch it"
    );
    assert_eq!(
        core.last_error.as_deref(),
        Some("exited with status 1 — the last words")
    );
    // an explicit start clears it and the counter
    core.reset_backoff(0);
    assert_eq!(core.phase, Phase::Stopped);
    assert_eq!(core.crashes_in_window(now), 0);
    assert!(core.last_error.is_none());
    run_one(&mut core, StartReason::User, 200, now);
    // crashes older than the window do not count: a later crash waits 1 s again
    let generation = core.generation;
    core.exited(generation, &crashed(), &[], now, 0);
    let later = now + CRASH_WINDOW + Duration::from_secs(1);
    run_one(&mut core, StartReason::CrashRestart, 201, later);
    let generation = core.generation;
    core.exited(generation, &crashed(), &[], later, 0);
    assert_eq!(core.crashes_in_window(later), 1);
    assert_eq!(
        core.next_retry.map(|at| at.duration_since(later)),
        Some(Duration::from_secs(1))
    );
    // the table's cap: the step never grows past its last entry
    assert_eq!(
        BACKOFF_SECS[(CRASH_LIMIT + 3).min(BACKOFF_SECS.len() - 1)],
        16
    );
}

#[test]
fn a_clean_exit_or_an_unregister_is_a_stop_not_a_crash() {
    let t0 = Instant::now();
    let mut core = Core::new(0);
    let ticket = run_one(&mut core, StartReason::User, 10, t0);
    assert!(core.registered(Some(&ticket.token), 0));
    assert_eq!(core.phase, Phase::Running);
    // the user closed its window: before-quit unregistered, then it exited
    core.unregistered(Some(&ticket.token));
    let generation = core.generation;
    core.exited(generation, &crashed(), &[], t0, 0);
    assert_eq!(core.phase, Phase::Stopped, "no relaunch a second later");
    assert_eq!(core.crashes_in_window(t0), 0);
    // exit status 0 without an unregister: stopped too
    run_one(&mut core, StartReason::User, 11, t0);
    let generation = core.generation;
    core.exited(
        generation,
        &ExitInfo {
            code: Some(0),
            signal: None,
            known: true,
        },
        &[],
        t0,
        0,
    );
    assert_eq!(core.phase, Phase::Stopped);
    // a late report of an older process changes nothing
    run_one(&mut core, StartReason::User, 12, t0);
    assert!(!core.exited(generation, &crashed(), &[], t0, 0));
    assert_eq!(core.phase, Phase::Starting);
}

#[test]
fn no_registration_in_60_s_and_a_lost_lease_stop_it_with_term_then_kill() {
    let t0 = Instant::now();
    let mut core = Core::new(0);
    run_one(&mut core, StartReason::OnDemand, 30, t0);
    assert!(core.due(t0 + Duration::from_secs(59), false).is_empty());
    let at = t0 + REGISTER_DEADLINE;
    assert_eq!(
        core.due(at, false),
        vec![Action::Signal {
            pid: 30,
            generation: core.generation,
            signal: StopSignal::Term
        }]
    );
    assert!(core.due(at + Duration::from_secs(1), false).is_empty());
    assert_eq!(
        core.due(at + TERM_WAIT, false),
        vec![Action::Signal {
            pid: 30,
            generation: core.generation,
            signal: StopSignal::Kill
        }]
    );
    let generation = core.generation;
    // SIGTERM'd Electron exits 0: still the crash herdr caused
    core.exited(
        generation,
        &ExitInfo {
            code: Some(0),
            signal: None,
            known: true,
        },
        &[],
        at + TERM_WAIT,
        0,
    );
    assert_eq!(core.phase, Phase::Crashed);
    assert_eq!(
        core.last_error.as_deref(),
        Some("did not register within 60 s")
    );

    // a registered one whose lease went: 30 s, then the same ladder
    let mut core = Core::new(0);
    let ticket = run_one(&mut core, StartReason::User, 31, t0);
    core.registered(Some(&ticket.token), 0);
    assert!(core.due(t0, true).is_empty());
    assert!(core.due(t0 + Duration::from_secs(1), false).is_empty());
    assert!(core.due(t0 + Duration::from_secs(30), false).is_empty());
    assert_eq!(
        core.due(t0 + Duration::from_secs(31), false),
        vec![Action::Signal {
            pid: 31,
            generation: core.generation,
            signal: StopSignal::Term
        }]
    );
    let generation = core.generation;
    core.exited(generation, &crashed(), &[], t0 + Duration::from_secs(32), 0);
    assert_eq!(core.last_error.as_deref(), Some("stopped heartbeating"));

    // an adopted one gets its grace before the lease rule runs
    let mut core = Core::new(0);
    core.adopt(record(32, "tok"), t0, 0);
    assert_eq!(core.phase, Phase::Reconnecting);
    let poll = |core: &Core| Action::Poll {
        pid: 32,
        generation: core.generation,
    };
    assert_eq!(core.due(t0, false), vec![poll(&core)]);
    assert_eq!(
        core.due(t0 + Duration::from_secs(29), false),
        vec![poll(&core)]
    );
    let late = core.due(t0 + Duration::from_secs(30), false);
    assert!(
        late.contains(&Action::Signal {
            pid: 32,
            generation: core.generation,
            signal: StopSignal::Term
        }),
        "{late:?}"
    );
    // and one that re-registers is running
    let mut core = Core::new(0);
    core.adopt(record(33, "tok"), t0, 0);
    assert!(core.registered(Some("tok"), 0));
    assert_eq!(core.phase, Phase::Running);
    assert!(
        !core.registered(Some("other"), 0),
        "another token is not ours"
    );
}

#[test]
fn uptimes_and_countdowns_read_short() {
    assert_eq!(duration_text(0), "0s");
    assert_eq!(duration_text(45), "45s");
    assert_eq!(duration_text(180), "3m");
    assert_eq!(duration_text(185), "3m 5s");
    assert_eq!(duration_text(2 * 3600 + 3 * 60 + 9), "2h 3m");
    assert_eq!(duration_text(26 * 3600), "1d 2h");
    assert_eq!(
        ExitInfo {
            code: None,
            signal: Some(9),
            known: true
        }
        .describe(),
        "killed by signal 9"
    );
    assert_eq!(ExitInfo::unknown().describe(), "exited (status unknown)");
}

#[test]
fn a_crash_reads_only_the_end_of_a_long_log() {
    let dir = temp_home("log-tail");
    let log = dir.join("companion.log");
    // short: the whole file
    std::fs::write(&log, "one\n\n  two  \nthree\nfour\n").unwrap();
    assert_eq!(log_tail(&log, 3), ["two", "three", "four"]);
    assert_eq!(log_tail(&log, 9), ["one", "two", "three", "four"]);
    // long: its last 64 KiB, which begin mid-line (that piece is no line)
    let mut text = "x".repeat(70 * 1024);
    text.push_str("\nthe last words\ncrashed\n");
    std::fs::write(&log, &text).unwrap();
    assert_eq!(log_tail(&log, 5), ["the last words", "crashed"]);
    let mut text = String::new();
    for i in 0..20_000 {
        text.push_str(&format!("line {i}\n"));
    }
    std::fs::write(&log, &text).unwrap();
    assert_eq!(log_tail(&log, 2), ["line 19998", "line 19999"]);
    assert!(log_tail(&dir.join("missing.log"), 3).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// ----- the hub's side --------------------------------------------------------

/// A spawner that starts nothing: pids are numbers, `register` makes the
/// "process" register with the hub (its token from the launch arguments).
#[derive(Default)]
pub(crate) struct FakeSpawner {
    pub(crate) hub: Mutex<Option<BrowserHub>>,
    pub(crate) specs: Mutex<Vec<LaunchSpec>>,
    pub(crate) alive: Mutex<HashSet<u32>>,
    /// A pid another process reuses now: its start token (else [`fake_token`]).
    pub(crate) tokens: Mutex<HashMap<u32, u64>>,
    pub(crate) signals: Mutex<Vec<(u32, StopSignal)>>,
    /// Signals the fake process exits on.
    pub(crate) exits_on: Mutex<Vec<StopSignal>>,
    pub(crate) register: std::sync::atomic::AtomicBool,
    pub(crate) next_pid: AtomicU32,
    /// How long a spawn takes.
    pub(crate) spawn_delay: Mutex<Duration>,
    /// A spawn is under way (set as it starts).
    pub(crate) in_spawn: std::sync::atomic::AtomicBool,
}

/// The start token a fake process has (until another one reuses its pid).
pub(crate) fn fake_token(pid: u32) -> u64 {
    1_000_000 + u64::from(pid)
}

impl FakeSpawner {
    pub(crate) fn new(hub: &BrowserHub, register: bool) -> Arc<Self> {
        let spawner = Arc::new(Self {
            hub: Mutex::new(Some(hub.clone())),
            next_pid: AtomicU32::new(5000),
            exits_on: Mutex::new(vec![StopSignal::Term, StopSignal::Kill]),
            ..Default::default()
        });
        spawner.register.store(register, Ordering::Relaxed);
        hub.launcher().set_spawner(spawner.clone());
        spawner
    }

    pub(crate) fn spawns(&self) -> usize {
        self.specs.lock().unwrap().len()
    }

    pub(crate) fn last_args(&self) -> Vec<String> {
        self.specs
            .lock()
            .unwrap()
            .last()
            .map(|spec| spec.args.clone())
            .unwrap_or_default()
    }

    pub(crate) fn signals(&self) -> Vec<StopSignal> {
        self.signals
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| *s)
            .collect()
    }
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn register_with(hub: &BrowserHub, instance: &str, token: Option<&str>, pid: u32) {
    let params = BrowserHostRegisterParams {
        kind: BrowserHostKind::Companion,
        instance: instance.into(),
        pid: Some(pid),
        cdp: BrowserHostCdp {
            host: "127.0.0.1".into(),
            port: 40000,
        },
        nonce: format!("nonce-{instance}"),
        version: "0.2.0".into(),
        protocol: 1,
        capabilities: vec!["views".into()],
        launch_token: token.map(str::to_string),
        managed: token.is_some(),
    };
    hub.host_register(&params).expect("register");
}

impl Spawner for FakeSpawner {
    fn spawn(&self, spec: &LaunchSpec) -> std::io::Result<Spawned> {
        self.in_spawn.store(true, Ordering::Relaxed);
        let delay = *self.spawn_delay.lock().unwrap();
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let pid = self.next_pid.fetch_add(1, Ordering::Relaxed);
        self.specs.lock().unwrap().push(spec.clone());
        self.alive.lock().unwrap().insert(pid);
        if self.register.load(Ordering::Relaxed) {
            let token = arg_after(&spec.args, "--launch-token");
            let hub = self.hub.lock().unwrap().clone();
            if let Some(hub) = hub {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(40));
                    register_with(&hub, &format!("fake-{pid}"), token.as_deref(), pid);
                });
            }
        }
        Ok(Spawned { pid, child: None })
    }

    fn signal(&self, pid: u32, signal: StopSignal) {
        self.signals.lock().unwrap().push((pid, signal));
        if self.exits_on.lock().unwrap().contains(&signal) {
            self.alive.lock().unwrap().remove(&pid);
        }
    }

    fn facts(&self, pid: u32) -> Option<ProcessFacts> {
        self.alive
            .lock()
            .unwrap()
            .contains(&pid)
            .then(|| ProcessFacts {
                start_token: Some(
                    self.tokens
                        .lock()
                        .unwrap()
                        .get(&pid)
                        .copied()
                        .unwrap_or_else(|| fake_token(pid)),
                ),
                started_ms: None,
            })
    }
}

pub(crate) fn wait_for(what: &str, within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn pane(id: &str) -> BrowserActor {
    BrowserActor::Pane {
        pane_id: id.into(),
        tab_id: "w1:t1".into(),
        workspace_id: "w1".into(),
        tab_label: "planner".into(),
        workspace_label: None,
        agent: Some("claude".into()),
        session: "default".into(),
        gone: false,
        shell_pid: None,
    }
}

fn open(url: &str) -> BrowserRunParams {
    BrowserRunParams {
        caller: None,
        profile: None,
        tab: None,
        op: BrowserOp::Open {
            url: url.into(),
            focus: false,
            wait: None,
        },
        timeout_ms: None,
    }
}

fn temp_home(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("herdr-companion-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An installed companion (a fake Electron dist, no branding) under `home`.
fn install(home: &Path) {
    let dist = home.join("electron-dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("version"), companion_install::ELECTRON_VERSION).unwrap();
    let bundle = if cfg!(target_os = "macos") {
        let app = dist.join("Electron.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), "<plist/>").unwrap();
        app
    } else {
        dist.clone()
    };
    let exe = companion_install::bundle_executable(&bundle);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "#!/bin/sh\n").unwrap();
    let env = SetupEnv {
        browser_home: home.to_path_buf(),
        binary: PathBuf::from("/usr/bin/true"),
        shell_file: home.join("shell.zsh"),
        zshrc: None,
        claude_json: None,
        claude_bin: None,
        claude_z_bin: None,
        codex_config: None,
        codex_bin: None,
        home: Some(home.to_path_buf()),
        node_override: None,
    };
    companion_install::setup_companion(
        &env,
        &BrowserConfig::default(),
        &CompanionSetupOptions {
            consent: false,
            dist_override: Some(dist),
            brand: false,
        },
    )
    .expect("install");
}

fn companion_config() -> BrowserConfig {
    BrowserConfig {
        host: "companion".into(),
        ..BrowserConfig::default()
    }
}

/// A hub on a temporary home with the fake sidecar, the companion
/// installed, `[browser] host = "companion"`.
pub(crate) fn companion_hub(
    name: &str,
    installed: bool,
) -> (BrowserHub, PathBuf, crate::browser::tests::Shared) {
    let home = temp_home(name);
    let hub = BrowserHub::with_home(companion_config(), home.clone());
    let (shared, stream) = crate::browser::tests::fake_host(&hub);
    // the far end lives as long as the test process
    std::mem::forget(stream);
    hub.launcher().set_socket(home.join("herdr-test.sock"));
    hub.set_companion_register_wait_for_test(Duration::from_secs(10));
    if installed {
        install(&home);
    }
    (hub, home, shared)
}

#[test]
fn fork_smoke_an_open_starts_the_installed_companion_and_waits_for_its_registration() {
    let (hub, home, _) = companion_hub("on-demand", true);
    let spawner = FakeSpawner::new(&hub, true);
    assert_eq!(hub.companion_app_info().state, "stopped");
    let result = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect("the open waits for the registration, then lands");
    assert_eq!(result.data["host"], "companion");
    assert_eq!(spawner.spawns(), 1);
    let args = spawner.last_args();
    assert!(args.contains(&"--managed".to_string()), "{args:?}");
    assert!(
        args.contains(&"--background".to_string()),
        "an agent's open never shows the window: {args:?}"
    );
    assert_eq!(
        arg_after(&args, "--socket").as_deref(),
        Some(home.join("herdr-test.sock").to_str().unwrap())
    );
    assert_eq!(
        arg_after(&args, "--profile-dir").map(PathBuf::from),
        Some(companion_install::profile_dir(&hub.companion_home()))
    );
    let token = arg_after(&args, "--launch-token").expect("a token");
    let info = hub.companion_app_info();
    assert_eq!(info.state, "running");
    assert!(info.managed);
    assert_eq!(
        info.installed.as_deref(),
        Some(crate::integration::companion_assets::COMPANION_APP_VERSION)
    );
    let run = read_run_record(&hub.companion_home()).expect("run.json");
    assert_eq!(run.launch_token, token);
    assert_eq!(Some(run.pid), info.pid);
    // the next open finds it registered: no second launch
    hub.run(&pane("w1:p1"), open("https://example.test/2"))
        .expect("second open");
    assert_eq!(spawner.spawns(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn concurrent_opens_share_one_launch() {
    let (hub, home, _) = companion_hub("single-flight", true);
    let spawner = FakeSpawner::new(&hub, true);
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let hub = hub.clone();
            std::thread::spawn(move || {
                hub.run(&pane(&format!("w1:p{i}")), open("https://example.test/"))
                    .map(|result| result.data["host"].clone())
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap().expect("open"), "companion");
    }
    assert_eq!(spawner.spawns(), 1, "one launch for every caller");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_open_without_an_install_names_the_setup_command() {
    let (hub, home, _) = companion_hub("not-installed", false);
    let spawner = FakeSpawner::new(&hub, true);
    assert_eq!(hub.companion_app_info().state, "not_installed");
    let err = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect_err("nothing to start");
    assert_eq!(err.code, "host_unavailable");
    assert_eq!(
        err.message,
        "the companion browser is not installed; ask the user to run \"herdr browser setup --companion\" (downloads Electron, about 115 MB)"
    );
    assert_eq!(spawner.spawns(), 0);
    // installed without Electron
    install(&home);
    let exe = companion_install::resolve(&hub.companion_home())
        .unwrap()
        .executable;
    std::fs::remove_file(exe).unwrap();
    let err = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect_err("no Electron");
    assert_eq!(
        err.message,
        "the companion browser is installed without Electron; ask the user to run \"herdr browser setup --companion\" in a terminal"
    );
    assert_eq!(spawner.spawns(), 0);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_open_while_it_failed_names_the_restart_and_a_silent_start_times_out() {
    let (hub, home, _) = companion_hub("failed", true);
    let spawner = FakeSpawner::new(&hub, false);
    {
        let mut core = hub.launcher().lock();
        core.phase = Phase::Failed;
        core.last_error = Some("exited with status 1 — boom".into());
    }
    let err = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect_err("failed");
    assert_eq!(
        err.message,
        "the companion browser crashed 5 times in 5 minutes and herdr stopped restarting it (last error: exited with status 1 — boom); ask the user to run \"herdr browser companion restart\""
    );
    assert_eq!(spawner.spawns(), 0);
    // a launch that never registers: the bounded wait names the log
    hub.launcher().lock().reset_backoff(0);
    hub.set_companion_register_wait_for_test(Duration::from_millis(400));
    let err = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect_err("never registers");
    assert_eq!(spawner.spawns(), 1);
    assert!(
        err.message
            .starts_with("the companion browser did not start within 0 s (log: ")
            && err.message.contains("companion.log")
            && err.message.ends_with(
                "retry once, then ask the user to run \"herdr browser companion status\""
            ),
        "{}",
        err.message
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Stop with millisecond waits (the real ladder takes seconds).
const QUICK_LADDER: Ladder = Ladder {
    quit: Duration::from_millis(300),
    unattached: Duration::from_millis(50),
    term: Duration::from_millis(300),
    kill: Duration::from_millis(300),
};

fn quick_stop(hub: &BrowserHub) {
    hub.companion_stop_blocking(QUICK_LADDER);
}

#[test]
fn stop_quits_through_the_control_page_then_term_then_kill() {
    let (hub, home, shared) = companion_hub("stop", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    // a companion that ignores the quit and SIGTERM
    *spawner.exits_on.lock().unwrap() = vec![StopSignal::Kill];
    quick_stop(&hub);
    let quits = shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .filter(|(op, _, _)| op == "control_quit")
        .count();
    assert_eq!(quits, 1, "the control page's quit first");
    assert_eq!(spawner.signals(), vec![StopSignal::Term, StopSignal::Kill]);
    let info = hub.companion_app_info();
    assert_eq!(info.state, "stopped");
    assert!(
        read_run_record(&hub.companion_home()).is_none(),
        "run.json removed"
    );
    assert_eq!(info.restarts, 0, "a stop is not a crash");

    // one that goes on SIGTERM never sees SIGKILL
    spawner.signals.lock().unwrap().clear();
    *spawner.exits_on.lock().unwrap() = vec![StopSignal::Term, StopSignal::Kill];
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the second registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    quick_stop(&hub);
    assert_eq!(spawner.signals(), vec![StopSignal::Term]);
    assert_eq!(hub.companion_app_info().state, "stopped");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fork_smoke_a_new_server_adopts_the_running_companion_from_its_run_record() {
    let (hub, home, _) = companion_hub("adopt", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.companion_start(StartReason::Agent).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    let run = read_run_record(&hub.companion_home()).expect("run.json");

    // the next server (a live handoff): same home, same socket, its own sidecar
    let next = BrowserHub::with_home(companion_config(), home.clone());
    let (_shared, stream) = crate::browser::tests::fake_host(&next);
    std::mem::forget(stream);
    next.launcher().set_socket(home.join("herdr-test.sock"));
    next.launcher().set_spawner(spawner.clone());
    *spawner.hub.lock().unwrap() = Some(next.clone());
    next.adopt_companion();
    let info = next.companion_app_info();
    assert_eq!(info.state, "reconnecting");
    assert_eq!(info.pid, Some(run.pid));
    // it re-registers with the same token: managed, never relaunched
    register_with(&next, "fake-again", Some(&run.launch_token), run.pid);
    let info = next.companion_app_info();
    assert_eq!(info.state, "running");
    assert!(info.managed);
    assert_eq!(spawner.spawns(), 1, "adopted, not relaunched");

    // its exit is noticed by the pid (no child to wait on)
    spawner.alive.lock().unwrap().remove(&run.pid);
    wait_for("the exit", Duration::from_secs(5), || {
        next.companion_app_info().state != "running"
    });
    assert!(read_run_record(&home.join("companion-app")).is_none());

    // a run record of another session is left alone, and routing names it
    let other = BrowserHub::with_home(companion_config(), home.clone());
    other.launcher().set_socket(home.join("another.sock"));
    other.launcher().set_spawner(spawner.clone());
    spawner.alive.lock().unwrap().insert(77);
    write_run_record(
        &other.companion_home(),
        &CompanionRunRecord {
            pid: 77,
            launch_token: "t".into(),
            start_token: fake_token(77),
            socket: home.join("herdr-test.sock").display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    other.adopt_companion();
    assert_eq!(other.companion_app_info().state, "stopped");
    let err = other
        .ensure_companion(Duration::from_millis(200), false)
        .expect_err("another session's");
    assert_eq!(
        err.message,
        format!(
            "the companion runs for another herdr session ({}); stop it there first",
            home.join("herdr-test.sock").display()
        )
    );
    // and a dead pid's record is cleared
    spawner.alive.lock().unwrap().remove(&77);
    other.adopt_companion();
    assert!(read_run_record(&other.companion_home()).is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_companion_started_by_hand_is_routed_to_but_never_stopped() {
    let (hub, home, _) = companion_hub("by-hand", true);
    let spawner = FakeSpawner::new(&hub, false);
    register_with(&hub, "by-hand", None, 4242);
    let info = hub.companion_app_info();
    assert_eq!(info.state, "external");
    assert!(!info.managed);
    assert_eq!(info.pid, Some(4242));
    let result = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect("routed to the running companion");
    assert_eq!(result.data["host"], "companion");
    assert_eq!(spawner.spawns(), 0, "nothing launched");
    assert_eq!(
        hub.companion_restart(StartReason::User).unwrap().as_deref(),
        Some(EXTERNAL_NOTE)
    );
    assert_eq!(
        hub.companion_stop().unwrap().as_deref(),
        Some(EXTERNAL_NOTE)
    );
    assert!(spawner.signals().is_empty(), "never signalled");
    assert!(
        !hub.companion_live(),
        "only its lease went (it re-registers)"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn launches_for_agents_pane_callers_and_autostart_pass_background() {
    let (hub, home, _) = companion_hub("background", true);
    let spawner = FakeSpawner::new(&hub, true);
    let launch = |reason: StartReason| -> Vec<String> {
        hub.companion_start(reason).unwrap();
        wait_for("the registration", Duration::from_secs(5), || {
            hub.companion_app_info().state == "running"
        });
        let args = spawner.last_args();
        quick_stop(&hub);
        args
    };
    let user = launch(StartReason::User);
    assert!(
        !user.contains(&"--background".to_string()),
        "the user's start shows the window: {user:?}"
    );
    let agent = launch(StartReason::Agent);
    assert!(agent.contains(&"--background".to_string()), "{agent:?}");
    // autostart: only with the setting
    hub.companion_autostart();
    assert_eq!(spawner.spawns(), 2, "companion_autostart is off by default");
    hub.apply_config(&BrowserConfig {
        companion_autostart: true,
        ..companion_config()
    });
    hub.companion_autostart();
    wait_for("the autostart", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    assert_eq!(spawner.spawns(), 3);
    assert!(spawner.last_args().contains(&"--background".to_string()));
    assert!(hub.companion_app_info().autostart);
    quick_stop(&hub);
    // an open routed to it
    hub.run(&pane("w1:p1"), open("https://example.test/"))
        .unwrap();
    assert!(spawner.last_args().contains(&"--background".to_string()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn new_app_files_make_an_update_ready_without_a_restart() {
    let (hub, home, _) = companion_hub("update", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    assert!(!hub.companion_app_info().update_ready);
    // a newer herdr installed newer app files under the running one
    let companion_home = hub.companion_home();
    let mut record = companion_install::read_install(&companion_home).unwrap();
    record.app = "0.2.1".into();
    record.app_sha256 = "b".repeat(64);
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        companion_home.join(companion_install::INSTALL_FILE),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    let info = hub.companion_app_info();
    assert!(info.update_ready);
    assert_eq!(info.installed.as_deref(), Some("0.2.1"));
    assert_eq!(info.state, "running", "nothing restarts by itself");
    // the lease answer tells the app (its update chip)
    let lease_id = {
        let token = hub.launcher().lock().token.clone();
        assert!(token.is_some());
        hub.companion_lease_id_for_test().expect("a lease")
    };
    let lease = hub.host_heartbeat(&lease_id).unwrap();
    assert_eq!(lease.update.as_deref(), Some("0.2.1"));
    assert_eq!(spawner.spawns(), 1);
    assert!(spawner.signals().is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

// ----- audit 1 -----------------------------------------------------------------

#[test]
fn a_run_record_names_its_process_by_its_start_token_else_its_start_time() {
    let facts = |start_token: Option<u64>, started_ms: Option<u64>| ProcessFacts {
        start_token,
        started_ms,
    };
    let mut run = record(4242, "tok");
    run.start_token = 77;
    run.started_at = 1_000;
    // the start token decides when both have one
    assert_eq!(
        match_record(&run, Some(&facts(Some(77), None))),
        RecordedProcess::Same
    );
    assert_eq!(
        match_record(&run, Some(&facts(Some(78), Some(1_000_000)))),
        RecordedProcess::Gone,
        "a reused pid: another start token, whatever its start time"
    );
    // no token to read: the start time, near the record's started_at
    assert_eq!(
        match_record(&run, Some(&facts(None, Some(999_400)))),
        RecordedProcess::Same
    );
    assert_eq!(
        match_record(&run, Some(&facts(None, Some(4_600_000)))),
        RecordedProcess::Gone
    );
    // a record without a token: the start time too
    run.start_token = 0;
    assert_eq!(
        match_record(&run, Some(&facts(Some(5), Some(1_000_200)))),
        RecordedProcess::Same
    );
    // nothing to compare: never trusted
    assert_eq!(
        match_record(&run, Some(&facts(None, None))),
        RecordedProcess::Unknown
    );
    run.started_at = 0;
    assert_eq!(
        match_record(&run, Some(&facts(Some(5), Some(1)))),
        RecordedProcess::Unknown
    );
    // no process with that pid, or no pid at all
    assert_eq!(match_record(&run, None), RecordedProcess::Gone);
    run.pid = 0;
    assert_eq!(
        match_record(&run, Some(&facts(Some(5), None))),
        RecordedProcess::Gone
    );
}

#[test]
fn the_real_spawner_tells_this_process_from_a_reused_pid() {
    let pid = std::process::id();
    let facts = ProcessSpawner.facts(pid).expect("this process lives");
    let token = facts.start_token.expect("a start token on macOS and Linux");
    let mut run = record(pid, "tok");
    run.start_token = token;
    assert_eq!(match_record(&run, Some(&facts)), RecordedProcess::Same);
    run.start_token = token + 1;
    assert_eq!(match_record(&run, Some(&facts)), RecordedProcess::Gone);
    // a record without a token: this process started about when it says
    run.start_token = 0;
    run.started_at = facts.started_ms.expect("a start time") / 1000;
    assert_eq!(match_record(&run, Some(&facts)), RecordedProcess::Same);
    assert!(ProcessSpawner.facts(0).is_none());
}

#[test]
fn fork_smoke_a_reused_pid_is_never_adopted_or_signalled() {
    let (hub, home, _) = companion_hub("reused-pid", true);
    let spawner = FakeSpawner::new(&hub, false);
    let pid = 6100;
    spawner.alive.lock().unwrap().insert(pid);
    let run = CompanionRunRecord {
        pid,
        launch_token: "ab".repeat(16),
        start_token: fake_token(pid),
        started_at: unix_now(),
        socket: home.join("herdr-test.sock").display().to_string(),
        ..record(pid, "")
    };
    // the pid lives, but as another process now (another Electron, say)
    write_run_record(&hub.companion_home(), &run).unwrap();
    spawner.tokens.lock().unwrap().insert(pid, 42);
    hub.adopt_companion();
    assert_eq!(hub.companion_app_info().state, "stopped", "not adopted");
    assert!(
        read_run_record(&hub.companion_home()).is_none(),
        "its stale record goes"
    );
    // nor is it another session's companion that blocks a start
    let mut other = run.clone();
    other.socket = "/elsewhere/herdr.sock".into();
    write_run_record(&hub.companion_home(), &other).unwrap();
    assert!(hub.companion_other_session(&hub.companion_home()).is_none());
    spawner.tokens.lock().unwrap().remove(&pid);
    assert_eq!(
        hub.companion_other_session(&hub.companion_home())
            .as_deref(),
        Some("/elsewhere/herdr.sock"),
        "while the pid is still that process, it is"
    );

    // adopted while it was the companion; then it exits and another
    // process takes its pid: the stop finds it gone and signals nothing
    write_run_record(&hub.companion_home(), &run).unwrap();
    hub.adopt_companion();
    assert_eq!(hub.companion_app_info().state, "reconnecting");
    spawner.tokens.lock().unwrap().insert(pid, 43);
    hub.companion_stop_blocking(QUICK_LADDER);
    assert_eq!(hub.companion_app_info().state, "stopped");
    assert!(
        spawner.signals().is_empty(),
        "never signalled: {:?}",
        spawner.signals()
    );
    assert!(read_run_record(&hub.companion_home()).is_none());

    // and the driver's poll of an adopted pid that is another process now:
    // gone (a crash), never signalled
    write_run_record(&hub.companion_home(), &run).unwrap();
    spawner.tokens.lock().unwrap().remove(&pid);
    hub.adopt_companion();
    assert_eq!(hub.companion_app_info().state, "reconnecting");
    spawner.tokens.lock().unwrap().insert(pid, 44);
    wait_for("the poll", Duration::from_secs(5), || {
        hub.companion_app_info().state != "reconnecting"
    });
    hub.stop_companion_with_server();
    assert!(
        spawner
            .signals
            .lock()
            .unwrap()
            .iter()
            .all(|(p, _)| *p != pid),
        "{:?}",
        spawner.signals()
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_stop_whose_quit_gets_no_answer_keeps_its_deadline() {
    let (hub, home, shared) = companion_hub("quit-unanswered", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    // the control page never answers the quit and the app ignores it:
    // only SIGTERM ends it
    shared.lock().unwrap().silent.push("control_quit".into());
    *spawner.exits_on.lock().unwrap() = vec![StopSignal::Term];
    let started = Instant::now();
    quick_stop(&hub);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(250),
        "the quit had its wait: {took:?}"
    );
    assert!(
        took < Duration::from_secs(3),
        "the quit's reply is never waited for (the sidecar's grace alone is 5 s): {took:?}"
    );
    assert!(shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .any(|(op, _, _)| op == "control_quit"));
    assert_eq!(spawner.signals(), vec![StopSignal::Term]);
    assert_eq!(hub.companion_app_info().state, "stopped");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_companion_without_a_lease_gets_sigterm_after_a_short_grace() {
    let (hub, home, shared) = companion_hub("unattached", true);
    // it never registers (still starting, or its lease lost)
    let spawner = FakeSpawner::new(&hub, false);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the spawn", Duration::from_secs(5), || {
        hub.companion_app_info().pid.is_some()
    });
    let started = Instant::now();
    hub.companion_stop_blocking(Ladder {
        quit: Duration::from_secs(5),
        ..QUICK_LADDER
    });
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "no quit was sent, so none is waited for: {took:?}"
    );
    assert!(!shared
        .lock()
        .unwrap()
        .ops
        .iter()
        .any(|(op, _, _)| op == "control_quit"));
    assert_eq!(spawner.signals(), vec![StopSignal::Term]);
    assert_eq!(hub.companion_app_info().state, "stopped");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_stop_during_a_launch_calls_it_off_or_stops_what_it_spawned() {
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(8);
    // before the spawn: nothing is spawned, and no start until then
    let mut core = Core::new(0);
    let ticket = core.begin_spawn(StartReason::User, t0, 0).unwrap();
    assert_eq!(
        core.begin_stop(QUICK_LADDER, deadline, 0),
        StopTarget::Launch {
            generation: ticket.generation
        }
    );
    assert_eq!(core.phase, Phase::Stopping);
    assert!(
        core.begin_spawn(StartReason::OnDemand, t0, 0).is_none(),
        "no start while that stop is pending"
    );
    assert!(!core.launch_goes_ahead(ticket.generation, 0));
    assert_eq!(core.phase, Phase::Stopped);
    assert!(!core.busy());
    // during the spawn: the launch takes the stop over
    let ticket = core.begin_spawn(StartReason::User, t0, 0).unwrap();
    assert!(core.launch_goes_ahead(ticket.generation, 0));
    assert_eq!(
        core.begin_stop(QUICK_LADDER, deadline, 0),
        StopTarget::Launch {
            generation: ticket.generation
        }
    );
    assert!(core.spawned(ticket.generation, 9, record(9, &ticket.token), true, 0));
    assert_eq!(
        core.stop_pending.take(),
        Some(PendingStop {
            ladder: QUICK_LADDER,
            deadline
        })
    );
    // its exit is a stop, not a crash
    core.exited(ticket.generation, &crashed(), &[], t0, 0);
    assert_eq!(core.phase, Phase::Stopped);
    assert_eq!(core.crashes_in_window(t0), 0);
    // a spawn that fails while a stop waits: stopped too
    let ticket = core.begin_spawn(StartReason::User, t0, 0).unwrap();
    core.begin_stop(QUICK_LADDER, deadline, 0);
    core.spawn_failed(ticket.generation, "no such file".into(), t0, 0);
    assert_eq!(core.phase, Phase::Stopped);
    assert_eq!(core.crashes_in_window(t0), 0);
}

#[test]
fn a_stop_while_the_companion_is_spawned_stops_it_once_it_is() {
    let (hub, home, _) = companion_hub("stop-in-spawn", true);
    let spawner = FakeSpawner::new(&hub, false);
    *spawner.spawn_delay.lock().unwrap() = Duration::from_millis(300);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the spawn under way", Duration::from_secs(5), || {
        spawner.in_spawn.load(Ordering::Relaxed)
    });
    let started = Instant::now();
    // blocks until the launch has stopped what it spawned
    quick_stop(&hub);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(spawner.spawns(), 1);
    assert_eq!(
        spawner.signals(),
        vec![StopSignal::Term],
        "no lease of its own yet: SIGTERM after the grace"
    );
    let info = hub.companion_app_info();
    assert_eq!(info.state, "stopped");
    assert_eq!(info.restarts, 0);
    assert!(read_run_record(&hub.companion_home()).is_none());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_crash_restart_found_due_loses_to_a_stop_and_the_server_exit_launches_nothing() {
    let t0 = Instant::now();
    let mut core = Core::new(0);
    run_one(&mut core, StartReason::User, 40, t0);
    let generation = core.generation;
    core.exited(generation, &crashed(), &[], t0, 0);
    let due = core.next_retry.expect("a restart is scheduled");
    assert_eq!(core.due(due, false), vec![Action::Respawn]);
    // the user stops it before the driver acts on that
    assert_eq!(core.begin_stop(QUICK_LADDER, due, 0), StopTarget::Nothing);
    assert_eq!(core.phase, Phase::Stopped);
    assert!(core.next_retry.is_none());
    assert!(core
        .begin_spawn(StartReason::CrashRestart, due, 0)
        .is_none());
    // the server's exit: nothing launches at all
    core.shutting_down = true;
    assert!(core.begin_spawn(StartReason::User, due, 0).is_none());
    assert!(core.begin_spawn(StartReason::OnDemand, due, 0).is_none());
}

#[test]
fn fork_smoke_the_server_exit_cancels_a_scheduled_restart_and_refuses_launches() {
    let (hub, home, _) = companion_hub("exit-while-crashed", true);
    let spawner = FakeSpawner::new(&hub, true);
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    // it crashes: a restart is due in 1 s
    let pid = hub.companion_app_info().pid.expect("a pid");
    spawner.alive.lock().unwrap().remove(&pid);
    wait_for("the crash", Duration::from_secs(5), || {
        hub.companion_app_info().state == "crashed"
    });
    hub.stop_companion_with_server();
    let info = hub.companion_app_info();
    assert_eq!(info.state, "stopped");
    assert!(info.next_retry_unix.is_none());
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(spawner.spawns(), 1, "no crash restart after the exit");
    // nothing launches any more: a start, an agent's open
    hub.companion_start(StartReason::User).unwrap();
    let err = hub
        .run(&pane("w1:p1"), open("https://example.test/"))
        .expect_err("the server is exiting");
    assert_eq!(err.code, "host_unavailable");
    assert!(err.message.contains("shutting down"), "{}", err.message);
    assert_eq!(spawner.spawns(), 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn an_open_that_names_the_companion_starts_it_on_demand_too() {
    let (hub, home, _) = companion_hub("named-profile", true);
    let spawner = FakeSpawner::new(&hub, true);
    let mut params = open("https://example.test/");
    params.profile = Some(COMPANION_PROFILE.into());
    let result = hub
        .run(&pane("w1:p1"), params)
        .expect("started, then the open lands");
    assert_eq!(result.data["host"], "companion");
    assert_eq!(spawner.spawns(), 1);
    assert!(spawner.last_args().contains(&"--background".to_string()));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_launch_keeps_the_app_files_a_companion_still_runs_from() {
    let (hub, home, _) = companion_hub("keep-running-dir", true);
    let _spawner = FakeSpawner::new(&hub, true);
    let companion_home = hub.companion_home();
    let root = companion_install::app_root(&companion_home);
    for (name, age) in [
        ("aaaaaaaaaaaa", 4),
        ("bbbbbbbbbbbb", 3),
        ("cccccccccccc", 2),
    ] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("package.json"), "{}").unwrap();
        let when = std::time::SystemTime::now() - Duration::from_secs(age * 60);
        std::fs::File::open(root.join(name))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
    // the oldest is a live companion's (this test process stands in for it)
    write_run_record(
        &companion_home,
        &CompanionRunRecord {
            pid: std::process::id(),
            launch_token: "t".into(),
            app_dir: root.join("aaaaaaaaaaaa").display().to_string(),
            socket: "/elsewhere/herdr.sock".into(),
            ..Default::default()
        },
    )
    .unwrap();
    // the installed app files are an older herdr's: the launch refreshes them
    let mut installed = companion_install::read_install(&companion_home).unwrap();
    installed.app_sha256 = format!("{}{}", "c".repeat(12), "0".repeat(52));
    std::fs::write(
        companion_home.join(companion_install::INSTALL_FILE),
        serde_json::to_string_pretty(&installed).unwrap(),
    )
    .unwrap();
    hub.companion_start(StartReason::User).unwrap();
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    assert!(
        root.join("aaaaaaaaaaaa").is_dir(),
        "a running companion's directory stays"
    );
    assert!(!root.join("bbbbbbbbbbbb").exists(), "older ones go");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_users_own_start_says_what_to_run_an_agents_says_to_ask_the_user() {
    let (hub, home, _) = companion_hub("user-wording", false);
    let spawner = FakeSpawner::new(&hub, true);
    let not_installed = |run: &str| {
        format!("the companion browser is not installed; {run} \"herdr browser setup --companion\" (downloads Electron, about 115 MB)")
    };
    // the TUI, the CLI from a plain terminal, the user's own open
    assert_eq!(
        hub.companion_start(StartReason::User).unwrap_err().message,
        not_installed("run")
    );
    assert_eq!(
        hub.companion_focus(true).unwrap_err().message,
        not_installed("run")
    );
    assert_eq!(
        hub.companion_restart(StartReason::User)
            .unwrap_err()
            .message,
        not_installed("run")
    );
    assert_eq!(
        hub.run(&BrowserActor::User, open("https://example.test/"))
            .unwrap_err()
            .message,
        not_installed("run")
    );
    // an agent (the CLI in a pane, an MCP tool) asks the user
    assert_eq!(
        hub.companion_start(StartReason::Agent).unwrap_err().message,
        not_installed("ask the user to run")
    );
    assert_eq!(
        hub.run(&pane("w1:p1"), open("https://example.test/"))
            .unwrap_err()
            .message,
        not_installed("ask the user to run")
    );
    // it gave up: the restart to run, or to ask for
    install(&home);
    {
        let mut core = hub.launcher().lock();
        core.phase = Phase::Failed;
        core.last_error = Some("boom".into());
    }
    let failed = |run: &str| {
        format!("the companion browser crashed 5 times in 5 minutes and herdr stopped restarting it (last error: boom); {run} \"herdr browser companion restart\"")
    };
    assert_eq!(
        hub.run(&BrowserActor::User, open("https://example.test/"))
            .unwrap_err()
            .message,
        failed("run")
    );
    assert_eq!(
        hub.run(&pane("w1:p1"), open("https://example.test/"))
            .unwrap_err()
            .message,
        failed("ask the user to run")
    );
    assert_eq!(spawner.spawns(), 0);
    let _ = std::fs::remove_dir_all(&home);
}

// ----- what a launch needs (audit 3) ----------------------------------------

/// A hub on a temporary home without any sidecar (not even the fake one):
/// the browser helper is whatever its home holds.
fn helperless_hub(name: &str, config: BrowserConfig) -> (BrowserHub, PathBuf) {
    let home = temp_home(name);
    let hub = BrowserHub::with_home(config, home.clone());
    hub.launcher().set_socket(home.join("herdr-test.sock"));
    hub.set_companion_register_wait_for_test(Duration::from_secs(10));
    (hub, home)
}

/// The browser helper's runtime under `home`: its record and a
/// playwright-core of `version`; `current`: this herdr's scripts.
fn install_helper(home: &Path, version: &str, current: bool) {
    use crate::integration::browser_assets;
    let host_dir = home.join(browser_assets::HOST_DIR);
    let playwright = host_dir.join("node_modules").join("playwright-core");
    std::fs::create_dir_all(&playwright).unwrap();
    std::fs::write(
        playwright.join("package.json"),
        format!("{{\"version\":\"{version}\"}}"),
    )
    .unwrap();
    let runtime = crate::api::schema::BrowserRuntimeInfo {
        version: 1,
        node: "/usr/bin/true".into(),
        assets_sha256: if current {
            browser_assets::assets_sha256()
        } else {
            "older".into()
        },
        ..Default::default()
    };
    browser_assets::write_runtime(&host_dir, &runtime).unwrap();
}

#[test]
fn fork_smoke_no_companion_starts_without_the_browser_helper_it_registers_through() {
    let (hub, home) = helperless_hub("no-helper", companion_config());
    // the launched companion would never register: nothing may spawn
    let spawner = FakeSpawner::new(&hub, false);
    assert_eq!(
        hub.companion_start(StartReason::User).unwrap_err().message,
        "the companion browser and the browser helper are not installed; run \"herdr browser setup\" and \"herdr browser setup --companion\" (downloads Electron, about 115 MB)"
    );
    install(&home);
    let missing = |run: &str| {
        format!("the browser helper the companion browser registers through is not installed; {run} \"herdr browser setup\" in a terminal")
    };
    let err = hub.companion_start(StartReason::User).unwrap_err();
    assert_eq!(err.code, "host_unavailable");
    assert_eq!(err.message, missing("run"));
    assert_eq!(
        hub.companion_restart(StartReason::User)
            .unwrap_err()
            .message,
        missing("run")
    );
    assert_eq!(
        hub.companion_focus(true).unwrap_err().message,
        missing("run")
    );
    // an agent's open answers at once (no wait for a registration that
    // cannot come) and names the command
    let asked = Instant::now();
    assert_eq!(
        hub.run(&pane("w1:p1"), open("https://example.test/"))
            .unwrap_err()
            .message,
        missing("ask the user to run")
    );
    assert!(asked.elapsed() < Duration::from_secs(2), "no wait");
    // autostart says nothing and starts nothing
    hub.apply_config(&BrowserConfig {
        companion_autostart: true,
        ..companion_config()
    });
    hub.companion_autostart();
    assert_eq!(spawner.spawns(), 0);
    // a helper older than this herdr's pin (its scripts outdated too)
    install_helper(&home, "1.0.0", false);
    assert_eq!(
        hub.companion_start(StartReason::Agent).unwrap_err().message,
        format!(
            "the browser helper is outdated (playwright-core 1.0.0, this herdr needs {}); ask the user to run \"herdr browser setup\" in a terminal",
            crate::integration::browser_assets::PLAYWRIGHT_CORE_VERSION
        )
    );
    assert_eq!(spawner.spawns(), 0);
    // with the helper in place it starts
    install_helper(
        &home,
        crate::integration::browser_assets::PLAYWRIGHT_CORE_VERSION,
        false,
    );
    hub.companion_start(StartReason::User).expect("start");
    wait_for("the spawn", Duration::from_secs(5), || {
        spawner.spawns() == 1
    });
    quick_stop(&hub);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fork_smoke_a_config_that_keeps_the_companion_off_never_starts_it() {
    // the herdr+ window on and chosen: every registration would be refused
    let window = BrowserConfig {
        window: true,
        host: "window".into(),
        companion_autostart: true,
        ..BrowserConfig::default()
    };
    let (hub, home, _) = companion_hub("off-window", true);
    hub.apply_config(&window);
    let spawner = FakeSpawner::new(&hub, true);
    let info = hub.companion_app_info();
    assert_eq!(info.state, DISABLED_STATE);
    assert!(
        hub.get(None).companion_app.is_none(),
        "browser.get leaves it out: nothing offers it"
    );
    let err = hub.companion_start(StartReason::User).unwrap_err();
    assert_eq!(err.code, "host_disabled");
    assert_eq!(
        err.message,
        "the companion browser is off: this herdr uses only its own browser window ([browser] host = \"window\"); set [browser] host = \"companion\" to use it"
    );
    assert_eq!(
        hub.companion_start(StartReason::Agent).unwrap_err().message,
        "the companion browser is off: this herdr uses only its own browser window ([browser] host = \"window\"); ask the user to set [browser] host = \"companion\" to use it"
    );
    assert_eq!(
        hub.companion_restart(StartReason::User).unwrap_err().code,
        "host_disabled"
    );
    assert_eq!(hub.companion_focus(true).unwrap_err().code, "host_disabled");
    assert_eq!(
        hub.ensure_companion(Duration::from_secs(5), false)
            .unwrap_err()
            .code,
        "host_disabled"
    );
    hub.companion_autostart();
    assert!(hub
        .companion_off_note()
        .is_some_and(|note| note.contains("host = \"window\"")));
    // the browser disabled: the same, and a restart stops nothing
    hub.apply_config(&BrowserConfig {
        enabled: false,
        ..companion_config()
    });
    assert_eq!(hub.companion_app_info().state, DISABLED_STATE);
    assert!(hub.get(None).companion_app.is_none());
    assert_eq!(
        hub.companion_restart(StartReason::User).unwrap_err().code,
        "browser_disabled"
    );
    assert_eq!(
        hub.companion_start(StartReason::User).unwrap_err().code,
        "browser_disabled"
    );
    assert_eq!(spawner.spawns(), 0);
    // one still running when the config turned it off keeps its state (and Stop)
    hub.apply_config(&companion_config());
    hub.companion_start(StartReason::User).expect("start");
    wait_for("the registration", Duration::from_secs(5), || {
        hub.companion_app_info().state == "running"
    });
    hub.apply_config(&window);
    assert_eq!(hub.companion_app_info().state, "running");
    assert!(hub.get(None).companion_app.is_some());
    quick_stop(&hub);
    assert_eq!(hub.companion_app_info().state, DISABLED_STATE);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_crash_restart_due_after_the_config_turned_the_companion_off_does_not_launch() {
    let (hub, home, _) = companion_hub("off-crashed", true);
    let spawner = FakeSpawner::new(&hub, false);
    {
        let mut core = hub.launcher().lock();
        core.phase = Phase::Crashed;
        core.next_retry = Some(Instant::now());
        core.last_error = Some("exited with status 1".into());
    }
    hub.apply_config(&BrowserConfig {
        enabled: false,
        ..companion_config()
    });
    // the driver finds the restart due
    hub.ensure_companion_driver();
    wait_for("the restart called off", Duration::from_secs(5), || {
        hub.launcher().lock().phase == Phase::Stopped
    });
    assert_eq!(spawner.spawns(), 0);
    assert_eq!(hub.companion_app_info().state, DISABLED_STATE);
    let _ = std::fs::remove_dir_all(&home);
}
