//! Launching and attaching to Chromium: executable resolution, the
//! herdr-picked DevTools port, the argv (never an automation switch), the
//! `run/<name>.json` record, `SingletonLock` and `/json/version` probes.
//!
//! On macOS the browser is started through LaunchServices (`open -n -g -a
//! <bundle> --args …`): a direct spawn from a server that adopted the
//! per-user service context gets a window the WindowServer never shows
//! (P0 spike, item 1). The pid comes from the profile's `SingletonLock`
//! symlink (`<host>-<pid>`), cross-checked by the DevTools port answering.
//! Chromium 154 does not write `DevToolsActivePort` for a fixed port, so the
//! run record is the only endpoint source.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::BrowserError;

pub const RUN_DIR: &str = "run";
/// The profile log is truncated when it grows past this.
pub const MAX_PROFILE_LOG_BYTES: u64 = 5 * 1024 * 1024;
/// Launch attempts before giving up (a port grabbed between pick and exec).
pub const LAUNCH_ATTEMPTS: u32 = 3;
const DEVTOOLS_POLL: Duration = Duration::from_millis(100);
const HTTP_TIMEOUT: Duration = Duration::from_millis(1500);
/// `/json/version` is a few hundred bytes; anything past this is not Chromium.
const MAX_VERSION_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    /// The `.app` bundle (macOS), when known.
    pub bundle: Option<PathBuf>,
    /// The binary inside the bundle, or the binary itself elsewhere.
    pub binary: PathBuf,
    /// `config`, `home (branded)` (`~/Applications/herdr+ Browser.app`), `home`
    /// (`~/Applications/Chromium.app`), `system` (`/Applications`), `path`.
    pub source: &'static str,
}

impl Executable {
    /// What status and logs show.
    pub fn display(&self) -> String {
        self.bundle
            .as_deref()
            .unwrap_or(&self.binary)
            .display()
            .to_string()
    }
}

const BUNDLE_BINARY: &str = "Contents/MacOS/Chromium";

fn bundle_executable(bundle: &Path, source: &'static str) -> Option<Executable> {
    let binary = bundle.join(BUNDLE_BINARY);
    binary.is_file().then(|| Executable {
        bundle: Some(bundle.to_path_buf()),
        binary,
        source,
    })
}

/// Resolve `[browser] executable`. `home` is `$HOME` (for `~/Applications`).
pub fn resolve_executable(
    configured: &str,
    home: Option<&Path>,
) -> Result<Executable, BrowserError> {
    let configured = configured.trim();
    if configured == crate::config::AUTO_EXECUTABLE || configured.is_empty() {
        if let Some(home) = home {
            // The branded install (`herdr browser install-chromium`) first.
            let branded = home
                .join("Applications")
                .join(format!("{}.app", super::brand::DEFAULT_APP_NAME));
            if let Some(exe) = bundle_executable(&branded, "home (branded)") {
                return Ok(exe);
            }
            if let Some(exe) =
                bundle_executable(&home.join("Applications").join("Chromium.app"), "home")
            {
                return Ok(exe);
            }
        }
        if let Some(exe) = bundle_executable(Path::new("/Applications/Chromium.app"), "system") {
            return Ok(exe);
        }
        for candidate in [
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ] {
            let path = Path::new(candidate);
            if path.is_file() {
                return Ok(Executable {
                    bundle: None,
                    binary: path.to_path_buf(),
                    source: "path",
                });
            }
        }
        return Err(BrowserError::new(
            "browser_executable_missing",
            "no herdr+ Browser.app or Chromium.app in ~/Applications, no /Applications/Chromium.app; run `herdr browser install-chromium <Chromium.app>` or set [browser] executable to a bundle path",
        ));
    }
    let path = PathBuf::from(configured);
    if path.extension().is_some_and(|ext| ext == "app") {
        return bundle_executable(&path, "config").ok_or_else(|| {
            BrowserError::new(
                "browser_executable_missing",
                format!(
                    "[browser] executable = {configured:?}: no {BUNDLE_BINARY} inside that bundle"
                ),
            )
        });
    }
    if !path.is_file() {
        return Err(BrowserError::new(
            "browser_executable_missing",
            format!("[browser] executable = {configured:?} does not exist"),
        ));
    }
    // A binary inside a bundle: launch the bundle so LaunchServices knows the app.
    let bundle = path
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf);
    Ok(Executable {
        bundle,
        binary: path,
        source: "config",
    })
}

