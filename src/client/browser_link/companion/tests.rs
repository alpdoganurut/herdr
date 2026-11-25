//! The machine's companion against a fake spawner, proxy and sidecar: no
//! Electron, no processes, no signals.

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use super::*;
use crate::api::schema::{BrowserHostCdp, BrowserHostKind};
use crate::browser::companion_app::{ProcessFacts, Spawned};

const KEY: &str = "0123456789abcdef";
const FIRST_PORT: u16 = 41000;

#[derive(Default)]
struct Procs {
    specs: Mutex<Vec<LaunchSpec>>,
    /// `proxy.json` as each spawn found it.
    proxy_seen: Mutex<Vec<Option<ProxyFile>>>,
    alive: Mutex<HashSet<u32>>,
    signals: Mutex<Vec<(u32, StopSignal)>>,
    next: AtomicU32,
    /// SIGTERM ends a process (SIGKILL always does).
    term_ends: AtomicBool,
}

impl Procs {
    fn spawns(&self) -> usize {
        self.specs.lock().unwrap().len()
    }

    fn signals(&self) -> Vec<StopSignal> {
        self.signals
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| *s)
            .collect()
    }

    fn end(&self, pid: u32) {
        self.alive.lock().unwrap().remove(&pid);
    }
}

impl Spawner for Procs {
    fn spawn(&self, spec: &LaunchSpec) -> io::Result<Spawned> {
        let home = spec
            .args
            .iter()
            .position(|a| a == "--remote")
            .and_then(|at| spec.args.get(at + 1))
            .map(PathBuf::from)
            .expect("--remote <home>");
        let seen = fs::read(home.join(PROXY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        self.proxy_seen.lock().unwrap().push(seen);
        self.specs.lock().unwrap().push(spec.clone());
        let pid = 7000 + self.next.fetch_add(1, Ordering::Relaxed);
        self.alive.lock().unwrap().insert(pid);
        Ok(Spawned { pid, child: None })
    }

    fn signal(&self, pid: u32, signal: StopSignal) {
        self.signals.lock().unwrap().push((pid, signal));
        if signal == StopSignal::Kill || self.term_ends.load(Ordering::Relaxed) {
            self.end(pid);
        }
    }

    fn facts(&self, pid: u32) -> Option<ProcessFacts> {
        self.alive
            .lock()
            .unwrap()
            .contains(&pid)
            .then(|| ProcessFacts {
                start_token: Some(1_000_000 + u64::from(pid)),
                started_ms: None,
            })
    }
}

struct Binder {
    taken: Mutex<HashSet<u16>>,
    binds: Mutex<Vec<(u16, String)>>,
    next: AtomicU16,
}

impl ProxyBinder for Binder {
    fn bind(&self, port: u16, secret: &str) -> io::Result<u16> {
        self.binds.lock().unwrap().push((port, secret.to_string()));
        if port == 0 {
            return Ok(self.next.fetch_add(1, Ordering::Relaxed));
        }
        if self.taken.lock().unwrap().contains(&port) {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "taken"));
        }
        Ok(port)
    }
}

struct Link {
    procs: Arc<Procs>,
    attaches: Mutex<Vec<(u16, String)>>,
    quits: AtomicUsize,
    gone: AtomicUsize,
}

impl CompanionLink for Link {
    fn attach(&self, port: u16, nonce: &str) -> Result<Value, BrowserError> {
        self.attaches
            .lock()
            .unwrap()
            .push((port, nonce.to_string()));
        Ok(serde_json::json!({ "tabs": [] }))
    }

    /// The control page's quit ends the process.
    fn quit(&self) {
        self.quits.fetch_add(1, Ordering::Relaxed);
        self.procs.alive.lock().unwrap().clear();
    }

