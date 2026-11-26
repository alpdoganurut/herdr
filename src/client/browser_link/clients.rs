//! The helper's clients: every herdr+ client on this Mac that shows the
//! machine talks to its helper on `<key>.link` (0600), one connection per
//! client, JSON lines tagged by `t` (contract 2.7). The client says where it
//! shows the browser and what the user did with the companion; the helper
//! says how the link and the companion are and relays the companion's asks.
//!
//! A client's processes are the socket peer's, never what the message says
//! (contract 3.2 rule 8).

use std::collections::HashMap;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::api::schema::{BrowserCompanionAppInfo, BrowserPlacementSpec, BrowserPlacementStatus};
use crate::server::headless::placement_action::PlacementActionPayload;
use crate::server::headless::view_cycle::ViewCyclePayload;

/// The version of these messages; another one is answered `bye{proto}`.
pub(crate) const CLIENT_PROTO: u32 = 1;
/// `bye` codes the helper sends a client.
pub(crate) const BYE_PROTO: &str = "proto";
pub(crate) const BYE_QUIT: &str = "quit";
/// The longest message line.
const LINE_MAX: usize = 256 * 1024;
/// A client says hello within this.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that does not read for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// The most pids a placement carries (the board's cap).
const PIDS_MAX: usize = 16;

/// The machine the client means (its catalog entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct MachineFacts {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: String,
    pub(crate) target: String,
    pub(crate) session: String,
}

/// What the user asked of the machine's companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompanionAction {
    Start,
    Stop,
    Restart,
    Focus,
    /// "Forget <machine>'s logins": only after the user confirmed it.
    Forget,
    #[serde(other)]
    Unknown,
}

/// client → helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    Hello {
        proto: u32,
        #[serde(default)]
        herdr: String,
        machine: MachineFacts,
        /// The user's direct bindings of `keys.cycle_browser` (the lease's
        /// `view_cycle_keys`).
        #[serde(default)]
        cycle_keys: Vec<String>,
    },
    /// Where this client shows the browser (`None`: nowhere); `active`: its
    /// surface shows this machine now; `focused`: its terminal has the
    /// user's focus.
    Placement {
        #[serde(default)]
        spec: Option<BrowserPlacementSpec>,
        #[serde(default)]
        active: bool,
        #[serde(default)]
        focused: bool,
    },
    Companion {
        action: CompanionAction,
    },
    #[serde(other)]
    Unknown,
}

/// How the link and the companion are, as a client shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) struct LinkView {
    /// `connecting`, `linked`, `reconnecting`, `in_use` (another Mac has
    /// the machine's browser), `off` (the machine takes no link) or
    /// `failed`.
    pub(crate) link: String,
    /// `in_use`: which Mac, as the machine said it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<String>,
    /// Why, for the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<String>,
    pub(crate) companion: BrowserCompanionAppInfo,
    #[serde(default)]
    pub(crate) reports: Vec<BrowserPlacementStatus>,
}

/// helper → client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(crate) enum HelperMessage {
    State(Box<LinkView>),
    PlacementAction(PlacementActionPayload),
    ViewCycle(ViewCyclePayload),
    Bye {
        code: String,
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Unknown,
}

pub(crate) fn encode<T: Serialize>(message: &T) -> io::Result<String> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    Ok(line)
}

/// What the helper does with its clients' messages. Called on the
/// client's reading thread.
pub(crate) trait ClientHandler: Send + Sync {
    fn hello(&self, client: u64, machine: &MachineFacts, cycle_keys: Vec<String>);
    /// `spec.pids` are the socket peer's already.
    fn placement(
        &self,
        client: u64,
        spec: Option<BrowserPlacementSpec>,
        active: bool,
        focused: bool,
    );
    fn companion(&self, client: u64, action: CompanionAction);
    fn gone(&self, client: u64);
}

struct Conn {
    stream: UnixStream,
}

