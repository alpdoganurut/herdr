//! How the browser reaches what the remote machine dials: one ssh forward per
//! host:port, from a socket of ours (0600, in the private 0700 directory) over
//! one of the browser's own masters. The remote sshd dials by name when a
//! connection comes; a failed dial is an accepted connection that ends at
//! once.
//!
//! The least recently used forward goes when there are too many: a cancelled
//! one keeps its open connections. Nothing is retried after a byte was sent.

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ssh::{BrowserMaster, ControlError, Destination, MasterError, MasterInfo, MASTERS};

/// The most forwards, all masters together.
const FORWARDS: usize = 64;
/// Fewer than this and browsing makes no sense.
const FORWARDS_MIN: usize = 8;
/// The connections the remote side dials at once, all masters together: a
/// remote sshd holds about 237 per connection and cannot be raised.
pub(crate) const REMOTE_CONNECTIONS: usize = 200;
/// What a master has open beside forwards and connections, with room.
const MASTER_OWN_FILES: u64 = 24;
/// A dial waits this long for another connection to end.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(20);
/// The persisted list, in the machine's home.
const FORWARDS_FILE: &str = "forwards.json";
/// The random part of a socket's name; the rest is the key's.
const RANDOM_LEN: usize = 24;
const KEY_LEN: usize = 8;

/// Connects to what the remote machine dials.
pub(crate) trait Dial: Send + Sync {
    fn dial(&self, to: &Destination) -> Result<RemoteConnection, DialError>;
}

/// A connection the remote side dials; it counts until it is dropped.
pub(crate) struct RemoteConnection {
    pub(crate) stream: UnixStream,
    pub(crate) permit: Permit,
}

/// Why there is no connection. Nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialError {
    /// The link is down; nothing is dialed until it is back.
    Down,
    /// [`REMOTE_CONNECTIONS`] are open, or the master takes no more.
    TooMany,
    /// herdr does not manage the ssh connection.
    Unmanaged,
    Failed(String),
}

impl From<ControlError> for DialError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::Full(_) => Self::TooMany,
            error => Self::Failed(error.to_string()),
        }
    }
}

impl From<MasterError> for DialError {
    fn from(error: MasterError) -> Self {
        match error {
            MasterError::Unmanaged => Self::Unmanaged,
            MasterError::Failed(text) => Self::Failed(text),
        }
    }
}

/// Counts the connections the remote side dials.
pub(crate) struct Connections {
    max: usize,
    open: Mutex<usize>,
    freed: Condvar,
}

/// One of them.
pub(crate) struct Permit(Arc<Connections>);

impl Connections {
    pub(crate) fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            open: Mutex::new(0),
            freed: Condvar::new(),
        })
    }

    /// Waits for room, at most `timeout`.
    pub(crate) fn enter(self: &Arc<Self>, timeout: Duration) -> Option<Permit> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut open, _) = self
            .freed
            .wait_timeout_while(open, timeout, |open| *open >= self.max)
            .unwrap_or_else(PoisonError::into_inner);
        if *open >= self.max {
            return None;
        }
        *open += 1;
        Some(Permit(Arc::clone(self)))
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut open = self.0.open.lock().unwrap_or_else(PoisonError::into_inner);
        *open -= 1;
        self.0.freed.notify_one();
    }
}

/// A master as the forwards use it.
pub(crate) trait Master: Send + Sync {
    fn ensure(&self) -> Result<MasterInfo, MasterError>;
    fn check(&self) -> Result<u32, ControlError>;
    fn forward(&self, socket: &Path, to: &Destination) -> Result<(), ControlError>;
    fn cancel(&self, socket: &Path, to: &Destination) -> Result<(), ControlError>;
    /// Ends the one running, with everything on it.
    fn close(&self);
    /// Ends the one a dead helper left.
    fn clear_stale(&self) -> io::Result<()>;
}

impl Master for BrowserMaster {
    fn ensure(&self) -> Result<MasterInfo, MasterError> {
        self.ensure()
    }
    fn check(&self) -> Result<u32, ControlError> {
        self.check()
    }
    fn forward(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
        self.forward(socket, to)
    }
    fn cancel(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
        self.cancel(socket, to)
    }
    fn close(&self) {
        self.close();
    }
    fn clear_stale(&self) -> io::Result<()> {
        self.clear_stale()
    }
}