    fn gone(&self) {
        self.gone.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Hooks {
    exits: Mutex<Vec<String>>,
}

impl CompanionHooks for Hooks {
    fn changed(&self) {}

    fn exited(&self, reason: &str) {
        self.exits.lock().unwrap().push(reason.to_string());
    }
}

struct Rig {
    root: PathBuf,
    options: CompanionOptions,
    procs: Arc<Procs>,
    binder: Arc<Binder>,
    link: Arc<Link>,
    hooks: Arc<Hooks>,
}

impl Rig {
    /// An install of app version `app` ("": none).
    fn new(app: &str) -> Self {
        let root = PathBuf::from("/tmp").join(format!(
            "hbc-{}-{}",
            std::process::id(),
            crate::browser::session::random_hex(6)
        ));
        let companion_home = root.join("companion-app");
        let sha = "ab".repeat(32);
        let bundle = root.join("Electron.app");
        let app_dir = companion_home.join("app").join(&sha[..12]);
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("package.json"), "{}").unwrap();
        let executable = companion_install::bundle_executable(&bundle);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "").unwrap();
        if !app.is_empty() {
            let install = serde_json::json!({
                "version": 1, "app": app, "app_sha256": sha, "electron": "45.2.1",
                "bundle": bundle.display().to_string(),
            });
            fs::write(companion_home.join("install.json"), install.to_string()).unwrap();
        }
        let procs = Arc::new(Procs {
            term_ends: AtomicBool::new(true),
            ..Default::default()
        });
        Self {
            options: CompanionOptions {
                companion_home,
                key: KEY.into(),
                label: "buildbox".into(),
                api_socket: root.join(format!("{KEY}.api")),
            },
            binder: Arc::new(Binder {
                taken: Mutex::new(HashSet::new()),
                binds: Mutex::new(Vec::new()),
                next: AtomicU16::new(FIRST_PORT),
            }),
            link: Arc::new(Link {
                procs: Arc::clone(&procs),
                attaches: Mutex::new(Vec::new()),
                quits: AtomicUsize::new(0),
                gone: AtomicUsize::new(0),
            }),
            hooks: Arc::new(Hooks::default()),
            procs,
            root,
        }
    }

    fn companion(&self) -> Arc<Companion> {
        Arc::new(
            Companion::new(
                self.options.clone(),
                self.procs.clone(),
                self.binder.clone(),
                self.link.clone(),
                self.hooks.clone(),
            )
            .unwrap(),
        )
    }

    fn home(&self) -> MachineHome {
        MachineHome::new(&self.options.companion_home, KEY)
    }