struct Shared {
    conns: Mutex<HashMap<u64, Conn>>,
    next_id: AtomicU64,
    /// Since when no client is connected (`None` while one is).
    idle_since: Mutex<Option<Instant>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The bound `<key>.link`; dropping it says `bye{quit}` to every client and
/// removes the file.
pub(crate) struct Clients {
    path: PathBuf,
    shared: Arc<Shared>,
    stop: Option<UnixStream>,
    accept: Option<JoinHandle<()>>,
}

impl Clients {
    pub(crate) fn bind(path: &Path, handler: Arc<dyn ClientHandler>) -> io::Result<Self> {
        let listener: UnixListener = super::api_socket::bind_private(path)?;
        let shared = Arc::new(Shared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            idle_since: Mutex::new(Some(Instant::now())),
        });
        let (stop, stopped) = UnixStream::pair()?;
        let accept = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("browser-link-clients".into())
                .spawn(move || {
                    super::api_socket::accept_loop(&listener, &stopped, move |stream| {
                        serve(stream, &shared, &handler)
                    })
                })?
        };
        Ok(Self {
            path: path.to_owned(),
            shared,
            stop: Some(stop),
            accept: Some(accept),
        })
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        lock(&self.shared.conns).len()
    }

    /// Since when no client is connected; `None` while one is.
    pub(crate) fn idle_since(&self) -> Option<Instant> {
        *lock(&self.shared.idle_since)
    }

    pub(crate) fn send(&self, client: u64, message: &HelperMessage) {
        let Ok(line) = encode(message) else {
            return;
        };
        let mut conns = lock(&self.shared.conns);
        let failed = conns
            .get_mut(&client)
            .is_some_and(|conn| conn.stream.write_all(line.as_bytes()).is_err());
        if failed {
            if let Some(conn) = conns.remove(&client) {
                let _ = conn.stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    pub(crate) fn broadcast(&self, message: &HelperMessage) {
        let ids: Vec<u64> = lock(&self.shared.conns).keys().copied().collect();
        for id in ids {
            self.send(id, message);
        }
    }
}

impl Drop for Clients {
    fn drop(&mut self) {
        self.broadcast(&HelperMessage::Bye {
            code: BYE_QUIT.into(),
            message: "the browser link ends".into(),
        });
        drop(self.stop.take());
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        for (_, conn) in lock(&self.shared.conns).drain() {
            let _ = conn.stream.shutdown(std::net::Shutdown::Both);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The socket peer and the processes above it, nearest first.
fn peer_pids(stream: &UnixStream) -> Vec<u32> {
    let Ok(clone) = stream.try_clone() else {
        return Vec::new();
    };
    let local = crate::ipc::LocalStream::UdSocket(
        interprocess::os::unix::uds_local_socket::Stream::from(clone),
    );
    let mut pids = crate::platform::local_stream_peer_pid(&local)
        .map(crate::platform::process_ancestry)
        .unwrap_or_default();
    pids.truncate(PIDS_MAX);
    pids
}

fn read_message(reader: &mut impl io::BufRead) -> io::Result<Option<ClientMessage>> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(LINE_MAX as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > LINE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a line too long",
        ));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn serve(stream: UnixStream, shared: &Arc<Shared>, handler: &Arc<dyn ClientHandler>) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(HELLO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        return;
    }
    let pids = peer_pids(&stream);
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let (machine, cycle_keys) = match read_message(&mut reader) {
        Ok(Some(ClientMessage::Hello {
            proto: CLIENT_PROTO,
            machine,
            cycle_keys,
            ..
        })) => (machine, cycle_keys),
        Ok(Some(ClientMessage::Hello { proto, .. })) => {
            let mut stream = stream;
            if let Ok(line) = encode(&HelperMessage::Bye {
                code: BYE_PROTO.into(),
                message: format!("this browser link speaks {CLIENT_PROTO}, not {proto}"),
            }) {
                let _ = stream.write_all(line.as_bytes());
            }
            return;
        }
        _ => return,
    };
    if reader.get_ref().set_read_timeout(None).is_err() {
        return;
    }
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    lock(&shared.conns).insert(id, Conn { stream: writer });
    *lock(&shared.idle_since) = None;
    handler.hello(id, &machine, cycle_keys);
    loop {
        match read_message(&mut reader) {
            Ok(Some(ClientMessage::Placement {
                spec,
                active,
                focused,
            })) => {
                let spec = spec.map(|mut spec| {
                    spec.pids = pids.clone();
                    spec
                });
                handler.placement(id, spec, active, focused);
            }
            Ok(Some(ClientMessage::Companion { action })) => handler.companion(id, action),
            Ok(Some(ClientMessage::Hello { .. } | ClientMessage::Unknown)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    let mut conns = lock(&shared.conns);
    conns.remove(&id);
    if conns.is_empty() {
        *lock(&shared.idle_since) = Some(Instant::now());
    }
    drop(conns);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    handler.gone(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::BrowserPlacementKind;
    use std::sync::mpsc;

    #[derive(Debug, PartialEq)]
    enum Seen {
        Hello(u64, String),
        Placement(u64, Option<Vec<u32>>, bool, bool),
        Companion(u64, CompanionAction),
        Gone(u64),
    }

    struct Recorder(Mutex<mpsc::Sender<Seen>>);

    impl ClientHandler for Recorder {
        fn hello(&self, client: u64, machine: &MachineFacts, _: Vec<String>) {
            let _ = lock(&self.0).send(Seen::Hello(client, machine.label.clone()));
        }
        fn placement(
            &self,
            client: u64,
            spec: Option<BrowserPlacementSpec>,
            active: bool,
            focused: bool,
        ) {
            let _ = lock(&self.0).send(Seen::Placement(
                client,
                spec.map(|s| s.pids),
                active,
                focused,
            ));
        }
        fn companion(&self, client: u64, action: CompanionAction) {
            let _ = lock(&self.0).send(Seen::Companion(client, action));
        }
        fn gone(&self, client: u64) {
            let _ = lock(&self.0).send(Seen::Gone(client));
        }
    }

    pub(crate) fn hello(label: &str) -> ClientMessage {
        ClientMessage::Hello {
            proto: CLIENT_PROTO,
            herdr: "test".into(),
            machine: MachineFacts {
                id: "0".repeat(32),
                label: label.into(),
                target: "buildbox".into(),
                session: "default".into(),
            },
            cycle_keys: vec!["ctrl+shift+b".into()],
        }
    }

    fn say(stream: &mut UnixStream, message: &ClientMessage) {
        stream
            .write_all(encode(message).unwrap().as_bytes())
            .unwrap();
    }

    fn hear(reader: &mut BufReader<UnixStream>) -> Option<HelperMessage> {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    }

    #[test]
    fn pids_come_from_the_peer_and_another_proto_is_told_bye() {
        let dir = super::super::api_socket::tests::short_dir();
        let path = dir.path().join("k.link");
        let (seen, saw) = mpsc::channel();
        let clients = Clients::bind(&path, Arc::new(Recorder(Mutex::new(seen)))).unwrap();
        assert_eq!(clients.count(), 0);
        assert!(clients.idle_since().is_some());

        let mut stream = UnixStream::connect(&path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        say(&mut stream, &hello("buildbox"));
        let wait = || saw.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(wait(), Seen::Hello(1, "buildbox".into()));
        assert_eq!(clients.count(), 1);
        assert!(clients.idle_since().is_none());
        let spec = BrowserPlacementSpec {
            id: "p1".into(),
            kind: BrowserPlacementKind::Side,
            tab_id: "w1:t1".into(),
            pids: vec![1, 2, 3],
            shown: true,
            ..Default::default()
        };
        say(
            &mut stream,
            &ClientMessage::Placement {
                spec: Some(spec),
                active: true,
                focused: false,
            },
        );
        let Seen::Placement(1, Some(pids), true, false) = wait() else {
            panic!("no placement");
        };
        assert_eq!(
            pids.first(),
            Some(&std::process::id()),
            "the peer, not the message"
        );
        assert_ne!(pids, vec![1, 2, 3]);
        say(
            &mut stream,
            &ClientMessage::Companion {
                action: CompanionAction::Forget,
            },
        );
        assert_eq!(wait(), Seen::Companion(1, CompanionAction::Forget));

        clients.send(
            1,
            &HelperMessage::ViewCycle(ViewCyclePayload {
                boot_id: "b".into(),
                revision: 1,
                tab_id: None,
            }),
        );
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        assert!(matches!(
            hear(&mut reader),
            Some(HelperMessage::ViewCycle(_))
        ));

        let mut old = UnixStream::connect(&path).unwrap();
        old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut hello = hello("buildbox");
        if let ClientMessage::Hello { proto, .. } = &mut hello {
            *proto = 2;
        }
        say(&mut old, &hello);
        let mut old_reader = BufReader::new(old);
        assert_eq!(
            hear(&mut old_reader),
            Some(HelperMessage::Bye {
                code: BYE_PROTO.into(),
                message: "this browser link speaks 1, not 2".into()
            })
        );
        assert_eq!(clients.count(), 1);

        drop(stream);
        drop(reader);
        assert_eq!(wait(), Seen::Gone(1));
        assert!(clients.idle_since().is_some());
        drop(clients);
        assert!(!path.exists());
    }
}