/// An entry of `forwards.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Listed {
    /// The socket's file name.
    sock: String,
    host: String,
    port: u16,
    master_pid: u32,
}

struct Entry {
    sock: String,
    pid: u32,
    /// Its `-O forward` worked.
    ready: bool,
    used: u64,
}

impl Entry {
    fn listed(&self, to: &Destination) -> Listed {
        Listed {
            sock: self.sock.clone(),
            host: to.host().to_owned(),
            port: to.port(),
            master_pid: self.pid,
        }
    }
}

#[derive(Default)]
struct Table {
    link_up: bool,
    entries: HashMap<Destination, Entry>,
    /// Out of use, their cancel not over: still on the list.
    leaving: Vec<Listed>,
    clock: u64,
    cap: usize,
}

/// The forwards of one machine.
pub(crate) struct Forwards {
    dir: PathBuf,
    home: PathBuf,
    prefix: String,
    masters: Vec<Arc<dyn Master>>,
    connections: Arc<Connections>,
    table: Mutex<Table>,
    changed: Condvar,
}

impl Forwards {
    /// `dir` is the socket directory, `home` the machine's, `key` its key;
    /// `masters` has [`MASTERS`] of them. Takes away what a dead helper left
    /// of this machine's, and nothing else in `dir`. The link starts down.
    pub(crate) fn new(
        dir: &Path,
        home: &Path,
        key: &str,
        masters: Vec<Arc<dyn Master>>,
    ) -> io::Result<Self> {
        if masters.len() != MASTERS || key.len() < KEY_LEN || !is_hex(&key[..KEY_LEN]) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a machine's forwards need its key and its masters",
            ));
        }
        let forwards = Self {
            dir: dir.to_owned(),
            home: home.to_owned(),
            prefix: key[..KEY_LEN].to_owned(),
            masters,
            connections: Connections::new(REMOTE_CONNECTIONS),
            table: Mutex::new(Table {
                cap: FORWARDS,
                ..Table::default()
            }),
            changed: Condvar::new(),
        };
        forwards.clear_leftovers();
        forwards.persist(&forwards.table())?;
        Ok(forwards)
    }

    /// Whether the link to the machine is up: without it nothing is dialed,
    /// even if a master still answers.
    pub(crate) fn set_link(&self, up: bool) {
        self.table().link_up = up;
    }

    /// Ends the masters and takes every socket away.
    pub(crate) fn close(&self) {
        let mut table = self.table();
        table.link_up = false;
        table.entries.clear();
        table.leaving.clear();
        for master in &self.masters {
            master.close();
        }
        self.unlink_own();
        if let Err(error) = self.persist(&table) {
            tracing::debug!(%error, "could not empty the list of forwards");
        }
        self.changed.notify_all();
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn clear_leftovers(&self) {
        for listed in self.load() {
            let Some(to) = Destination::new(&listed.host, listed.port) else {
                continue;
            };
            let master = &self.masters[to.master()];
            if !self.is_own(&listed.sock) || master.check() != Ok(listed.master_pid) {
                continue;
            }
            if let Err(error) = master.cancel(&self.dir.join(&listed.sock), &to) {
                tracing::debug!(%error, "could not cancel a leftover forward");
            }
        }
        for master in &self.masters {
            if let Err(error) = master.clear_stale() {
                tracing::warn!(%error, "could not end a leftover ssh connection");
            }
        }
        self.unlink_own();
    }

    /// `^<first 8 of the key>[0-9a-f]{24}$`: the directory holds ssh masters
    /// and other machines' sockets too.
    fn is_own(&self, name: &str) -> bool {
        name.strip_prefix(&self.prefix)
            .is_some_and(|rest| rest.len() == RANDOM_LEN && is_hex(rest))
    }

    fn unlink_own(&self) {
        let Ok(files) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for file in files.flatten() {
            let own = file.file_name().to_str().is_some_and(|n| self.is_own(n));
            if own && file.file_type().is_ok_and(|kind| kind.is_socket()) {
                self.unlink_path(&file.path());
            }
        }
    }

    fn unlink(&self, sock: &str) {
        self.unlink_path(&self.dir.join(sock));
    }

    fn unlink_path(&self, path: &Path) {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::debug!(%error, "could not unlink a forward's socket");
            }
        }
    }

    fn load(&self) -> Vec<Listed> {
        std::fs::read(self.home.join(FORWARDS_FILE))
            .ok()
            .and_then(|text| serde_json::from_slice(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the list; the caller holds the table, so the last write wins.
    fn persist(&self, table: &Table) -> io::Result<()> {
        let mut list = table
            .entries
            .iter()
            .map(|(to, entry)| entry.listed(to))
            .collect::<Vec<_>>();
        list.extend(table.leaving.iter().cloned());
        let text = serde_json::to_vec(&list).map_err(io::Error::other)?;
        let path = self.home.join(FORWARDS_FILE);
        let temp = self.home.join(format!("{FORWARDS_FILE}.tmp"));
        match std::fs::remove_file(&temp) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?
            .write_all(&text)?;
        std::fs::rename(&temp, &path)
    }

    fn persist_or_log(&self, table: &Table) {
        if let Err(error) = self.persist(table) {
            tracing::warn!(%error, "could not write the list of forwards");
        }
    }

    /// The socket of a forward to `to`, added when there is none.
    fn socket_for(&self, to: &Destination) -> Result<String, DialError> {
        if !self.table().link_up {
            return Err(DialError::Down);
        }
        let master = &self.masters[to.master()];
        let info = master.ensure()?;
        let cap = forward_cap(info.file_limit).ok_or_else(|| {
            DialError::Failed(format!(
                "the ssh connection may open {} files, too few for a browser",
                info.file_limit
            ))
        })?;
        let mut table = self.table();
        let victim = loop {
            if !table.link_up {
                return Err(DialError::Down);
            }
            self.drop_other_masters(&mut table, to.master(), info.pid);
            table.clock += 1;
            let clock = table.clock;
            if let Some(entry) = table.entries.get_mut(to) {
                if entry.ready {
                    entry.used = clock;
                    return Ok(entry.sock.clone());
                }
            } else {
                table.cap = table.cap.min(cap);
                if table.entries.len() < table.cap {
                    break None;
                }
                let oldest = table
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.ready)
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(to, _)| to.clone());
                if let Some(oldest) = oldest {
                    break Some(oldest);
                }
            }
            // Another connection adds this one, or every one is being added.
            let (woken, waited) = self
                .changed
                .wait_timeout(table, QUEUE_TIMEOUT)
                .unwrap_or_else(PoisonError::into_inner);
            table = woken;
            if waited.timed_out() {
                return Err(DialError::TooMany);
            }
        };
        let victim = victim.and_then(|to| {
            let entry = table.entries.remove(&to)?;
            table.leaving.push(entry.listed(&to));
            Some((to, entry.sock))
        });
        let sock = format!(
            "{}{}",
            self.prefix,
            crate::browser::session::random_hex(RANDOM_LEN)
        );
        let used = table.clock;
        table.entries.insert(
            to.clone(),
            Entry {
                sock: sock.clone(),
                pid: info.pid,
                ready: false,
                used,
            },
        );
        // On the list before it exists: a crash leaves nothing unlisted.
        if let Err(error) = self.persist(&table) {
            table.entries.remove(to);
            self.changed.notify_all();
            return Err(DialError::Failed(format!(
                "could not write the list of forwards: {error}"
            )));
        }
        drop(table);
        if let Some((victim, sock)) = victim {
            self.take_away(&victim, &sock);
        }
        let added = master.forward(&self.dir.join(&sock), to);
        let mut table = self.table();
        // `close` may have emptied the table meanwhile.
        let entry = table.entries.get_mut(to).filter(|entry| entry.sock == sock);
        let result = match (added, entry) {
            (Ok(()), Some(entry)) => {
                entry.ready = true;
                Ok(sock)
            }
            (Ok(()), None) => {
                self.unlink(&sock);
                Err(DialError::Down)
            }
            (Err(error), entry) => {
                if entry.is_some() {
                    table.entries.remove(to);
                    self.unlink(&sock);
                    self.persist_or_log(&table);
                }
                if matches!(error, ControlError::Gone(_)) {
                    // Its control socket is gone: the next dial starts another.
                    master.close();
                }
                Err(error.into())
            }
        };
        self.changed.notify_all();
        result
    }

    /// Forwards of a master that is no more: their listeners died with it.
    fn drop_other_masters(&self, table: &mut Table, index: usize, pid: u32) {
        let stale = table
            .entries
            .iter()
            .filter(|(to, entry)| entry.ready && to.master() == index && entry.pid != pid)
            .map(|(to, _)| to.clone())
            .collect::<Vec<_>>();
        if stale.is_empty() {
            return;
        }
        for to in stale {
            if let Some(entry) = table.entries.remove(&to) {
                self.unlink(&entry.sock);
            }
        }
        self.persist_or_log(table);
        self.changed.notify_all();
    }

    /// Cancels a forward that is out of the table and takes its socket and its
    /// place on the list away.
    fn take_away(&self, to: &Destination, sock: &str) {
        match self.masters[to.master()].cancel(&self.dir.join(sock), to) {
            Ok(()) | Err(ControlError::NotForwarded) => {}
            // Without its socket nothing reaches it any more.
            Err(error) => tracing::debug!(%error, "could not cancel a forward"),
        }
        self.unlink(sock);
        let mut table = self.table();
        table.leaving.retain(|listed| listed.sock != sock);
        self.persist_or_log(&table);
    }

    /// A forward's socket took no connection: it was evicted meanwhile, the
    /// master is gone, or only this forward is.
    fn refused(&self, to: &Destination, sock: &str) {
        let pid = match self.table().entries.get(to) {
            Some(entry) if entry.sock == sock => entry.pid,
            _ => return,
        };
        let master = &self.masters[to.master()];
        match master.check() {
            Ok(running) if running == pid => {}
            // The next dial starts another, and drops this one's forwards.
            Ok(_) | Err(ControlError::Gone(_)) => master.close(),
            Err(error) => tracing::debug!(%error, "the browser's ssh connection did not answer"),
        }
        let mut table = self.table();
        match table.entries.get(to) {
            Some(entry) if entry.sock == sock => {
                let listed = entry.listed(to);
                table.entries.remove(to);
                table.leaving.push(listed);
            }
            _ => return,
        }
        drop(table);
        self.take_away(to, sock);
        self.changed.notify_all();
    }
}