    fn proxy_bytes(&self) -> Option<Vec<u8>> {
        fs::read(self.home().proxy_path()).ok()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn params(token: Option<String>) -> BrowserHostRegisterParams {
    BrowserHostRegisterParams {
        kind: BrowserHostKind::Companion,
        instance: "i1".into(),
        pid: None,
        cdp: BrowserHostCdp {
            host: "127.0.0.1".into(),
            port: 9222,
        },
        nonce: "n1".into(),
        version: REMOTE_APP_MIN.into(),
        protocol: 1,
        capabilities: Vec::new(),
        launch_token: token,
        managed: true,
    }
}

/// Launch it as an `ensure` does, and register the process.
fn running(rig: &Rig, companion: &Arc<Companion>) -> (u32, String) {
    let before = rig.procs.spawns();
    let waiting = {
        let companion = Arc::clone(companion);
        std::thread::spawn(move || companion.ensure(Duration::from_secs(10)))
    };
    wait_for("the spawn", || {
        rig.procs.spawns() == before + 1 && companion.info().pid.is_some()
    });
    let token = companion.current().1.expect("a token");
    companion.register(&params(Some(token.clone()))).unwrap();
    assert_eq!(waiting.join().unwrap(), Ok(()));
    let pid = companion.info().pid.expect("a pid");
    (pid, token)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn a_lazy_start_writes_proxy_json_first_and_runs_in_its_own_home_and_profile() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    assert_eq!(rig.procs.spawns(), 0, "nothing starts before it is needed");
    let (pid, token) = running(&rig, &companion);
    let home = rig.home();
    let spec = rig.procs.specs.lock().unwrap()[0].clone();
    assert_eq!(
        spec.args,
        vec![
            "--managed".to_string(),
            "--background".into(),
            "--launch-token".into(),
            token.clone(),
            "--socket".into(),
            rig.options.api_socket.display().to_string(),
            "--profile-dir".into(),
            home.dir().join("profile").display().to_string(),
            "--remote".into(),
            home.dir().display().to_string(),
        ]
    );
    assert_eq!(spec.log, home.dir().join("companion.log"));
    assert_eq!(
        home.dir(),
        rig.options.companion_home.join("machines").join(KEY)
    );
    // the file was there before the process
    let seen = rig.procs.proxy_seen.lock().unwrap()[0]
        .clone()
        .expect("proxy.json at the spawn");
    assert_eq!(seen.port, FIRST_PORT);
    assert_eq!(seen.label.as_deref(), Some("buildbox"));
    assert_eq!(seen.secret.len(), 64);
    assert!(seen.secret.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(
        !spec.args.iter().any(|arg| arg.contains(&seen.secret)),
        "the secret is in no argv"
    );
    assert_eq!(mode(&home.proxy_path()), 0o600);
    assert_eq!(mode(home.dir()), 0o700);
    assert_eq!(
        companion.grant(),
        Some(ProxyGrant {
            port: FIRST_PORT,
            secret: seen.secret.clone()
        })
    );
    let record = companion_app::read_run_record(home.dir()).expect("run.json");
    assert_eq!(
        (record.pid, record.launch_token.as_str()),
        (pid, token.as_str())
    );
    assert_eq!(record.socket, rig.options.api_socket.display().to_string());
    assert_eq!(
        rig.link.attaches.lock().unwrap().clone(),
        vec![(9222, "n1".to_string())]
    );
    assert_eq!(companion.info().state, "running");
    assert!(companion.live());
    // a second need finds it running
    companion.ensure(Duration::from_secs(1)).unwrap();
    assert_eq!(rig.procs.spawns(), 1);
    assert!(companion.heartbeat(&token));
    assert!(!companion.heartbeat("0000"));
}

#[test]
fn five_crashes_in_five_minutes_and_it_gives_up() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    companion.start(StartReason::OnDemand).unwrap();
    wait_for("the first spawn", || companion.info().pid.is_some());
    for n in 1..=5usize {
        let pid = companion.info().pid.expect("a process");
        rig.procs.end(pid);
        companion.tick(Instant::now());
        wait_for("the crash", || companion.info().pid.is_none());
        let info = companion.info();
        assert_eq!(info.restarts as usize, n);
        if n == 5 {
            assert_eq!(info.state, "failed");
            break;
        }
        assert_eq!(info.state, "crashed");
        let delay = companion_app::BACKOFF_SECS[n - 1];
        companion.tick(Instant::now() + Duration::from_secs(delay) + Duration::from_millis(50));
        wait_for("the restart", || {
            rig.procs.spawns() == n + 1 && companion.info().pid.is_some()
        });
    }
    companion.tick(Instant::now() + Duration::from_secs(3600));
    assert_eq!(rig.procs.spawns(), 5, "no restart after the fifth crash");
    let err = companion.ensure(Duration::from_secs(1)).unwrap_err();
    let EnsureError::Failed(why) = &err else {
        panic!("{err:?}")
    };
    assert!(why.contains("crashed 5 times"), "{why}");
    assert_eq!(
        err.link_error("mbp").message,
        format!("the browser on mbp did not start ({why}); nothing was sent")
    );
    assert_eq!(
        rig.procs.spawns(),
        5,
        "an ensure does not relaunch a failed one"
    );
    // the user's start resets it, and shows its window
    companion.start(StartReason::User).unwrap();
    wait_for("the user's start", || rig.procs.spawns() == 6);
    assert!(!rig.procs.specs.lock().unwrap()[5]
        .args
        .contains(&"--background".to_string()));
    assert_eq!(rig.hooks.exits.lock().unwrap().len(), 5);
    assert_eq!(rig.link.gone.load(Ordering::Relaxed), 5);
}

#[test]
fn exit_2_is_no_crash_to_retry() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    companion.start(StartReason::OnDemand).unwrap();
    wait_for("the spawn", || companion.info().pid.is_some());
    let (generation, _) = companion.current();
    companion.process_exited(
        generation,
        ExitInfo {
            code: Some(BAD_FLAGS_EXIT),
            signal: None,
            known: true,
        },
    );
    let info = companion.info();
    assert_eq!(info.state, "failed");
    assert!(info.last_error.unwrap().contains("(exit 2)"));
    let err = companion.ensure(Duration::from_secs(1)).unwrap_err();
    assert!(
        matches!(&err, EnsureError::Failed(why) if why.contains("(exit 2)") && !why.contains("5 times")),
        "{err:?}"
    );
    companion.tick(Instant::now() + Duration::from_secs(3600));
    assert_eq!(rig.procs.spawns(), 1);
    assert!(companion_app::read_run_record(rig.home().dir()).is_none());
}

#[test]
fn a_restarted_helper_adopts_the_companion_with_the_same_port_and_secret() {
    let rig = Rig::new("0.2.13");
    let first = rig.companion();
    let (pid, token) = running(&rig, &first);
    let grant = first.grant().unwrap();
    let file = rig.proxy_bytes().unwrap();
    drop(first); // the helper went away; its companion lives on
    let second = rig.companion();
    assert_eq!(second.recover(), Recovered::Adopted { pid });
    assert_eq!(
        rig.binder.binds.lock().unwrap().last().cloned(),
        Some((grant.port, grant.secret.clone())),
        "the proxy listens with the port and secret the companion has"
    );
    assert_eq!(second.grant(), Some(grant));
    assert_eq!(
        rig.proxy_bytes().unwrap(),
        file,
        "proxy.json is not rewritten"
    );
    assert_eq!(rig.procs.spawns(), 1, "no new process");
    assert_eq!(second.info().state, "reconnecting");
    // it registers again with its launch token
    second.register(&params(Some(token))).unwrap();
    assert_eq!(second.info().state, "running");
    assert!(second.live());
    second.ensure(Duration::from_secs(1)).unwrap();
    assert_eq!(rig.procs.spawns(), 1);
    assert!(rig.procs.signals().is_empty());
    // the adopted process is watched by its record
    rig.procs.end(pid);
    second.tick(Instant::now());
    wait_for("the exit", || second.info().pid.is_none());
}

#[test]
fn a_port_that_cannot_come_back_relaunches_with_a_new_port_and_secret() {
    let rig = Rig::new("0.2.13");
    let first = rig.companion();
    let (pid, _) = running(&rig, &first);
    let old = first.grant().unwrap();
    drop(first);
    rig.binder.taken.lock().unwrap().insert(old.port);
    let second = rig.companion();
    assert_eq!(second.recover(), Recovered::Relaunched);
    assert_eq!(
        rig.procs.signals.lock().unwrap()[0],
        (pid, StopSignal::Term)
    );
    wait_for("the new process", || {
        companion_app::read_run_record(rig.home().dir()).is_some_and(|r| r.pid != pid)
    });
    let seen = rig.procs.proxy_seen.lock().unwrap()[1].clone().unwrap();
    assert_ne!(seen.port, old.port);
    assert_ne!(seen.secret, old.secret);
    assert_eq!(second.grant().unwrap().port, seen.port);
}

#[test]
fn another_socket_in_the_record_relaunches_too() {
    let rig = Rig::new("0.2.13");
    let first = rig.companion();
    let (pid, _) = running(&rig, &first);
    drop(first);
    let mut record = companion_app::read_run_record(rig.home().dir()).unwrap();
    record.socket = "/tmp/elsewhere.api".into();
    companion_app::write_run_record(rig.home().dir(), &record).unwrap();
    let second = rig.companion();
    assert_eq!(second.recover(), Recovered::Relaunched);
    assert!(!rig.procs.alive.lock().unwrap().contains(&pid));
    wait_for("the new process", || rig.procs.spawns() == 2);
}

#[test]
fn a_record_of_a_gone_process_is_cleared() {
    let rig = Rig::new("0.2.13");
    let first = rig.companion();
    let (pid, _) = running(&rig, &first);
    drop(first);
    rig.procs.end(pid);
    let second = rig.companion();
    assert_eq!(second.recover(), Recovered::Nothing);
    assert!(companion_app::read_run_record(rig.home().dir()).is_none());
    assert_eq!(second.info().state, "stopped");
    assert!(rig.procs.signals().is_empty());
}

#[test]
fn quit_goes_through_the_control_page_and_leaves_nothing() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    running(&rig, &companion);
    companion.shutdown();
    assert_eq!(rig.link.quits.load(Ordering::Relaxed), 1);
    assert!(
        rig.procs.signals().is_empty(),
        "no signal when the quit works"
    );
    assert!(companion_app::read_run_record(rig.home().dir()).is_none());
    assert!(rig.proxy_bytes().is_none());
    assert_eq!(companion.info().state, "stopped");
    assert_eq!(companion.info().restarts, 0, "a stop is no crash");
    assert!(
        companion.ensure(Duration::from_secs(1)).is_err(),
        "nothing starts after the quit"
    );
    assert_eq!(rig.procs.spawns(), 1);
}

