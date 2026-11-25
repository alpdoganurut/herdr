//! The client's side of the browser link: a herdr+ client that shows a
//! machine starts its helper (`herdr browser-link <machine>`) unless one
//! runs, and talks to it on `<key>.link` (contract 2.7). Nothing here
//! blocks the caller: a thread of its own connects, says hello, sends what
//! the client set and keeps what the helper said for the next look.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::clients::{encode, ClientMessage, HelperMessage, BYE_PROTO, CLIENT_PROTO};
/// What the client shell names (C8).
pub(crate) use super::clients::{CompanionAction, LinkView, MachineFacts};
use crate::api::schema::BrowserPlacementSpec;
use crate::server::headless::placement_action::PlacementActionPayload;
use crate::server::headless::view_cycle::ViewCyclePayload;

/// The text while the helper runs another herdr+ version.
pub(crate) const OTHER_VERSION: &str =
    "the browser link runs another herdr+ version; it ends 30 s after the last older client quits";

/// How a client reaches its helper.
#[derive(Clone)]
pub(crate) struct LinkClientConfig {
    pub(crate) machine: MachineFacts,
    pub(crate) cycle_keys: Vec<String>,
    /// `<socket dir>/<key>.link`.
    pub(crate) socket: PathBuf,
    /// Starts the helper ([`spawn_helper`]).
    pub(crate) spawn: Arc<dyn Fn() -> io::Result<()> + Send + Sync>,
    /// Between attempts to connect.
    pub(crate) retry: Duration,
    /// After `bye{proto}`.
    pub(crate) proto_retry: Duration,
    /// A started helper has this long to listen.
    pub(crate) start_wait: Duration,
}

impl LinkClientConfig {
    /// A saved machine's: its helper's socket in the private ssh directory,
    /// started as `herdr browser-link <id>` with its log in the machine's
    /// home.
    pub(crate) fn for_machine(machine: MachineFacts, cycle_keys: Vec<String>) -> io::Result<Self> {
        let key = super::ssh::machine_key(
            &crate::config::state_dir(),
            &machine.target,
            &machine.session,
        );
        let socket = super::ssh::socket_dir()?.join(format!("{key}.link"));
        let log = super::helper::machine_home(&key)?.join("helper.log");
        let id = machine.id.clone();
        let spawn = Arc::new(move || spawn_helper(&id, &log));
        Ok(Self::new(machine, cycle_keys, socket, spawn))
    }

    pub(crate) fn new(
        machine: MachineFacts,
        cycle_keys: Vec<String>,
        socket: PathBuf,
        spawn: Arc<dyn Fn() -> io::Result<()> + Send + Sync>,
    ) -> Self {
        Self {
            machine,
            cycle_keys,
            socket,
            spawn,
            retry: Duration::from_millis(500),
            proto_retry: Duration::from_secs(30),
            start_wait: Duration::from_secs(5),
        }
    }
}

/// What the helper said, for the client's next look.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct HelperNews {
    pub(crate) connected: bool,
    pub(crate) view: Option<LinkView>,
    /// Why the helper cannot be used now.
    pub(crate) notice: Option<String>,
    pub(crate) actions: Vec<PlacementActionPayload>,
    pub(crate) cycles: Vec<ViewCyclePayload>,
}

#[derive(Default)]
struct Shared {
    news: Mutex<HelperNews>,
    /// The last placement, sent again to a new helper.
    placement: Mutex<Option<ClientMessage>>,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A client's link to its machine's helper; dropping it disconnects (the
/// helper ends 30 s after its last client).
pub(crate) struct LinkClient {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<ClientMessage>,
    thread: Option<JoinHandle<()>>,
}

impl LinkClient {
    pub(crate) fn start(config: LinkClientConfig) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let (outbox, inbox) = mpsc::channel();
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("browser-link-client".into())
                .spawn(move || run(&config, &shared, &inbox))?
        };
        Ok(Self {
            shared,
            outbox,
            thread: Some(thread),
        })
    }

    /// Where this client shows the browser; `active`: its surface shows the
    /// machine; `focused`: its terminal has the user's focus. Sent only
    /// when it changed.
    pub(crate) fn set_placement(
        &self,
        spec: Option<BrowserPlacementSpec>,
        active: bool,
        focused: bool,
    ) {
        let message = ClientMessage::Placement {
            spec,
            active,
            focused,
        };
        let mut last = lock(&self.shared.placement);
        if last.as_ref() == Some(&message) {
            return;
        }
        *last = Some(message.clone());
        drop(last);
        let _ = self.outbox.send(message);
    }

    /// The user's start, stop, restart, focus; `Forget` only after the user
    /// confirmed it.
    pub(crate) fn companion(&self, action: CompanionAction) {
        let _ = self.outbox.send(ClientMessage::Companion { action });
    }