/// How many forwards a master with this soft file limit can have beside its
/// connections; `None` for too few to browse.
fn forward_cap(file_limit: u64) -> Option<usize> {
    let room = file_limit.saturating_sub(MASTER_OWN_FILES + REMOTE_CONNECTIONS as u64);
    let room = usize::try_from(room).unwrap_or(FORWARDS).min(FORWARDS);
    (room >= FORWARDS_MIN).then_some(room)
}

fn is_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Dial for Forwards {
    fn dial(&self, to: &Destination) -> Result<RemoteConnection, DialError> {
        if !self.table().link_up {
            return Err(DialError::Down);
        }
        let permit = self
            .connections
            .enter(QUEUE_TIMEOUT)
            .ok_or(DialError::TooMany)?;
        let mut failure = None;
        // A refused connect sent nothing: one more try on a new forward.
        for _ in 0..2 {
            let sock = self.socket_for(to)?;
            match UnixStream::connect(self.dir.join(&sock)) {
                Ok(stream) => return Ok(RemoteConnection { stream, permit }),
                Err(error) => {
                    self.refused(to, &sock);
                    failure = Some(error);
                }
            }
        }
        Err(DialError::Failed(match failure {
            Some(error) => format!("a forward took no connection: {error}"),
            None => "a forward took no connection".to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    const KEY: &str = "0123456789abcdef";

    /// A master whose forwards are listeners here; a cancelled one leaves its
    /// socket file, as OpenSSH does.
    struct Fake {
        home: PathBuf,
        state: Mutex<FakeState>,
    }

    struct FakeState {
        pid: u32,
        file_limit: u64,
        /// Whether `check` finds it.
        alive: bool,
        fail: Option<ControlError>,
        listeners: HashMap<PathBuf, UnixListener>,
        log: Vec<String>,
        /// Forwards that were not on the list when they were added, or when
        /// they were cancelled.
        unlisted: usize,
        /// What `check` says instead of the pid.
        check_pid: Option<u32>,
        /// How long an add takes.
        delay: Duration,
    }

    impl Fake {
        fn new(home: &Path, pid: u32) -> Arc<Self> {
            Arc::new(Self {
                home: home.to_owned(),
                state: Mutex::new(FakeState {
                    pid,
                    file_limit: 512,
                    alive: true,
                    fail: None,
                    listeners: HashMap::new(),
                    log: Vec::new(),
                    unlisted: 0,
                    check_pid: None,
                    delay: Duration::ZERO,
                }),
            })
        }

        fn state(&self) -> MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }

        fn log(&self) -> Vec<String> {
            self.state().log.clone()
        }

        /// How often an operation ran.
        fn count(&self, operation: &str) -> usize {
            self.log()
                .iter()
                .filter(|line| line.starts_with(operation))
                .count()
        }
    }

    impl Master for Fake {
        fn ensure(&self) -> Result<MasterInfo, MasterError> {
            let mut state = self.state();
            if !state.alive {
                state.alive = true;
                state.pid += 1;
                state.log.push("start".into());
            }
            Ok(MasterInfo {
                pid: state.pid,
                file_limit: state.file_limit,
            })
        }

        fn check(&self) -> Result<u32, ControlError> {
            let state = self.state();
            if state.alive {
                Ok(state.check_pid.unwrap_or(state.pid))
            } else {
                Err(ControlError::Gone("no socket".into()))
            }
        }

        fn forward(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
            let delay = self.state().delay;
            std::thread::sleep(delay);
            let list = std::fs::read_to_string(self.home.join(FORWARDS_FILE)).unwrap_or_default();
            let mut state = self.state();
            state.log.push(format!("forward {to}"));
            if let Some(error) = state.fail.take() {
                return Err(error);
            }
            if !list.contains(socket.file_name().unwrap().to_str().unwrap()) {
                state.unlisted += 1;
            }
            let listener = UnixListener::bind(socket).unwrap();
            state.listeners.insert(socket.to_owned(), listener);
            Ok(())
        }

        fn cancel(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
            let list = std::fs::read_to_string(self.home.join(FORWARDS_FILE)).unwrap_or_default();
            let mut state = self.state();
            state.log.push(format!("cancel {to}"));
            if !list.contains(socket.file_name().unwrap().to_str().unwrap()) {
                state.unlisted += 1;
            }
            match state.listeners.remove(socket) {
                Some(_) => Ok(()),
                None => Err(ControlError::NotForwarded),
            }
        }

        fn close(&self) {
            let mut state = self.state();
            state.alive = false;
            state.listeners.clear();
            state.log.push("close".into());
        }

        fn clear_stale(&self) -> io::Result<()> {
            self.state().log.push("clear".into());
            Ok(())
        }
    }

    struct Rig {
        root: PathBuf,
        masters: Vec<Arc<Fake>>,
        forwards: Arc<Forwards>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with(|_, _| {})
        }

        fn with(prepare: impl FnOnce(&Path, &Path)) -> Self {
            let root = PathBuf::from("/tmp").join(format!(
                "hbf-{}-{}",
                std::process::id(),
                crate::browser::session::random_hex(6)
            ));
            let (dir, home) = (root.join("s"), root.join("home"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir_all(&home).unwrap();
            prepare(&dir, &home);
            let masters = vec![Fake::new(&home, 100), Fake::new(&home, 200)];
            let dynamic = masters
                .iter()
                .map(|master| Arc::clone(master) as Arc<dyn Master>)
                .collect();
            let forwards = Arc::new(Forwards::new(&dir, &home, KEY, dynamic).unwrap());
            forwards.set_link(true);
            Self {
                root,
                masters,
                forwards,
            }
        }

        fn files(&self) -> Vec<String> {
            let mut files = std::fs::read_dir(self.root.join("s"))
                .unwrap()
                .map(|file| file.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            files.sort();
            files
        }

        fn listed(&self) -> Vec<Listed> {
            let mut listed = self.forwards.load();
            listed.sort_by(|a, b| (&a.host, a.port).cmp(&(&b.host, b.port)));
            listed
        }

        /// The far end of the next connection to a destination.
        fn accept(&self, to: &Destination) -> UnixStream {
            let sock = self.forwards.table().entries[to].sock.clone();
            let state = self.masters[to.master()].state();
            let listener = &state.listeners[&self.root.join("s").join(sock)];
            listener.accept().unwrap().0
        }

        fn limit(&self, files: u64) {
            for master in &self.masters {
                master.state().file_limit = files;
            }
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn to(port: u16) -> Destination {
        Destination::new("localhost", port).unwrap()
    }

    /// `count` destinations of one master.
    fn of_master(index: usize, count: usize) -> Vec<Destination> {
        (1..)
            .map(to)
            .filter(|to| to.master() == index)
            .take(count)
            .collect()
    }

    #[test]
    fn a_forward_is_listed_first_shared_and_private() {
        let rig = Rig::new();
        let first = rig.forwards.dial(&to(3000)).unwrap();
        let again = rig.forwards.dial(&to(3000)).unwrap();
        let other = rig.forwards.dial(&to(3001)).unwrap();
        let forwards = rig
            .masters
            .iter()
            .map(|m| m.count("forward"))
            .sum::<usize>();
        assert_eq!(forwards, 2);
        assert!(rig.masters.iter().all(|m| m.state().unlisted == 0));
        let listed = rig.listed();
        assert_eq!(
            listed
                .iter()
                .map(|l| (l.host.as_str(), l.port))
                .collect::<Vec<_>>(),
            [("localhost", 3000), ("localhost", 3001)]
        );
        for listed in &listed {
            assert!(rig.forwards.is_own(&listed.sock), "{}", listed.sock);
            assert!(listed.sock.starts_with("01234567") && listed.sock.len() == 32);
            assert_eq!(
                listed.master_pid,
                rig.masters[to(listed.port).master()].state().pid
            );
        }
        let mode = std::fs::metadata(rig.root.join("home").join(FORWARDS_FILE)).unwrap();
        assert_eq!(mode.permissions().mode() & 0o777, 0o600);
        drop((first, again, other));
        rig.forwards.close();
        assert!(rig.files().is_empty());
        assert!(rig.listed().is_empty());
        assert_eq!(rig.forwards.dial(&to(3000)).err(), Some(DialError::Down));
    }

    #[test]
    fn the_least_recently_used_forward_goes_and_keeps_its_connections() {
        let rig = Rig::new();
        // 8 forwards beside 200 connections.
        rig.limit(232);
        let mut first = rig.forwards.dial(&to(1)).unwrap();
        let mut far_first = rig.accept(&to(1));
        let mut second = rig.forwards.dial(&to(2)).unwrap();
        let mut far_second = rig.accept(&to(2));
        for port in 3..=8 {
            rig.forwards.dial(&to(port)).unwrap();
        }
        // Used again: the second one is the oldest now.
        rig.forwards.dial(&to(1)).unwrap();
        rig.forwards.dial(&to(9)).unwrap();
        let ports = rig.listed().iter().map(|l| l.port).collect::<Vec<_>>();
        assert_eq!(ports, [1, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(rig.files().len(), 8);
        // The evicted one's open connection goes on, and so does the other's.
        for (near, far) in [
            (&mut first.stream, &mut far_first),
            (&mut second.stream, &mut far_second),
        ] {
            far.write_all(b"live").unwrap();
            let mut got = [0; 4];
            near.read_exact(&mut got).unwrap();
            assert_eq!(&got, b"live");
        }
        // Back under another name.
        let old = rig.listed()[0].sock.clone();
        let seen = rig.listed().into_iter().map(|l| l.sock).collect::<Vec<_>>();
        rig.forwards.dial(&to(2)).unwrap();
        assert!(seen.contains(&old));
        assert!(!seen.contains(&rig.forwards.table().entries[&to(2)].sock));
    }

    #[test]
    fn a_busy_machine_evicts_the_65th() {
        let rig = Rig::new();
        for port in 1..=65 {
            rig.forwards.dial(&to(port)).unwrap();
        }
        assert_eq!(rig.files().len(), FORWARDS);
        let cancels = rig.masters.iter().map(|m| m.count("cancel")).sum::<usize>();
        assert_eq!(cancels, 1);
        // Listed until its cancel is over.
        assert!(rig.masters.iter().all(|m| m.state().unlisted == 0));
        assert!(!rig.forwards.table().entries.contains_key(&to(1)));
    }

    #[test]
    fn the_file_limit_lowers_the_cap_and_then_says_so() {
        assert_eq!(forward_cap(512), Some(64));
        assert_eq!(forward_cap(u64::MAX), Some(64));
        assert_eq!(forward_cap(256), Some(32));
        assert_eq!(forward_cap(231), None);
        assert_eq!(forward_cap(0), None);
        let rig = Rig::new();
        rig.limit(128);
        match rig.forwards.dial(&to(1)).err() {
            Some(DialError::Failed(text)) => assert!(text.contains("128 files"), "{text}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn many_connections_at_once_add_one_forward() {
        let rig = Rig::new();
        rig.masters[to(3000).master()].state().delay = Duration::from_millis(50);
        let dials = (0..16)
            .map(|_| {
                let forwards = Arc::clone(&rig.forwards);
                std::thread::spawn(move || forwards.dial(&to(3000)).map(|c| c.stream))
            })
            .collect::<Vec<_>>();
        for dial in dials {
            dial.join().unwrap().unwrap();
        }
        let forwards = rig
            .masters
            .iter()
            .map(|m| m.count("forward"))
            .sum::<usize>();
        assert_eq!(forwards, 1);
        // Nobody got the socket before it existed.
        assert_eq!(
            rig.masters.iter().map(|m| m.count("cancel")).sum::<usize>(),
            0
        );
    }

    #[test]
    fn a_forward_of_another_master_closes_it() {
        let rig = Rig::new();
        let to = to(3000);
        let master = &rig.masters[to.master()];
        rig.forwards.dial(&to).unwrap();
        master.state().listeners.clear();
        master.state().check_pid = Some(999);
        rig.forwards.dial(&to).unwrap();
        assert_eq!(master.count("close"), 1);
    }

    #[test]
    fn many_hosts_at_once_stay_under_the_cap() {
        let rig = Rig::new();
        rig.limit(232);
        let dials = (1..=40)
            .map(|port| {
                let forwards = Arc::clone(&rig.forwards);
                // Evicted before it connected: that is its one retry.
                std::thread::spawn(move || forwards.dial(&to(port)).is_ok())
            })
            .collect::<Vec<_>>();
        for dial in dials {
            dial.join().unwrap();
        }
        assert!(rig.files().len() <= 8, "{:?}", rig.files());
        assert_eq!(rig.listed().len(), rig.files().len());
        let live = rig
            .masters
            .iter()
            .map(|m| m.state().listeners.len())
            .sum::<usize>();
        assert_eq!(live, rig.files().len());
    }

    #[test]
    fn a_replaced_master_gets_its_forwards_anew() {
        let rig = Rig::new();
        let mine = of_master(0, 2);
        let other = of_master(1, 1);
        for to in mine.iter().chain(&other) {
            rig.forwards.dial(to).unwrap();
        }
        let before = rig.forwards.table().entries[&mine[0]].sock.clone();
        // It died and is back under another pid.
        rig.masters[0].close();
        rig.forwards.dial(&mine[0]).unwrap();
        assert_eq!(rig.masters[0].count("start"), 1);
        let table = rig.forwards.table();
        assert_ne!(table.entries[&mine[0]].sock, before);
        assert!(
            !table.entries.contains_key(&mine[1]),
            "added again when asked for"
        );
        assert!(table.entries.contains_key(&other[0]));
        drop(table);
        assert_eq!(rig.masters[1].count("cancel"), 0);
        assert_eq!(rig.files().len(), 2);
        assert_eq!(rig.listed().len(), 2);
    }

    #[test]
    fn a_refusing_forward_is_added_again_and_only_a_gone_master_is_closed() {
        let rig = Rig::new();
        let to = to(3000);
        let master = &rig.masters[to.master()];
        rig.forwards.dial(&to).unwrap();
        // The listener is gone, the master is not.
        master.state().listeners.clear();
        rig.forwards.dial(&to).unwrap();
        assert_eq!((master.count("forward"), master.count("close")), (2, 0));
        assert_eq!(rig.files().len(), 1);

        // Its control socket is gone while it seemed to run.
        master.state().fail = Some(ControlError::Gone("no socket".into()));
        master.state().listeners.clear();
        assert!(matches!(
            rig.forwards.dial(&to).err(),
            Some(DialError::Failed(_))
        ));
        assert_eq!(master.count("close"), 1);
        rig.forwards.dial(&to).unwrap();
        assert_eq!(master.count("start"), 1);
    }

    #[test]
    fn failed_adds_leave_nothing() {
        let rig = Rig::new();
        for (error, expected) in [
            (
                ControlError::Full("hello exchange".into()),
                DialError::TooMany,
            ),
            (
                ControlError::Failed("forwarding request failed".into()),
                DialError::Failed("forwarding request failed".into()),
            ),
        ] {
            rig.masters[to(1).master()].state().fail = Some(error);
            assert_eq!(rig.forwards.dial(&to(1)).err(), Some(expected));
            assert!(rig.files().is_empty() && rig.listed().is_empty());
        }
        rig.forwards.dial(&to(1)).unwrap();
    }

    #[test]
    fn without_the_link_nothing_is_dialed() {
        let rig = Rig::new();
        rig.forwards.dial(&to(1)).unwrap();
        rig.forwards.set_link(false);
        assert_eq!(rig.forwards.dial(&to(1)).err(), Some(DialError::Down));
        assert_eq!(rig.forwards.dial(&to(2)).err(), Some(DialError::Down));
        assert_eq!(rig.files().len(), 1);
        rig.forwards.set_link(true);
        rig.forwards.dial(&to(1)).unwrap();
        assert_eq!(
            rig.masters
                .iter()
                .map(|m| m.count("forward"))
                .sum::<usize>(),
            1
        );
    }

    #[test]
    fn a_dead_helpers_forwards_are_cancelled_and_others_are_not_touched() {
        let own = format!("01234567{}", "a".repeat(24));
        let foreign = [
            format!("01234568{}", "a".repeat(24)),
            format!("01234567{}", "a".repeat(23)),
            format!("01234567{}G", "a".repeat(23)),
            "0123456789abcdef.ssh0".to_owned(),
            "0123456789abcdef.link".to_owned(),
        ];
        let listed_to = of_master(0, 1).remove(0);
        let rig = Rig::with(|dir, home| {
            for name in foreign.iter().chain([&own]) {
                UnixListener::bind(dir.join(name)).unwrap();
            }
            // A file under our pattern is not a socket of ours.
            std::fs::write(dir.join(format!("01234567{}", "b".repeat(24))), b"x").unwrap();
            let entry = |sock: &str, to: &Destination, pid| Listed {
                sock: sock.to_owned(),
                host: to.host().to_owned(),
                port: to.port(),
                master_pid: pid,
            };
            let mut list = vec![
                entry(&own, &listed_to, 100),
                entry(&own, &of_master(0, 2)[1], 99),
                entry(&foreign[0], &of_master(0, 3)[2], 100),
            ];
            list.push(Listed {
                host: "-oProxyCommand=x".into(),
                ..list[0].clone()
            });
            std::fs::write(home.join(FORWARDS_FILE), serde_json::to_vec(&list).unwrap()).unwrap();
        });
        assert_eq!(
            rig.masters[0].log(),
            [format!("cancel {listed_to}"), "clear".to_owned()]
        );
        assert_eq!(rig.masters[1].log(), ["clear"]);
        let mut left = foreign.to_vec();
        left.push(format!("01234567{}", "b".repeat(24)));
        left.sort();
        assert_eq!(rig.files(), left);
        assert!(rig.listed().is_empty());
    }

    #[test]
    fn a_broken_list_is_no_obstacle() {
        let rig = Rig::with(|_, home| {
            std::fs::write(home.join(FORWARDS_FILE), b"{not json").unwrap();
        });
        rig.forwards.dial(&to(1)).unwrap();
        assert_eq!(rig.listed().len(), 1);
    }

    #[test]
    fn connections_queue_and_then_are_refused() {
        let connections = Connections::new(1);
        let first = connections.enter(Duration::ZERO).unwrap();
        assert!(connections.enter(Duration::from_millis(20)).is_none());
        let entered = Arc::new(AtomicBool::new(false));
        let waiting = std::thread::spawn({
            let (connections, entered) = (Arc::clone(&connections), Arc::clone(&entered));
            move || {
                let permit = connections.enter(Duration::from_secs(10));
                entered.store(permit.is_some(), Ordering::SeqCst);
            }
        });
        std::thread::sleep(Duration::from_millis(20));
        assert!(!entered.load(Ordering::SeqCst));
        drop(first);
        waiting.join().unwrap();
        assert!(entered.load(Ordering::SeqCst));
        assert!(connections.enter(Duration::ZERO).is_some());
    }

    #[test]
    fn a_connection_counts_until_it_is_dropped() {
        let rig = Rig::new();
        let open = || *rig.forwards.connections.open.lock().unwrap();
        let first = rig.forwards.dial(&to(1)).unwrap();
        let second = rig.forwards.dial(&to(1)).unwrap();
        assert_eq!(open(), 2);
        drop((first, second));
        assert_eq!(open(), 0);
        rig.masters[to(2).master()].state().fail = Some(ControlError::TimedOut);
        assert!(rig.forwards.dial(&to(2)).is_err());
        assert_eq!(open(), 0);
    }
}