#[test]
fn a_stop_without_a_registration_signals_and_a_later_ensure_starts_again() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    companion.start(StartReason::OnDemand).unwrap();
    wait_for("the spawn", || companion.info().pid.is_some());
    companion.stop();
    assert_eq!(rig.link.quits.load(Ordering::Relaxed), 0);
    assert_eq!(rig.procs.signals(), vec![StopSignal::Term]);
    assert_eq!(companion.info().state, "stopped");
    assert_eq!(companion.info().restarts, 0);
    running(&rig, &companion);
    assert_eq!(rig.procs.spawns(), 2);
}

#[test]
fn only_the_launched_process_registers() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    let stranger = Some("ab".repeat(16));
    assert_eq!(
        companion
            .register(&params(stranger.clone()))
            .unwrap_err()
            .code,
        "host_in_use"
    );
    companion.start(StartReason::OnDemand).unwrap();
    wait_for("the spawn", || companion.info().pid.is_some());
    assert_eq!(
        companion.register(&params(None)).unwrap_err().code,
        "host_in_use"
    );
    assert_eq!(
        companion.register(&params(stranger)).unwrap_err().code,
        "host_in_use"
    );
    assert!(
        rig.link.attaches.lock().unwrap().is_empty(),
        "no attach for a stranger"
    );
    assert!(!companion.live());
}