/// Bind `127.0.0.1:0`, read the port, close.
pub fn pick_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// The Chromium argv. `extra` is already filtered by the config.
pub fn argv(
    profile_dir: &Path,
    port: u16,
    restore: bool,
    extra: &[String],
    first_launch: bool,
    extension_dir: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        format!("--user-data-dir={}", profile_dir.display()),
        format!("--remote-debugging-port={port}"),
        // (no --disable-blink-features=AutomationControlled: on a fixed port this
        // build reports navigator.webdriver = false without it, and the flag only
        // drew the "unsupported command-line flag" infobar)
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-timer-throttling".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-renderer-backgrounding".to_string(),
    ];
    if restore {
        argv.push("--restore-last-session".to_string());
    }
    // The companion extension (tab groups): herdr's own switch, like the
    // profile dir; a user copy in extra_args is dropped by the filter below.
    if let Some(dir) = extension_dir {
        argv.push(format!("--load-extension={}", dir.display()));
    }
    for arg in extra {
        if crate::config::is_forbidden_switch(arg) {
            continue;
        }
        argv.push(arg.clone());
    }
    if first_launch {
        argv.push("about:blank".to_string());
    }
    argv
}

/// `GET /json/version` on the loopback DevTools port. Chromium rejects a
/// foreign `Host`, so it is the loopback address, and no `Origin` is sent.
pub fn json_version(port: u16, timeout: Duration) -> Option<serde_json::Value> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    stream
        .write_all(
            format!(
                "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    let mut response = Vec::new();
    let _ = (&mut stream)
        .take(MAX_VERSION_RESPONSE_BYTES)
        .read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return None;
    }
    serde_json::from_str(body.trim()).ok()
}

/// Poll `/json/version` until it answers or the deadline passes.
pub fn wait_for_devtools(port: u16, deadline: Instant) -> Option<serde_json::Value> {
    loop {
        if let Some(version) = json_version(port, HTTP_TIMEOUT) {
            return Some(version);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(DEVTOOLS_POLL);
    }
}

/// The pid Chromium recorded in `<profile>/SingletonLock` (`<host>-<pid>`).
pub fn singleton_lock_pid(profile_dir: &Path) -> Option<u32> {
    let target = fs::read_link(profile_dir.join("SingletonLock")).ok()?;
    parse_singleton_lock(&target.to_string_lossy())
}

/// The pid of a live Chromium holding `profile_dir`: its `SingletonLock` pid
/// is alive and its `SingletonSocket` accepts a connection, or the socket
/// does not answer but the pid is the browser (`is_browser`, by executable
/// name like Chromium's own `IsChromeProcess`: a busy or wedged browser still
/// holds it). A
/// lock whose browser died and whose pid the OS gave to another process has
/// no listening socket and no browser behind the pid, so it is stale
/// (Chromium takes such a lock over itself).
pub fn singleton_lock_holder(
    profile_dir: &Path,
    alive: &dyn Fn(u32) -> bool,
    is_browser: &dyn Fn(u32) -> bool,
) -> Option<u32> {
    let pid = singleton_lock_pid(profile_dir).filter(|pid| alive(*pid))?;
    (singleton_socket_answers(profile_dir) || is_browser(pid)).then_some(pid)
}

/// Whether process `pid` is the browser: the configured `binary` (by its
/// exact file name) or the branded app; Chromium or Chrome by name only when
/// no binary resolves. `false` when its name cannot be read (Windows and
/// other platforms: the socket alone decides, as before).
pub fn is_browser_process(pid: u32, binary: Option<&Path>) -> bool {
    crate::platform::process_comm_name(pid)
        .is_some_and(|name| is_browser_process_name(&name, binary))
}

/// [`is_browser_process`] on a process name, like Chromium's own
/// `IsChromeProcess` (the exact executable name): the resolved `binary`'s
/// file name, or [`super::brand::DEFAULT_APP_NAME`]. The name may be cut
/// short (`comm`: 15 bytes on Linux, 16 on macOS), so the binary also matches
/// by a long enough prefix. A helper, crash handler or driver
/// (`Google Chrome He`, `chrome_crashpad_handler`, `chromedriver`) is not the
/// browser. Only without a resolved binary does any `chromium`/`chrome` name
/// count.
pub fn is_browser_process_name(name: &str, binary: Option<&Path>) -> bool {
    const COMM_PREFIX_MIN: usize = 15;
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    if name.to_lowercase() == super::brand::DEFAULT_APP_NAME.to_lowercase() {
        return true;
    }
    match binary.and_then(Path::file_name) {
        Some(file) => {
            let file = file.to_string_lossy();
            file == name || (name.len() >= COMM_PREFIX_MIN && file.starts_with(name))
        }
        None => {
            let lower = name.to_lowercase();
            lower.contains("chromium") || lower.contains("chrome")
        }
    }
}

/// Whether the profile's `SingletonSocket` (Chromium's single-instance
/// socket) accepts a connection. Without Unix sockets the live pid decides.
pub fn singleton_socket_answers(profile_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        // Connect to the link's target (Chromium puts it in a short temp
        // dir): the profile path itself can exceed a socket path's limit.
        let link = profile_dir.join("SingletonSocket");
        let target = fs::read_link(&link).unwrap_or(link);
        std::os::unix::net::UnixStream::connect(target).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = profile_dir;
        true
    }
}

