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
        "--disable-blink-features=AutomationControlled".to_string(),
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
    /// Something else holds the profile (a live `SingletonLock` pid herdr did
    /// not record); never killed.
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
) -> AttachDecision {
    if let Some(record) = read_run_record(home, name) {
        if alive(record.pid) && devtools_answers(record.port) {
            return AttachDecision::Attach(record);
        }
        clear_run_record(home, name);
    }
    if let Some(pid) = singleton_lock_pid(profile_dir) {
        if alive(pid) {
            return AttachDecision::InUse { pid };
        }
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
    /// The companion extension to load (`[browser] show_activity`).
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
                // browser on this profile that we cannot reach is ours to stop.
                if let Some(pid) = singleton_lock_pid(profile_dir).or(spawned_pid) {
                    if crate::platform::process_exists(pid) {
                        terminate(pid);
                        let gone_by = Instant::now() + Duration::from_secs(5);
                        while crate::platform::process_exists(pid) && Instant::now() < gone_by {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
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
        assert!(args.contains(&"--disable-blink-features=AutomationControlled".to_string()));
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
        assert_eq!(
            attach_decision(&root, "main", &profile, &alive, &|port| port == 1234),
            AttachDecision::Attach(record.clone())
        );
        // dead pid in the record: stale, cleared, launch
        let dead = RunRecord {
            pid: 4_000_000,
            ..record.clone()
        };
        write_run_record(&root, "main", &dead).unwrap();
        assert_eq!(
            attach_decision(&root, "main", &profile, &alive, &|_| true),
            AttachDecision::Launch
        );
        assert!(
            read_run_record(&root, "main").is_none(),
            "stale record cleared"
        );
        // alive pid but the port does not answer: stale too
        write_run_record(&root, "main", &record).unwrap();
        assert_eq!(
            attach_decision(&root, "main", &profile, &alive, &|_| false),
            AttachDecision::Launch
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(format!("host-{me}"), profile.join("SingletonLock"))
                .unwrap();
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false),
                AttachDecision::InUse { pid: me }
            );
            fs::remove_file(profile.join("SingletonLock")).unwrap();
            std::os::unix::fs::symlink("host-4000000", profile.join("SingletonLock")).unwrap();
            assert_eq!(
                attach_decision(&root, "main", &profile, &alive, &|_| false),
                AttachDecision::Launch,
                "a dead SingletonLock pid is ignored"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }
}