#[test]
fn an_app_without_remote_mode_or_no_install_is_never_launched() {
    let old = Rig::new("0.2.12");
    let err = old.companion().ensure(Duration::from_secs(1)).unwrap_err();
    assert!(
        matches!(&err, EnsureError::Failed(why) if why.contains("0.2.12") && why.contains(REMOTE_APP_MIN)),
        "{err:?}"
    );
    assert_eq!(old.procs.spawns(), 0);
    let none = Rig::new("");
    let companion = none.companion();
    let err = companion.ensure(Duration::from_secs(1)).unwrap_err();
    assert_eq!(err, EnsureError::NotInstalled);
    assert_eq!(
        err.link_error("mbp"),
        LinkError::new(
            "host_unavailable",
            "the browser is not installed on mbp; ask the user to run \"herdr browser setup --companion\" there"
        )
    );
    assert_eq!(companion.info().state, "not_installed");
    assert_eq!(none.procs.spawns(), 0);
    assert!(app_supports_remote("0.2.13") && app_supports_remote("0.3.0"));
    assert!(app_supports_remote("v1.0.0"));
    assert!(!app_supports_remote("0.2.9") && !app_supports_remote(""));
    assert!(!app_supports_remote("dev"));
}

#[test]
fn forget_removes_this_profile_and_nothing_else() {
    let rig = Rig::new("0.2.13");
    let companion = rig.companion();
    running(&rig, &companion);
    let home = rig.home();
    fs::create_dir_all(home.profile_dir().join("Default")).unwrap();
    fs::write(home.profile_dir().join("Default").join("Cookies"), "x").unwrap();
    let other = MachineHome::new(&rig.options.companion_home, "fedcba9876543210");
    other.create().unwrap();
    fs::create_dir_all(other.profile_dir()).unwrap();
    fs::write(other.profile_dir().join("Cookies"), "y").unwrap();
    companion.forget().unwrap();
    assert_eq!(rig.link.quits.load(Ordering::Relaxed), 1);
    assert!(!home.profile_dir().exists());
    let left: Vec<_> = fs::read_dir(home.dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("profile"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(fs::read(other.profile_dir().join("Cookies")).unwrap(), b"y");
    // the next need starts a signed-out one
    running(&rig, &companion);
    assert_eq!(rig.procs.spawns(), 2);
    assert_eq!(
        rig.binder.binds.lock().unwrap().len(),
        1,
        "the proxy keeps its port and secret"
    );
}

#[test]
fn proxy_json_is_what_the_companion_accepts() {
    let rig = Rig::new("0.2.13");
    let home = rig.home();
    home.create().unwrap();
    let file = ProxyFile {
        port: 1234,
        secret: new_secret().unwrap(),
        label: None,
    };
    write_proxy_file(&home, &file).unwrap();
    assert_eq!(read_proxy_file(&home), Some(file.clone()));
    let text = fs::read_to_string(home.proxy_path()).unwrap();
    assert!(!text.contains("label"), "{text}");
    for bad in [
        r#"{"port":0,"secret":"abc"}"#,
        r#"{"port":1234,"secret":""}"#,
        r#"{"port":1234,"secret":"a b"}"#,
        r#"{"port":70000,"secret":"abc"}"#,
    ] {
        fs::write(home.proxy_path(), bad).unwrap();
        assert_eq!(read_proxy_file(&home), None, "{bad}");
    }
    assert_ne!(new_secret().unwrap(), file.secret);
}

/// The real companion (`RB_C5_COMPANION_APP`: an `apps/companion` with
/// `--remote`, Electron in its node_modules) and the real sidecar
/// (`RB_C5_SIDECAR_ASSETS`: a `host.mjs` with `remote`), hidden
/// (`HERDR_COMPANION_HEADLESS=1`), in a fresh temp home, against a stand-in
/// for the helper's API socket and the real proxy. Run the test binary
/// itself under a hard timer, with a temp HOME.
mod e2e {
    use std::collections::HashMap;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::Command;

    use super::*;
    use crate::browser::companion_app::ProcessSpawner;
    use crate::client::browser_link::forward::{Dial, DialError, RemoteConnection};
    use crate::client::browser_link::proxy::{Classifier, Proxy, ProxyOptions, Route};
    use crate::client::browser_link::sidecar::{
        NodeSidecar, Sidecar, SidecarHooks, SidecarOptions,
    };
    use crate::client::browser_link::ssh::Destination;

    struct Nowhere;

    impl Classifier for Nowhere {
        fn canonical(&self, host: &str) -> Result<String, String> {
            Ok(host.to_ascii_lowercase())
        }

        fn route(&self, _host: &str, _port: u16) -> Route {
            Route::Refuse("the test dials nothing".into())
        }
    }

    impl Dial for Nowhere {
        fn dial(&self, _to: &Destination) -> Result<RemoteConnection, DialError> {
            Err(DialError::Down)
        }
    }

    /// The helper's proxy: the real one, refusing every page.
    #[derive(Default)]
    struct RealProxy(Mutex<Option<Proxy>>);

    impl ProxyBinder for RealProxy {
        fn bind(&self, port: u16, secret: &str) -> io::Result<u16> {
            let proxy = Proxy::start(
                port,
                ProxyOptions {
                    label: "buildbox".into(),
                    instance: crate::browser::session::random_hex(16),
                    secret: secret.to_string(),
                    remote_only: true,
                    classifier: Arc::new(Nowhere),
                    dial: Arc::new(Nowhere),
                },
            )?;
            let port = proxy.port();
            *self.0.lock().unwrap() = Some(proxy);
            Ok(port)
        }
    }

    #[derive(Default)]
    struct Quiet;

    impl CompanionHooks for Quiet {
        fn changed(&self) {}
        fn exited(&self, _reason: &str) {}
    }

    impl SidecarHooks for Quiet {
        fn event(&self, _event: Value) {}
        fn attached(&self, _answer: &Value) {}
        fn lost(&self, _reason: &str) {}
    }

    /// The helper's API socket as the companion uses it: one request per
    /// connection, the events stream kept open.
    struct Api {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        accept: Option<std::thread::JoinHandle<()>>,
    }

    impl Api {
        fn start(path: &Path, companion: &Arc<Companion>) -> Self {
            let _ = fs::remove_file(path);
            let listener = UnixListener::bind(path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let leases: Arc<Mutex<HashMap<String, String>>> = Arc::default();
            let (flag, companion) = (Arc::clone(&stop), Arc::clone(companion));
            let accept = std::thread::spawn(move || {
                let mut streams = Vec::new();
                while !flag.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            streams.push(stream.try_clone().unwrap());
                            let (companion, leases) = (Arc::clone(&companion), Arc::clone(&leases));
                            std::thread::spawn(move || serve(stream, &companion, &leases));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(20)),
                    }
                }
                for stream in streams {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            });
            Self {
                path: path.to_owned(),
                stop,
                accept: Some(accept),
            }
        }
    }

    impl Drop for Api {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(accept) = self.accept.take() {
                let _ = accept.join();
            }
            let _ = fs::remove_file(&self.path);
        }
    }

    fn lease(id: &str) -> Value {
        serde_json::json!({
            "type": "browser_host_lease",
            "lease": { "lease_id": id, "profile": "companion", "state": "attached", "heartbeat_ms": 1000,
                       "ttl_ms": 15000, "expires_unix": unix_now() + 15, "host_protocol": 1, "server_features": [] }
        })
    }

    fn serve(stream: UnixStream, companion: &Companion, leases: &Mutex<HashMap<String, String>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            return;
        };
        let id = request["id"].clone();
        let params = request["params"].clone();
        let token_of = |params: &Value| {
            params["lease_id"]
                .as_str()
                .and_then(|lease| leases.lock().unwrap().get(lease).cloned())
        };
        let unknown = || Err(("lease_unknown".to_string(), "no such lease".to_string()));
        let answer: Result<Value, (String, String)> = match request["method"].as_str().unwrap_or("")
        {
            "browser.host_register" => {
                match serde_json::from_value::<BrowserHostRegisterParams>(params) {
                    Ok(params) => match companion.register(&params) {
                        Ok(_) => {
                            let lease_id = format!("L{}", crate::browser::session::random_hex(8));
                            leases
                                .lock()
                                .unwrap()
                                .insert(lease_id.clone(), params.launch_token.unwrap_or_default());
                            Ok(lease(&lease_id))
                        }
                        Err(err) => Err((err.code, err.message)),
                    },
                    Err(err) => Err(("invalid_request".into(), err.to_string())),
                }
            }
            "browser.host_heartbeat" => match token_of(&params) {
                Some(token) if companion.heartbeat(&token) => {
                    Ok(lease(params["lease_id"].as_str().unwrap_or("")))
                }
                _ => unknown(),
            },
            "browser.host_unregister" => match token_of(&params) {
                Some(token) => {
                    companion.unregistered(&token);
                    Ok(serde_json::json!({ "type": "ok" }))
                }
                None => unknown(),
            },
            "browser.host_state" => Ok(
                serde_json::json!({ "type": "browser_host_state", "state": { "focused_tab_id": "w1:t1", "tabs": [{ "tab_id": "w1:t1", "workspace_id": "w1", "label": "agents" }] } }),
            ),
            "events.subscribe" => {
                let started =
                    serde_json::json!({ "id": id, "result": { "type": "subscription_started" } });
                let mut out = &stream;
                if writeln!(out, "{started}").is_err() {
                    return;
                }
                // the stream stays open until either side ends
                let mut rest = String::new();
                let _ = reader.read_line(&mut rest);
                return;
            }
            other => Err((
                "unknown_method".into(),
                format!("{other} is not served here"),
            )),
        };
        let line = match answer {
            Ok(result) => serde_json::json!({ "id": id, "result": result }),
            Err((code, message)) => {
                serde_json::json!({ "id": id, "error": { "code": code, "message": message } })
            }
        };
        let mut out = &stream;
        let _ = writeln!(out, "{line}");
    }

    fn copy(from: &Path, to: &Path) {
        let status = Command::new("cp")
            .arg("-Rc")
            .arg(from)
            .arg(to)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "cp -Rc {} {}",
            from.display(),
            to.display()
        );
    }

    /// Everything a run started ends with it, also on a failed assert.
    struct Run {
        companion: Option<Arc<Companion>>,
        sidecar: Option<Arc<Sidecar>>,
        api: Option<Api>,
        pids: Vec<u32>,
    }

    impl Drop for Run {
        fn drop(&mut self) {
            if let Some(companion) = self.companion.take() {
                companion.shutdown();
            }
            if let Some(sidecar) = self.sidecar.take() {
                sidecar.shutdown();
            }
            drop(self.api.take());
        }
    }

    fn sidecar(node: &Path, host: &Path) -> Arc<Sidecar> {
        Arc::new(Sidecar::new(
            Arc::new(NodeSidecar::new(node.to_owned(), host.to_owned())),
            Arc::new(Quiet),
            SidecarOptions::default(),
        ))
    }

    fn companion(
        rig_options: &CompanionOptions,
        proxy: &Arc<RealProxy>,
        sidecar: &Arc<Sidecar>,
    ) -> Arc<Companion> {
        Arc::new(
            Companion::new(
                rig_options.clone(),
                Arc::new(ProcessSpawner),
                proxy.clone(),
                sidecar.clone(),
                Arc::new(Quiet),
            )
            .unwrap(),
        )
    }

    #[test]
    #[ignore = "starts a hidden Electron companion and node; needs RB_C5_COMPANION_APP and RB_C5_SIDECAR_ASSETS"]
    fn e2e_the_real_companion_starts_attaches_remote_is_adopted_and_quits() {
        let app = PathBuf::from(std::env::var("RB_C5_COMPANION_APP").expect("RB_C5_COMPANION_APP"));
        let assets =
            PathBuf::from(std::env::var("RB_C5_SIDECAR_ASSETS").expect("RB_C5_SIDECAR_ASSETS"));
        assert_eq!(
            std::env::var("HERDR_COMPANION_HEADLESS").as_deref(),
            Ok("1"),
            "the companion runs hidden only"
        );
        let node = crate::browser::node::discover_default(None, None)
            .expect("node")
            .path;
        let root =
            PathBuf::from("/tmp").join(format!("hbe-{}", crate::browser::session::random_hex(6)));
        fs::create_dir_all(&root).unwrap();
        struct Clean(PathBuf);
        impl Drop for Clean {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _clean = Clean(root.clone());
        // the install: this app's files (copied) and its Electron (read in place)
        let companion_home = root.join("companion-app");
        let sha = "c5".repeat(32);
        let app_dir = companion_home.join("app").join(&sha[..12]);
        fs::create_dir_all(&app_dir).unwrap();
        for part in ["package.json", "src", "assets"] {
            copy(&app.join(part), &app_dir.join(part));
        }
        let version: Value =
            serde_json::from_slice(&fs::read(app.join("package.json")).unwrap()).unwrap();
        let bundle = app.join("node_modules/electron/dist/Electron.app");
        let install = serde_json::json!({
            "version": 1, "app": version["version"], "app_sha256": sha, "electron": "45.2.1",
            "bundle": bundle.display().to_string(),
        });
        fs::write(companion_home.join("install.json"), install.to_string()).unwrap();
        // the sidecar: its scripts and playwright-core, copied
        let host = root.join("host");
        copy(&assets, &host);
        fs::create_dir_all(host.join("node_modules")).unwrap();
        copy(
            &app.join("node_modules/playwright-core"),
            &host.join("node_modules/playwright-core"),
        );
        let options = CompanionOptions {
            companion_home: companion_home.clone(),
            key: KEY.into(),
            label: "buildbox".into(),
            api_socket: root.join("k.api"),
        };
        let mut run = Run {
            companion: None,
            sidecar: None,
            api: None,
            pids: Vec::new(),
        };

        // a helper starts it for the first op
        let proxy = Arc::new(RealProxy::default());
        let first_sidecar = sidecar(&node, &host);
        let first = companion(&options, &proxy, &first_sidecar);
        run.sidecar = Some(Arc::clone(&first_sidecar));
        run.companion = Some(Arc::clone(&first));
        run.api = Some(Api::start(&options.api_socket, &first));
        assert_eq!(first.recover(), Recovered::Nothing);
        let started = Instant::now();
        first.ensure(ENSURE_WAIT).unwrap_or_else(|err| {
            let log = fs::read_to_string(first.home().log_path()).unwrap_or_default();
            panic!("{err:?}\n{log}")
        });
        let pid = first.info().pid.expect("a pid");
        run.pids.push(pid);
        eprintln!("registered in {} ms", started.elapsed().as_millis());
        let grant = first.grant().unwrap();
        let file = read_proxy_file(first.home()).expect("proxy.json");
        assert_eq!((file.port, &file.secret), (grant.port, &grant.secret));
        assert_eq!(file.label.as_deref(), Some("buildbox"));
        assert!(first_sidecar.attached());
        run.pids.extend(first_sidecar.pid());
        // remote mode: no path of this Mac is shown, pictures come back in the answer
        let show = first_sidecar
            .request(
                "show",
                None,
                serde_json::json!({ "path": "/etc/hosts" }),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(show.error.map(|e| e.code).as_deref(), Some("host_refused"));
        let opened = first_sidecar
            .request(
                "open",
                None,
                serde_json::json!({ "url": "about:blank", "tab_id": "w1:t1" }),
                Duration::from_secs(20),
            )
            .unwrap();
        assert!(opened.ok, "{opened:?}");
        let target = opened.result["target"]
            .as_str()
            .expect("a target")
            .to_string();
        let shot = first_sidecar
            .request(
                "screenshot",
                Some(&target),
                serde_json::json!({}),
                Duration::from_secs(20),
            )
            .unwrap();
        assert!(shot.ok, "{shot:?}");
        assert!(
            shot.result["full"]
                .as_str()
                .is_some_and(|png| png.len() > 100),
            "the picture is in the answer"
        );

        // the helper goes away; the companion lives on
        drop(run.api.take());
        run.companion = None;
        drop(first);
        first_sidecar.shutdown();
        run.sidecar = None;
        drop(first_sidecar);
        drop(proxy);

        // a new helper adopts it with the same port and secret
        let proxy = Arc::new(RealProxy::default());
        let second_sidecar = sidecar(&node, &host);
        let second = companion(&options, &proxy, &second_sidecar);
        run.sidecar = Some(Arc::clone(&second_sidecar));
        run.companion = Some(Arc::clone(&second));
        run.api = Some(Api::start(&options.api_socket, &second));
        assert_eq!(second.recover(), Recovered::Adopted { pid });
        assert_eq!(second.grant(), Some(grant));
        let until = Instant::now() + Duration::from_secs(30);
        while !second.live() {
            assert!(
                Instant::now() < until,
                "the adopted companion did not register again"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        run.pids.extend(second_sidecar.pid());
        let shot = second_sidecar
            .request(
                "screenshot",
                Some(&target),
                serde_json::json!({}),
                Duration::from_secs(20),
            )
            .unwrap();
        assert!(
            shot.ok,
            "the same page after the helper's restart: {shot:?}"
        );

        // the quit: the control page, and nothing is left
        second.shutdown();
        assert_eq!(second.info().pid, None);
        assert!(ProcessSpawner.facts(pid).is_none(), "the companion exited");
        assert!(read_proxy_file(second.home()).is_none());
        second_sidecar.shutdown();
        for pid in &run.pids {
            assert!(ProcessSpawner.facts(*pid).is_none(), "pid {pid} still runs");
        }
    }
}