pub fn parse_singleton_lock(target: &str) -> Option<u32> {
    let pid: u32 = target.rsplit_once('-')?.1.trim().parse().ok()?;
    // A pid is a positive i32; anything else is garbage, never signalled.
    (1..=i32::MAX as u32).contains(&pid).then_some(pid)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    pub port: u16,
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub launched_at: u64,
    #[serde(default)]
    pub server_pid: u32,
    #[serde(default)]
    pub argv: Vec<String>,
    /// `Chrome/154.0.8037.93` from `/json/version`.
    #[serde(default)]
    pub browser: String,
}

pub fn run_record_path(home: &Path, name: &str) -> PathBuf {
    home.join(RUN_DIR).join(format!("{name}.json"))
}

pub fn read_run_record(home: &Path, name: &str) -> Option<RunRecord> {
    let content = fs::read_to_string(run_record_path(home, name)).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn write_run_record(home: &Path, name: &str, record: &RunRecord) -> io::Result<()> {
    let path = run_record_path(home, name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
    let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn clear_run_record(home: &Path, name: &str) {
    let _ = fs::remove_file(run_record_path(home, name));
}

/// What `ensure_running` does before touching anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachDecision {
    /// herdr launched it and it still answers.
    Attach(RunRecord),
    /// Something else holds the profile (a `SingletonLock` herdr did not
    /// record, whose pid is alive and whose socket answers); never killed.
    InUse { pid: u32 },
    /// Nothing usable: launch.
    Launch,
}

pub fn attach_decision(
    home: &Path,
    name: &str,
    profile_dir: &Path,
    alive: &dyn Fn(u32) -> bool,
    devtools_answers: &dyn Fn(u16) -> bool,
    is_browser: &dyn Fn(u32) -> bool,
) -> AttachDecision {
    if let Some(record) = read_run_record(home, name) {
        if alive(record.pid) && devtools_answers(record.port) {
            return AttachDecision::Attach(record);
        }
        clear_run_record(home, name);
    }
    if let Some(pid) = singleton_lock_holder(profile_dir, alive, is_browser) {
        return AttachDecision::InUse { pid };
    }
    AttachDecision::Launch
}

fn truncate_large_log(path: &Path) {
    if fs::metadata(path)
        .map(|m| m.len() > MAX_PROFILE_LOG_BYTES)
        .unwrap_or(false)
    {
        let _ = fs::write(path, b"");
    }
}

/// Start the process. macOS: LaunchServices (`open`), pid unknown until the
/// `SingletonLock` appears. Elsewhere: a setsid child, pid known.
fn spawn_chromium(
    exe: &Executable,
    argv: &[String],
    log_path: &Path,
) -> Result<Option<u32>, BrowserError> {
    truncate_large_log(log_path);
    #[cfg(target_os = "macos")]
    {
        let Some(bundle) = exe.bundle.as_deref() else {
            return Err(BrowserError::new(
                "browser_executable_missing",
                format!(
                    "{} is not inside a .app bundle; on macOS [browser] executable must point at a Chromium.app",
                    exe.binary.display()
                ),
            ));
        };
        let mut command = std::process::Command::new("/usr/bin/open");
        command
            .arg("-n")
            .arg("-g")
            .arg("-a")
            .arg(bundle)
            .arg("--stdout")
            .arg(log_path)
            .arg("--stderr")
            .arg(log_path)
            .arg("--args")
            .args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        scrub_herdr_env(&mut command);
        let output = command.output().map_err(|err| {
            BrowserError::new(
                "browser_start_failed",
                format!("failed to run /usr/bin/open: {err}"),
            )
        })?;
        if !output.status.success() {
            return Err(BrowserError::new(
                "browser_start_failed",
                format!(
                    "open {} failed: {}",
                    bundle.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        Ok(None)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|err| BrowserError::new("browser_start_failed", err.to_string()))?;
        let mut command = std::process::Command::new(&exe.binary);
        command
            .args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(log.try_clone().map_err(
                |err| BrowserError::new("browser_start_failed", err.to_string()),
            )?))
            .stderr(std::process::Stdio::from(log));
        scrub_herdr_env(&mut command);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command.spawn().map_err(|err| {
            BrowserError::new(
                "browser_start_failed",
                format!("failed to start {}: {err}", exe.binary.display()),
            )
        })?;
        let pid = child.id();
        // Not waited on: it outlives us by design (the setsid child is reparented
        // to init when we exit; while we live it becomes a zombie we never reap,
        // which is harmless for one long-lived process).
        std::mem::forget(child);
        Ok(Some(pid))
    }
}

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub restore: bool,
    pub extra_args: Vec<String>,
    pub first_launch: bool,
    pub timeout: Duration,
    pub server_pid: u32,
    /// The companion extension to load (always, when installed; the attach
    /// reloads restored tabs through it).
    pub extension_dir: Option<PathBuf>,
}

/// Launch Chromium on the profile and wait for its DevTools port; retries
/// with a new port when the first one was grabbed in between (a live
/// browser that never answers is terminated first).
pub fn launch(
    home: &Path,
    name: &str,
    profile_dir: &Path,
    log_path: &Path,
    exe: &Executable,
    options: &LaunchOptions,
) -> Result<RunRecord, BrowserError> {
    let mut last_error = None;
    // A lock left from before this launch is not its browser (attach_decision
    // found it stale: dead, or a reused pid). Read once: a browser an earlier
    // attempt started and could not stop is still this launch's to stop.
    let pre_spawn_lock_pid = singleton_lock_pid(profile_dir);
    for attempt in 1..=LAUNCH_ATTEMPTS {
        let port = pick_port()?;
        let argv = argv(
            profile_dir,
            port,
            options.restore,
            &options.extra_args,
            options.first_launch,
            options.extension_dir.as_deref(),
        );
        let _ = fs::remove_file(profile_dir.join("DevToolsActivePort"));
        let spawned_pid = spawn_chromium(exe, &argv, log_path)?;
        let deadline = Instant::now() + options.timeout;
        match wait_for_devtools(port, deadline) {
            Some(version) => {
                let pid = singleton_lock_pid(profile_dir)
                    .or(spawned_pid)
                    .ok_or_else(|| {
                        BrowserError::new(
                            "browser_start_failed",
                            "Chromium answered on its port but wrote no SingletonLock; cannot record its pid",
                        )
                    })?;
                let record = RunRecord {
                    pid,
                    port,
                    exe: exe.display(),
                    launched_at: super::unix_now(),
                    server_pid: options.server_pid,
                    argv,
                    browser: version
                        .get("Browser")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                };
                write_run_record(home, name, &record)?;
                return Ok(record);
            }
            None => {
                // The port was taken, or the browser did not come up: a live
                // browser this launch started that we cannot reach is ours to stop.
                stop_unreachable_launch(
                    timed_out_launch_pid(
                        pre_spawn_lock_pid,
                        singleton_lock_pid(profile_dir),
                        spawned_pid,
                    ),
                    &crate::platform::process_exists,
                    &terminate,
                    Duration::from_secs(5),
                );
                last_error = Some(BrowserError::new(
                    "browser_launch_timeout",
                    format!(
                        "Chromium did not answer on 127.0.0.1:{port} within {} ms (attempt {attempt} of {LAUNCH_ATTEMPTS}); see {}",
                        options.timeout.as_millis(),
                        log_path.display()
                    ),
                ));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| BrowserError::new("browser_start_failed", "launch failed")))
}

/// The pid a timed-out attempt stops: the `SingletonLock` pid when any
/// attempt of this launch wrote it (it differs from `pre_spawn`, the lock pid
/// read once before the first attempt), else the spawned child. The
/// pre-spawn lock pid is never chosen: it belongs to no browser of this
/// launch, and after a pid reuse it is an unrelated process.
fn timed_out_launch_pid(
    pre_spawn: Option<u32>,
    lock_now: Option<u32>,
    spawned: Option<u32>,
) -> Option<u32> {
    lock_now
        .filter(|pid| Some(*pid) != pre_spawn)
        .or(spawned)
        .filter(|pid| Some(*pid) != pre_spawn)
}

/// SIGTERM `pid` when it is alive and wait up to `grace` for it to go.
fn stop_unreachable_launch(
    pid: Option<u32>,
    exists: &dyn Fn(u32) -> bool,
    stop: &dyn Fn(u32),
    grace: Duration,
) {
    let Some(pid) = pid.filter(|pid| exists(*pid)) else {
        return;
    };
    stop(pid);
    let gone_by = Instant::now() + grace;
    while exists(pid) && Instant::now() < gone_by {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Chromium is not a herdr pane: no `HERDR_*` variable reaches it (the
/// server's own `HERDR_BIN_PATH` included).
pub fn scrub_herdr_env(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("HERDR_") {
            command.env_remove(&key);
        }
    }
}

/// SIGTERM: Chromium treats it as a graceful exit (`exit_type` `SessionEnded`).
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Fork (companion launcher): SIGKILL, the companion stop's last step.
pub fn kill_hard(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-browser-launch-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_bundle(root: &Path, name: &str) -> PathBuf {
        let bundle = root.join(name);
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::write(bundle.join(BUNDLE_BINARY), "").unwrap();
        bundle
    }

    #[test]
    fn executable_resolution_prefers_home_then_system_then_errors() {
        let root = temp("exe");
        let home = root.join("home");
        assert!(
            matches!(
                resolve_executable("auto", Some(&root.join("nohome"))),
                Err(err) if err.code == "browser_executable_missing" || err.code.is_empty()
            ) || resolve_executable("auto", Some(&root.join("nohome"))).is_ok(),
            "auto may find a system Chromium on this machine"
        );
        let bundle = fake_bundle(&home.join("Applications"), "Chromium.app");
        let exe = resolve_executable("auto", Some(&home)).unwrap();
        assert_eq!(exe.bundle.as_deref(), Some(bundle.as_path()));
        assert_eq!(exe.source, "home");
        assert_eq!(exe.display(), bundle.display().to_string());
        // The branded install wins over a plain Chromium.app next to it.
        let branded = fake_bundle(&home.join("Applications"), "herdr+ Browser.app");
        let exe = resolve_executable("auto", Some(&home)).unwrap();
        assert_eq!(exe.bundle.as_deref(), Some(branded.as_path()));
        assert_eq!(exe.source, "home (branded)");
        assert!(exe
            .binary
            .ends_with("herdr+ Browser.app/Contents/MacOS/Chromium"));

        let configured = fake_bundle(&root, "Other.app");
        let exe = resolve_executable(configured.to_str().unwrap(), Some(&home)).unwrap();
        assert_eq!(exe.source, "config");
        assert_eq!(exe.binary, configured.join(BUNDLE_BINARY));
        let inner = configured.join(BUNDLE_BINARY);
        let exe = resolve_executable(inner.to_str().unwrap(), Some(&home)).unwrap();
        assert_eq!(
            exe.bundle.as_deref(),
            Some(configured.as_path()),
            "the bundle is derived from an inner path"
        );
        assert!(matches!(
            resolve_executable("/nope/Chromium.app", Some(&home)),
            Err(err) if err.code == "browser_executable_missing"
        ));
        assert!(matches!(
            resolve_executable("/nope/chromium", Some(&home)),
            Err(err) if err.code == "browser_executable_missing"
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn argv_carries_no_automation_switches_and_respects_flags() {
        let profile = Path::new("/tmp/p");
        let args = argv(
            profile,
            4321,
            true,
            &[
                "--lang=tr".into(),
                "--enable-automation".into(),
                "--headless=new".into(),
                "--no-sandbox".into(),
                "-remote-debugging-pipe".into(),
                "-use-mock-keychain".into(),
                "--user-data-dir=/elsewhere".into(),
                "--load-extension=/evil".into(),
                "--disable-extensions".into(),
            ],
            true,
            Some(Path::new("/tmp/host/companion")),
        );
        assert_eq!(args[0], "--user-data-dir=/tmp/p");
        assert_eq!(args[1], "--remote-debugging-port=4321");
        assert!(
            !args.iter().any(|a| a.contains("AutomationControlled")),
            "the infobar flag is not passed: {args:?}"
        );
        assert!(args.contains(&"--restore-last-session".to_string()));
        assert!(args.contains(&"--lang=tr".to_string()));
        assert_eq!(args.last().unwrap(), "about:blank");
        // herdr's own three (profile dir, port, companion) are the only forbidden switches present
        let own: Vec<&String> = args
            .iter()
            .filter(|a| crate::config::is_forbidden_switch(a))
            .collect();
        assert_eq!(own.len(), 3, "{own:?}");
        assert!(own.iter().all(|a| {
            a.starts_with("--user-data-dir=")
                || a.starts_with("--remote-debugging-port=")
                || a == &"--load-extension=/tmp/host/companion"
        }));
        assert!(!args.contains(&"--disable-extensions".to_string()));
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--")),
            "{args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--user-data-dir="))
                .count(),
            1
        );
        assert!(!args.iter().any(|a| a == "--remote-debugging-port=0"));
        let again = argv(profile, 1, false, &[], false, None);
        assert!(!again.contains(&"--restore-last-session".to_string()));
        assert!(!again.iter().any(|a| a.starts_with("--load-extension")));
        assert_ne!(again.last().unwrap(), "about:blank");
    }

    #[test]
    fn json_version_talks_http_to_a_loopback_listener() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = r#"{"Browser":"Chrome/154.0.8037.93","webSocketDebuggerUrl":"ws://127.0.0.1/devtools/browser/x"}"#;
            let _ = stream.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes(),
            );
            request
        });
        let version = json_version(port, Duration::from_secs(2)).unwrap();
        assert_eq!(version["Browser"], "Chrome/154.0.8037.93");
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /json/version HTTP/1.1"));
        assert!(request.contains(&format!("Host: 127.0.0.1:{port}")));
        assert!(!request.contains("Origin:"));
        assert!(
            json_version(port, Duration::from_millis(300)).is_none(),
            "nothing listens any more"
        );
    }

    #[test]
    fn singleton_lock_and_run_records() {
        let root = temp("lock");
        let profile = root.join("profiles/main");
        fs::create_dir_all(&profile).unwrap();
        assert_eq!(singleton_lock_pid(&profile), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Mac-22871", profile.join("SingletonLock")).unwrap();
            assert_eq!(singleton_lock_pid(&profile), Some(22871));
        }
        assert_eq!(parse_singleton_lock("my-host-name-4242"), Some(4242));
        assert_eq!(parse_singleton_lock("garbage"), None);
        assert_eq!(parse_singleton_lock("host-4294967295"), None, "not a pid");
        assert_eq!(parse_singleton_lock("host-0"), None);

        assert!(read_run_record(&root, "main").is_none());
        let record = RunRecord {
            pid: 7,
            port: 9,
            exe: "x".into(),
            launched_at: 1,
            server_pid: 2,
            argv: vec![],
            browser: "Chrome/1".into(),
        };
        write_run_record(&root, "main", &record).unwrap();
        assert_eq!(read_run_record(&root, "main"), Some(record));
        clear_run_record(&root, "main");
        assert!(read_run_record(&root, "main").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn attach_decision_covers_attach_stale_in_use_and_launch() {
        let root = temp("decision");
        let profile = root.join("profiles/main");
        fs::create_dir_all(&profile).unwrap();
        let me = std::process::id();
        let record = RunRecord {
            pid: me,
            port: 1234,
            exe: String::new(),
            launched_at: 0,
            server_pid: 0,
            argv: vec![],
            browser: String::new(),
        };
        write_run_record(&root, "main", &record).unwrap();
        let alive = |pid: u32| pid == me;
        let not_browser = |_: u32| false;
        assert_eq!(
            attach_decision(
                &root,
                "main",
                &profile,
                &alive,
                &|port| port == 1234,
                &not_browser
            ),
            AttachDecision::Attach(record.clone())
        );
        // dead pid in the record: stale, cleared, launch
        let dead = RunRecord {
            pid: 4_000_000,
            ..record.clone()
        };
        write_run_record(&root, "main", &dead).unwrap();
        assert_eq!(
            attach_decision(&root, "main", &profile, &alive, &|_| true, &not_browser),
            AttachDecision::Launch
        );
        assert!(
            read_run_record(&root, "main").is_none(),
            "stale record cleared"
        );
        // alive pid but the port does not answer: stale too
        write_run_record(&root, "main", &record).unwrap();
        assert_eq!(
            attach_decision(&root, "main", &profile, &alive, &|_| false, &not_browser),
            AttachDecision::Launch
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(format!("host-{me}"), profile.join("SingletonLock"))
                .unwrap();
            // A live pid without a listening SingletonSocket: the browser
            // died and the OS reused its pid, so the lock is stale.
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false, &not_browser),
                AttachDecision::Launch,
                "a reused pid without the socket is not a holder"
            );
            // The same pid read as a Chromium process: a browser whose
            // socket does not answer still holds the profile.
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false, &|pid| pid
                    == me),
                AttachDecision::InUse { pid: me },
                "a live Chromium pid holds the lock without its socket"
            );
            let socket = std::env::temp_dir().join(format!("herdr-sl-{me}.sock"));
            let _ = fs::remove_file(&socket);
            let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std::os::unix::fs::symlink(&socket, profile.join("SingletonSocket")).unwrap();
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false, &not_browser),
                AttachDecision::InUse { pid: me }
            );
            fs::remove_file(profile.join("SingletonLock")).unwrap();
            std::os::unix::fs::symlink("host-4000000", profile.join("SingletonLock")).unwrap();
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false, &not_browser),
                AttachDecision::Launch,
                "a dead SingletonLock pid is ignored"
            );
            let _ = fs::remove_file(&socket);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn browser_process_names_match_chromium_chrome_the_brand_and_the_binary() {
        // No binary resolves: any Chromium or Chrome name, and the brand.
        for name in [
            "Chromium",
            "chromium-browse",
            "chrome",
            "Google Chrome",
            "herdr+ Browser",
        ] {
            assert!(is_browser_process_name(name, None), "{name}");
        }
        for name in ["", "zsh", "node", "Safari"] {
            assert!(!is_browser_process_name(name, None), "{name}");
        }
        let binary = Path::new("/opt/custom/my-browser-build-2026");
        assert!(is_browser_process_name(
            "my-browser-build-2026",
            Some(binary)
        ));
        assert!(
            is_browser_process_name("my-browser-buil", Some(binary)),
            "a comm cut to 15 bytes"
        );
        assert!(
            !is_browser_process_name("my", Some(binary)),
            "too short a prefix"
        );
        assert!(!is_browser_process_name("zsh", Some(binary)));
        assert!(
            is_browser_process_name("herdr+ Browser", Some(binary)),
            "the brand always"
        );
    }

    #[test]
    fn fork_smoke_a_configured_binary_matches_only_its_own_name_not_other_chrome_processes() {
        for binary in [
            Path::new("/Applications/Chromium.app/Contents/MacOS/Chromium"),
            Path::new("/Users/u/Applications/herdr+ Browser.app/Contents/MacOS/Chromium"),
        ] {
            assert!(is_browser_process_name("Chromium", Some(binary)));
            assert!(is_browser_process_name("herdr+ Browser", Some(binary)));
            for name in [
                // A macOS helper's comm, cut to 16 bytes.
                "Google Chrome He",
                "Google Chrome",
                "chrome_crashpad_handler",
                "chromedriver",
                "Chromium Helper",
            ] {
                assert!(
                    !is_browser_process_name(name, Some(binary)),
                    "{name} is not the configured browser"
                );
            }
        }
    }

    #[test]
    fn fork_smoke_a_timed_out_launch_never_stops_the_pre_spawn_lock_pid() {
        // The lock still names the pre-spawn pid (a reused pid): the spawned
        // child, never that pid.
        assert_eq!(timed_out_launch_pid(Some(10), Some(10), Some(20)), Some(20));
        // macOS (pid unknown) and an unchanged lock: nothing.
        assert_eq!(timed_out_launch_pid(Some(10), Some(10), None), None);
        // This launch wrote the lock: its pid.
        assert_eq!(timed_out_launch_pid(Some(10), Some(30), Some(20)), Some(30));
        assert_eq!(timed_out_launch_pid(None, Some(30), None), Some(30));
        assert_eq!(timed_out_launch_pid(None, None, Some(20)), Some(20));
        assert_eq!(timed_out_launch_pid(Some(20), None, Some(20)), None);

        let stopped = std::cell::RefCell::new(Vec::new());
        let alive = std::cell::RefCell::new(vec![10u32, 30]);
        let exists = |pid: u32| alive.borrow().contains(&pid);
        let stop = |pid: u32| {
            stopped.borrow_mut().push(pid);
            alive.borrow_mut().retain(|live| *live != pid);
        };
        stop_unreachable_launch(
            timed_out_launch_pid(Some(10), Some(10), None),
            &exists,
            &stop,
            Duration::from_millis(200),
        );
        assert!(
            stopped.borrow().is_empty(),
            "the pre-spawn pid is not signalled"
        );
        stop_unreachable_launch(
            timed_out_launch_pid(Some(10), Some(30), None),
            &exists,
            &stop,
            Duration::from_millis(200),
        );
        assert_eq!(*stopped.borrow(), vec![30]);
        stop_unreachable_launch(Some(40), &exists, &stop, Duration::from_millis(200));
        assert_eq!(*stopped.borrow(), vec![30], "a dead pid is not signalled");
    }

    #[test]
    fn a_browser_that_survives_attempt_one_is_still_stopped_by_attempt_two() {
        // The stale lock pid 10 is read once, before the first attempt.
        // Attempt 1's browser (30) writes the lock and ignores the SIGTERM;
        // attempt 2's child hands off to it, so the lock still names 30.
        let pre_spawn = Some(10);
        let stopped = std::cell::RefCell::new(Vec::new());
        let exists = |pid: u32| pid == 10 || pid == 30;
        let stop = |pid: u32| stopped.borrow_mut().push(pid);
        for spawned in [Some(31), Some(32)] {
            stop_unreachable_launch(
                timed_out_launch_pid(pre_spawn, Some(30), spawned),
                &exists,
                &stop,
                Duration::from_millis(50),
            );
        }
        assert_eq!(
            *stopped.borrow(),
            vec![30, 30],
            "attempt 2 still stops attempt 1's browser, never the stale pid"
        );
    }
}