    /// How the link is now, without the asks.
    pub(crate) fn news(&self) -> HelperNews {
        let news = lock(&self.shared.news);
        HelperNews {
            actions: Vec::new(),
            cycles: Vec::new(),
            ..news.clone()
        }
    }

    /// The companion's asks since the last call, oldest first.
    pub(crate) fn take_asks(&self) -> (Vec<PlacementActionPayload>, Vec<ViewCyclePayload>) {
        let mut news = lock(&self.shared.news);
        (
            std::mem::take(&mut news.actions),
            std::mem::take(&mut news.cycles),
        )
    }
}

impl Drop for LinkClient {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn stopped(shared: &Shared) -> bool {
    shared.stop.load(Ordering::Acquire)
}

/// Waits `wait` unless the client goes first.
fn pause(shared: &Shared, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !stopped(shared) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Connects, starting the helper when nothing listens.
fn connect(config: &LinkClientConfig, shared: &Shared) -> Option<UnixStream> {
    if let Ok(stream) = UnixStream::connect(&config.socket) {
        return Some(stream);
    }
    if let Err(err) = (config.spawn)() {
        tracing::warn!(%err, "the browser link's helper did not start");
        lock(&shared.news).notice = Some(format!("the browser link did not start: {err}"));
        return None;
    }
    let deadline = Instant::now() + config.start_wait;
    while !stopped(shared) && Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(&config.socket) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn send(stream: &mut UnixStream, message: &ClientMessage) -> io::Result<()> {
    stream.write_all(encode(message)?.as_bytes())
}

fn run(config: &LinkClientConfig, shared: &Arc<Shared>, inbox: &mpsc::Receiver<ClientMessage>) {
    while !stopped(shared) {
        let Some(stream) = connect(config, shared) else {
            pause(shared, config.retry);
            continue;
        };
        let wait = match session(config, shared, inbox, stream) {
            Some(BYE_PROTO_CODE) => config.proto_retry,
            _ => config.retry,
        };
        lock(&shared.news).connected = false;
        pause(shared, wait);
    }
}

const BYE_PROTO_CODE: &str = BYE_PROTO;

/// One connection; the helper's `bye` code when it said one.
fn session(
    config: &LinkClientConfig,
    shared: &Arc<Shared>,
    inbox: &mpsc::Receiver<ClientMessage>,
    mut stream: UnixStream,
) -> Option<&'static str> {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let hello = ClientMessage::Hello {
        proto: CLIENT_PROTO,
        herdr: env!("CARGO_PKG_VERSION").into(),
        machine: config.machine.clone(),
        cycle_keys: config.cycle_keys.clone(),
    };
    if send(&mut stream, &hello).is_err() {
        return None;
    }
    let placement = lock(&shared.placement).clone();
    if let Some(placement) = placement {
        if send(&mut stream, &placement).is_err() {
            return None;
        }
    }
    let Ok(read_half) = stream.try_clone() else {
        return None;
    };
    let (said_bye, heard_bye) = mpsc::channel();
    let reader = {
        let shared = Arc::clone(shared);
        std::thread::Builder::new()
            .name("browser-link-client-read".into())
            .spawn(move || read_helper(read_half, &shared, &said_bye))
    };
    let Ok(reader) = reader else {
        return None;
    };
    lock(&shared.news).connected = true;
    let mut bye = None;
    loop {
        if stopped(shared) {
            break;
        }
        match heard_bye.try_recv() {
            Ok(code) => {
                bye = code;
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match inbox.recv_timeout(Duration::from_millis(50)) {
            Ok(message) => {
                if send(&mut stream, &message).is_err() {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = reader.join();
    if bye.is_none() {
        bye = heard_bye.try_recv().ok().flatten();
    }
    bye
}

/// Reads the helper until it ends; says its `bye` code (`None`: the
/// connection just ended).
fn read_helper(stream: UnixStream, shared: &Shared, bye: &mpsc::Sender<Option<&'static str>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = bye.send(None);
                return;
            }
            Ok(_) => {}
        }
        let Ok(message) = serde_json::from_str::<HelperMessage>(&line) else {
            continue;
        };
        let mut news = lock(&shared.news);
        match message {
            HelperMessage::State(view) => {
                news.view = Some(*view);
                news.notice = None;
            }
            HelperMessage::PlacementAction(action) => news.actions.push(action),
            HelperMessage::ViewCycle(cycle) => news.cycles.push(cycle),
            HelperMessage::Bye { code, .. } => {
                let code = if code == BYE_PROTO {
                    news.notice = Some(OTHER_VERSION.into());
                    Some(BYE_PROTO_CODE)
                } else {
                    None
                };
                news.view = None;
                drop(news);
                let _ = bye.send(code);
                return;
            }
            HelperMessage::Unknown => {}
        }
    }
}

/// Starts `herdr browser-link <machine>` in a session of its own, so it
/// outlives this client; its output goes to `log`. The child is waited for
/// on a thread of its own.
pub(crate) fn spawn_helper(machine_id: &str, log: &Path) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let mut command = Command::new(exe);
    command
        .arg("browser-link")
        .arg(machine_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .env_remove("HERDR_PANE_ID");
    crate::platform::detach_background_command(&mut command);
    let mut child = command.spawn()?;
    std::thread::Builder::new()
        .name("browser-link-helper-wait".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::clients::{ClientHandler, Clients};
    use super::*;
    use crate::api::schema::BrowserPlacementKind;
    use std::sync::atomic::AtomicUsize;

    struct Quiet(Mutex<Vec<String>>);

    impl ClientHandler for Quiet {
        fn hello(&self, _: u64, machine: &MachineFacts, _: Vec<String>) {
            lock(&self.0).push(format!("hello {}", machine.label));
        }
        fn placement(&self, _: u64, spec: Option<BrowserPlacementSpec>, active: bool, _: bool) {
            lock(&self.0).push(format!(
                "placement {} {active}",
                spec.map(|s| s.id).unwrap_or_default()
            ));
        }
        fn companion(&self, _: u64, action: CompanionAction) {
            lock(&self.0).push(format!("companion {action:?}"));
        }
        fn gone(&self, _: u64) {
            lock(&self.0).push("gone".into());
        }
    }

    fn wait_for(what: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !what() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn machine() -> MachineFacts {
        MachineFacts {
            id: "0".repeat(32),
            label: "workstation".into(),
            target: "workstation".into(),
            session: "default".into(),
        }
    }

    #[test]
    fn the_client_starts_its_helper_and_sends_again_after_a_restart() {
        let dir = super::super::api_socket::tests::short_dir();
        let socket = dir.path().join("k.link");
        let seen = Arc::new(Quiet(Mutex::new(Vec::new())));
        let helper: Arc<Mutex<Option<Clients>>> = Arc::default();
        let spawns = Arc::new(AtomicUsize::new(0));
        let spawn = {
            let (socket, seen, helper, spawns) = (
                socket.clone(),
                Arc::clone(&seen),
                Arc::clone(&helper),
                Arc::clone(&spawns),
            );
            Arc::new(move || {
                spawns.fetch_add(1, Ordering::SeqCst);
                let clients = Clients::bind(&socket, Arc::clone(&seen) as Arc<dyn ClientHandler>)?;
                *lock(&helper) = Some(clients);
                Ok(())
            })
        };
        let mut config = LinkClientConfig::new(machine(), Vec::new(), socket.clone(), spawn);
        config.retry = Duration::from_millis(20);
        let client = LinkClient::start(config).unwrap();
        wait_for(|| lock(&seen.0).contains(&"hello workstation".to_string()));
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        let spec = BrowserPlacementSpec {
            id: "p1".into(),
            kind: BrowserPlacementKind::Side,
            tab_id: "w1:t1".into(),
            ..Default::default()
        };
        client.set_placement(Some(spec.clone()), true, true);
        client.set_placement(Some(spec.clone()), true, true);
        client.companion(CompanionAction::Forget);
        wait_for(|| lock(&seen.0).contains(&"companion Forget".to_string()));
        assert_eq!(
            lock(&seen.0)
                .iter()
                .filter(|s| s.starts_with("placement"))
                .count(),
            1,
            "an unchanged placement is not sent again"
        );
        wait_for(|| client.news().connected);

        // the helper goes; the next one is started and hears the placement
        lock(&helper).take();
        wait_for(|| {
            spawns.load(Ordering::SeqCst) == 2
                && lock(&seen.0)
                    .iter()
                    .filter(|s| s.starts_with("placement p1"))
                    .count()
                    == 2
        });
        assert!(lock(&helper)
            .as_ref()
            .is_some_and(|clients| clients.count() == 1));
        drop(client);
        wait_for(|| {
            lock(&helper)
                .as_ref()
                .is_some_and(|clients| clients.count() == 0)
        });
    }

    #[test]
    fn another_version_is_told_and_waited_out() {
        let dir = super::super::api_socket::tests::short_dir();
        let socket = dir.path().join("k.link");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let spawn = Arc::new(|| Err(io::Error::other("no helper in this test")));
        let mut config = LinkClientConfig::new(machine(), Vec::new(), socket, spawn);
        config.proto_retry = Duration::from_secs(60);
        let client = LinkClient::start(config).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(line.contains("\"t\":\"hello\""));
        let bye = HelperMessage::Bye {
            code: BYE_PROTO.into(),
            message: "this browser link speaks 2, not 1".into(),
        };
        stream.write_all(encode(&bye).unwrap().as_bytes()).unwrap();
        wait_for(|| client.news().notice.as_deref() == Some(OTHER_VERSION));
        listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            listener.accept().is_err(),
            "no new try before the proto retry"
        );
    }
}
