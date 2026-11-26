//! The helper's ssh: the link child on the client's shared connection, and
//! the browser's own connections ("masters") that carry its forwards.
//!
//! The browser never shares the client's connection: the remote sshd resolves
//! names in one process per connection, so one slow lookup would freeze the
//! terminal's keys too (spike S9). A master is this process's child and ends
//! with it: its remote command is `cat`, which ends with our stdin pipe.
//!
//! OpenSSH's control commands are judged by what they print, never by their
//! exit code (a failed `-O cancel` exits 0, spike S7).

use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use crate::remote::{SavedSshLink, SavedSshLinkChild};
#[cfg(test)]
use crate::remote::{SavedSshLinkEnd, SavedSshLinkProcess};

/// The link child must have started its bridge by then.
const LINK_START_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(test)]
const LINK_END_TIMEOUT: Duration = Duration::from_secs(1);
/// One control command; a live master answers within milliseconds.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
/// A master's connection: `ConnectTimeout=10`, then keys and the login.
const MASTER_START_TIMEOUT: Duration = Duration::from_secs(20);
const MASTER_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
/// After a failed start every dial gets that failure for this long, instead
/// of a login of its own.
const MASTER_RETRY_AFTER: Duration = Duration::from_secs(2);
const STDERR_LIMIT: usize = 16 * 1024;
/// The soft file limit a master starts with: one file per forward and one per
/// open connection (macOS gives 256).
pub(crate) const MASTER_FILE_LIMIT: u64 = 512;
/// The browser's connections to one machine. A remote sshd process holds
/// about 237 forwarded connections at macOS's default limit (spike S7).
pub(crate) const MASTERS: usize = 2;

/// What a master has beside herdr's managed options; before them, because
/// OpenSSH keeps the first value it sees.
const MASTER_OPTIONS: &[&str] = &[
    "ControlMaster=yes",
    "ControlPersist=no",
    // A connection that died in the Mac's sleep ends in 20 s instead of the
    // managed 60: until then every page on it hangs.
    "ServerAliveInterval=5",
    "ServerAliveCountMax=4",
    // The user's own forwards belong to the user's own connection.
    "ClearAllForwardings=yes",
    "ForwardAgent=no",
    "ForwardX11=no",
    "PermitLocalCommand=no",
    "RemoteCommand=none",
    "RequestTTY=no",
    "StreamLocalBindMask=0177",
    "StreamLocalBindUnlink=no",
];

/// The name a machine's files and sockets have on this Mac: one per home,
/// target and session.
pub(crate) fn machine_key(state_dir: &Path, target: &str, session: &str) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    let mut hash = Sha256::new();
    hash.update(state_dir.as_os_str().as_bytes());
    hash.update([0]);
    hash.update(target.as_bytes());
    hash.update([0]);
    hash.update(session.as_bytes());
    format!("{:x}", hash.finalize())[..16].to_owned()
}

/// Tests (and only tests) put the helper's sockets in a directory of their
/// own instead of the shared one: an absolute, short path, made 0700.
pub(crate) const SOCKET_DIR_ENV: &str = "HERDR_BROWSER_LINK_SOCKET_DIR";

/// The private directory of the helper's sockets (0700, ours, no symlink). The
/// client's masters and other machines' sockets are in it too.
pub(crate) fn socket_dir() -> io::Result<PathBuf> {
    match std::env::var_os(SOCKET_DIR_ENV).filter(|dir| !dir.is_empty()) {
        Some(dir) => private_dir(Path::new(&dir)),
        None => crate::platform::shared_ssh_dir(),
    }
}

/// `dir`, made 0700 when missing; refused unless it is absolute, a
/// directory (not a symlink), ours and 0700.
fn private_dir(dir: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    if !dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{SOCKET_DIR_ENV} must be an absolute path"),
        ));
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o7777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} must be ours, mode 0700, and not a symlink",
                dir.display()
            ),
        ));
    }
    Ok(dir.to_owned())
}

/// Nanoseconds on the clock that goes on while the Mac sleeps: what pings are
/// stamped with, so that an op that waited out the sleep is stale.
pub(crate) fn clock_ns() -> io::Result<u64> {
    crate::platform::clock_with_sleep_ns()
}

/// The machine's ssh, ready to [`open_link`] and for its [`BrowserMaster`]s.
/// `cancelled` ends every wait in it.
pub(crate) fn machine_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Arc<SavedSshLink>> {
    SavedSshLink::new(profile_id, target, session, cancelled).map(Arc::new)
}

/// Opens the link child: the remote API bridge, ready to carry JSON lines.
/// Nothing was sent before it returns, so a failed start may be repeated:
/// once with rediscovered metadata, once after a dead shared connection was
/// retired.
pub(crate) fn open_link(link: &SavedSshLink) -> io::Result<SavedSshLinkChild> {
    open_link_within(link, LINK_START_TIMEOUT)
}

/// Ends a link child by the end of its input, or after a second by its own
/// process handle; the shared connection goes on.
#[cfg(test)]
pub(crate) fn end_link(
    writer: Box<dyn io::Write + Send>,
    process: SavedSshLinkProcess,
) -> SavedSshLinkEnd {
    drop(writer);
    process.finish(LINK_END_TIMEOUT)
}

fn open_link_within(link: &SavedSshLink, timeout: Duration) -> io::Result<SavedSshLinkChild> {
    let mut use_cached = true;
    let mut may_recover = true;
    loop {
        let (command, used_cached) = link.api_command(use_cached)?;
        let failure = match link.connect(command, timeout) {
            Ok(child) => return Ok(child),
            Err(failure) => failure,
        };
        if failure.stale && used_cached {
            link.invalidate_metadata();
            use_cached = false;
        } else if !failure.stale && may_recover && !link.cancelled() && link.recover_transport() {
            may_recover = false;
        } else {
            return Err(failure.error);
        }
    }
}

/// A host and port the remote side dials, in the one spelling that may reach
/// an ssh argument: a lowercased name or IPv4 address, or a bracketed IPv6
/// address printed by us. A `/` would be read as a socket path on the remote
/// side, a leading `-` as an option.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Destination {
    host: String,
    port: u16,
}

impl Destination {
    /// `host` as a URL has it: an IPv6 address in brackets, one final dot
    /// allowed (and dropped).
    pub(crate) fn new(host: &str, port: u16) -> Option<Self> {
        if port == 0 {
            return None;
        }
        let host = if let Some(address) = host.strip_prefix('[') {
            let address: std::net::Ipv6Addr = address.strip_suffix(']')?.parse().ok()?;
            format!("[{address}]")
        } else {
            let mut host = host.to_ascii_lowercase();
            if host.ends_with('.') {
                host.pop();
            }
            let word =
                |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
            let bytes = host.as_bytes();
            let named = (1..=253).contains(&bytes.len())
                && word(bytes[0])
                && word(bytes[bytes.len() - 1])
                && bytes
                    .iter()
                    .all(|&byte| word(byte) || matches!(byte, b'.' | b'-'));
            if !named {
                return None;
            }
            host
        };
        Some(Self { host, port })
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// Which of the browser's masters dials it: the same one in every process.
    pub(crate) fn master(&self) -> usize {
        usize::from(Sha256::digest(self.to_string())[0]) % MASTERS
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Why a control command did not do its work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlError {
    /// No master listens on the control path.
    Gone(String),
    /// The master answers no new command: it is at its file limit.
    Full(String),
    /// `cancel`: the master has no such forward.
    NotForwarded,
    /// No answer in time.
    TimedOut,
    /// Everything else it said; an answer we do not know is a failure.
    Failed(String),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone(text) => write!(f, "the ssh connection is gone: {text}"),
            Self::Full(text) => write!(f, "the ssh connection takes no more: {text}"),
            Self::NotForwarded => f.write_str("port not forwarded"),
            Self::TimedOut => f.write_str("the ssh connection did not answer"),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

fn output_text(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text.trim().replace('\r', "")
}

fn control_failure(text: &str, status: &str) -> ControlError {
    if text.contains("master hello exchange failed") || text.contains("read from master failed") {
        ControlError::Full(text.to_owned())
    } else if text.contains("Control socket connect(") {
        ControlError::Gone(text.to_owned())
    } else if text.contains("port not forwarded") {
        ControlError::NotForwarded
    } else if text.is_empty() {
        ControlError::Failed(format!("ssh ended with {status} and said nothing"))
    } else {
        ControlError::Failed(text.to_owned())
    }
}

/// `forward` and `cancel` say nothing when they worked.
fn parse_silent(output: &Output) -> Result<(), ControlError> {
    let text = output_text(output);
    if text.is_empty() && output.status.success() {
        Ok(())
    } else {
        Err(control_failure(&text, &output.status.to_string()))
    }
}

fn parse_check(output: &Output) -> Result<u32, ControlError> {
    let text = output_text(output);
    text.split_once("Master running (pid=")
        .and_then(|(_, rest)| rest.split_once(')'))
        .and_then(|(pid, _)| pid.parse().ok())
        .ok_or_else(|| control_failure(&text, &output.status.to_string()))
}

fn parse_exit(output: &Output) -> Result<(), ControlError> {
    let text = output_text(output);
    if text.contains("Exit request sent.") {
        Ok(())
    } else {
        Err(control_failure(&text, &output.status.to_string()))
    }
}

/// A running master.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MasterInfo {
    pub(crate) pid: u32,
    /// Its soft file limit.
    pub(crate) file_limit: u64,
}

/// Why there is no master.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MasterError {
    /// `remote.manage_ssh_config = false`: herdr may not share connections.
    Unmanaged,
    Failed(String),
}

impl fmt::Display for MasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmanaged => f.write_str(
                "browsing through a machine needs herdr's managed ssh connection (remote.manage_ssh_config)",
            ),
            Self::Failed(text) => f.write_str(text),
        }
    }
}

/// One of the browser's own ssh connections to a machine. Started by the
/// first [`Self::ensure`], ended by [`Self::close`] or with this process.
pub(crate) struct BrowserMaster {
    link: Arc<SavedSshLink>,
    control: PathBuf,
    state: Mutex<MasterState>,
}

#[derive(Default)]
struct MasterState {
    running: Option<RunningMaster>,
    failed: Option<(Instant, MasterError)>,
}

struct RunningMaster {
    info: MasterInfo,
    child: MasterChild,
}

/// The ssh process of a master: stdin's end ends it, and only a master that
/// ignores that is ended by its process handle.
struct MasterChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl MasterChild {
    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.exited() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    fn stderr(&self) -> String {
        let stderr = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&stderr).trim().replace('\r', "")
    }
}

impl Drop for MasterChild {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if !self.wait(MASTER_EXIT_TIMEOUT) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl BrowserMaster {
    /// `dir` is the private socket directory (`platform::shared_ssh_dir`),
    /// `index` below [`MASTERS`]. The control path is literal, without
    /// OpenSSH's `%C`: the key names the target already, and `%C` changes
    /// with this Mac's host name and with the proxy of a control command.
    pub(crate) fn new(link: Arc<SavedSshLink>, dir: &Path, key: &str, index: usize) -> Self {
        Self {
            link,
            control: dir.join(format!("{key}.ssh{index}")),
            state: Mutex::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn control_path(&self) -> &Path {
        &self.control
    }

    fn state(&self) -> MutexGuard<'_, MasterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The running master, started when there is none.
    pub(crate) fn ensure(&self) -> Result<MasterInfo, MasterError> {
        let mut state = self.state();
        if let Some(running) = &mut state.running {
            if !running.child.exited() {
                return Ok(running.info);
            }
            state.running = None;
        }
        if let Some((at, error)) = &state.failed {
            if at.elapsed() < MASTER_RETRY_AFTER {
                return Err(error.clone());
            }
        }
        match self.start() {
            Ok(running) => {
                let info = running.info;
                state.failed = None;
                state.running = Some(running);
                Ok(info)
            }
            Err(error) => {
                tracing::warn!(%error, "the browser's ssh connection did not start");
                state.failed = Some((Instant::now(), error.clone()));
                Err(error)
            }
        }
    }

    /// The master that runs now, or `None`; never starts one.
    #[cfg(test)]
    pub(crate) fn running(&self) -> Option<MasterInfo> {
        let mut state = self.state();
        let running = state.running.as_mut()?;
        (!running.child.exited()).then_some(running.info)
    }

    fn start(&self) -> Result<RunningMaster, MasterError> {
        let command = self
            .link
            .unshared_command(MASTER_OPTIONS)
            .ok_or(MasterError::Unmanaged)?;
        self.clear_stale().map_err(|error| {
            MasterError::Failed(format!(
                "could not clear {}: {error}",
                self.control.display()
            ))
        })?;
        let failed = |text: String| MasterError::Failed(text);
        let mut child = self
            .spawn(command)
            .map_err(|error| failed(format!("could not start ssh: {error}")))?;
        let deadline = Instant::now() + MASTER_START_TIMEOUT;
        let pid = loop {
            if child.exited() {
                // The last words are in the pipe until its reader had them.
                std::thread::sleep(Duration::from_millis(50));
                let stderr = child.stderr();
                return Err(failed(if stderr.is_empty() {
                    "the ssh connection ended while it started".to_owned()
                } else {
                    stderr
                }));
            }
            if self.link.cancelled() {
                return Err(failed("the ssh connection was cancelled".to_owned()));
            }
            if self.control.symlink_metadata().is_ok() {
                if let Ok(pid) = self.check() {
                    break pid;
                }
            }
            if Instant::now() >= deadline {
                return Err(failed(
                    "the ssh connection did not start in time".to_owned(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(RunningMaster {
            info: MasterInfo {
                pid,
                file_limit: crate::platform::soft_file_limit_for(MASTER_FILE_LIMIT).unwrap_or(0),
            },
            child,
        })
    }

    fn spawn(&self, mut command: Command) -> io::Result<MasterChild> {
        use std::os::unix::process::CommandExt as _;

        command
            .arg("-S")
            .arg(&self.control)
            .arg("-T")
            .arg(self.link.target())
            .arg("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // Only system calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                let _ = crate::platform::raise_soft_file_limit(MASTER_FILE_LIMIT);
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let kept = Arc::clone(&stderr);
            std::thread::Builder::new()
                .name("browser-ssh-err".into())
                .spawn(move || {
                    let mut buffer = [0_u8; 4096];
                    while let Ok(read @ 1..) = pipe.read(&mut buffer) {
                        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                        let room = STDERR_LIMIT.saturating_sub(kept.len());
                        kept.extend_from_slice(&buffer[..read.min(room)]);
                    }
                })?;
        }
        Ok(MasterChild {
            child,
            stdin,
            stderr,
        })
    }

    /// Ends the master a dead helper left on this control path and takes its
    /// socket away. Nothing but a socket of ours is touched.
    pub(crate) fn clear_stale(&self) -> io::Result<()> {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let metadata = match self.control.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not a socket of the current user",
            ));
        }
        if self.request_exit().is_ok() {
            let deadline = Instant::now() + MASTER_EXIT_TIMEOUT;
            while self.control.symlink_metadata().is_ok() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        match std::fs::remove_file(&self.control) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// A control command. The configuration stays out of it (`-F /dev/null`):
    /// it would put the user's own forwards into every `forward` and `cancel`.
    /// The proxy that always fails keeps a dead master from becoming a fresh
    /// login.
    fn control_command(&self, operation: &str, forward: Option<&str>) -> Command {
        let mut command = Command::new("ssh");
        command
            .args(["-F", "/dev/null", "-S"])
            .arg(&self.control)
            .args([
                "-o",
                "ControlMaster=no",
                "-o",
                "ControlPersist=no",
                "-o",
                "ProxyCommand=/usr/bin/false",
                "-o",
                "BatchMode=yes",
                "-o",
                "LogLevel=ERROR",
                "-O",
                operation,
            ]);
        if let Some(forward) = forward {
            command.arg("-L").arg(forward);
        }
        command.arg(self.link.target());
        command
    }

    fn control(&self, operation: &str, forward: Option<&str>) -> Result<Output, ControlError> {
        self.link
            .bounded_output(self.control_command(operation, forward), CONTROL_TIMEOUT)
            .map_err(|error| match error.kind() {
                io::ErrorKind::TimedOut => ControlError::TimedOut,
                _ => ControlError::Failed(error.to_string()),
            })
    }

    /// The pid of the master on the control path.
    pub(crate) fn check(&self) -> Result<u32, ControlError> {
        parse_check(&self.control("check", None)?)
    }

    /// Adds a forward from a fresh socket path. The remote side dials only
    /// when a connection comes, so a wrong destination shows nothing here.
    pub(crate) fn forward(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
        parse_silent(&self.control("forward", Some(&forward_spec(socket, to)))?)
    }

    /// Takes a forward away. Its open connections go on, and the socket file
    /// stays: the caller unlinks it.
    pub(crate) fn cancel(&self, socket: &Path, to: &Destination) -> Result<(), ControlError> {
        parse_silent(&self.control("cancel", Some(&forward_spec(socket, to)))?)
    }

    fn request_exit(&self) -> Result<(), ControlError> {
        parse_exit(&self.control("exit", None)?)
    }

    /// Ends the master and every forward and connection on it.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.failed = None;
        if let Some(mut running) = state.running.take() {
            if !running.child.exited() && self.request_exit().is_ok() {
                running.child.wait(MASTER_EXIT_TIMEOUT);
            }
        }
    }
}

fn forward_spec(socket: &Path, to: &Destination) -> String {
    format!("{}:{to}", socket.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Write as _};
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::ExitStatusExt as _;

    const PROFILE: &str = "0123456789abcdef0123456789abcdef";
    const TARGET: &str = "link-test-host";

    /// The remote machine and its ssh, both in a temporary directory: `ssh`
    /// writes its arguments down, runs remote commands here with the fake
    /// remote `herdr` on PATH, and answers control commands from a state file.
    const SSH: &str = r##"#!/bin/sh
root=@ROOT@
{ for arg do printf '%s\n' "$arg"; done; printf '%s\n' '--'; } >> "$root/ssh-args"
control=; previous=; operation=; forward=; master=; configuration=
for arg do
    if [ "$previous" = -S ]; then control=$arg; fi
    if [ "$previous" = -O ]; then operation=$arg; fi
    if [ "$previous" = -L ]; then forward=$arg; fi
    if [ "$arg" = ControlMaster=yes ]; then master=yes; fi
    if [ "$arg" = -G ]; then configuration=yes; fi
    previous=$arg; last=$arg
done
if [ -n "$configuration" ]; then printf 'controlpath %s\n' "$control"; exit 0; fi
case "$operation" in
    '') ;;
    check) cat "$root/check" 2>/dev/null && exit 0
        printf 'Control socket connect(%s): No such file or directory\r\n' "$control" >&2; exit 255 ;;
    exit) rm -f "$root/check"; printf 'Exit request sent.\r\n' >&2; : > "$root/exited"; exit 0 ;;
    *) cat "$root/$operation-text" >&2 2>/dev/null; exit 0 ;;
esac
if [ -n "$master" ]; then
    if [ -f "$root/refuse" ]; then cat "$root/refuse" >&2; exit 255; fi
    python3 -c 'import socket, sys
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])' "$control"
    ulimit -S -n > "$root/limit"
    printf 'Master running (pid=%s)\r\n' "$$" > "$root/check"
    cat
    rm -f "$root/check" "$control"
    : > "$root/ended"
    exit 0
fi
if [ -f "$root/silent" ]; then exec sleep 30; fi
export PATH="$root/remote:/usr/bin:/bin"
printf 'a login banner'
exec /bin/sh -c "$last"
"##;

    const HERDR: &str = r##"#!/bin/sh
root=@ROOT@
printf '%s\n' "$*" >> "$root/herdr-args"
case "$*" in
    *--check) printf 'herdr-api-bridge-v1\n' ;;
    *remote-api-bridge) exec cat ;;
esac
"##;

    /// Holds the environment lock: the helper's ssh reads HOME, PATH and the
    /// XDG directories.
    struct Rig {
        root: PathBuf,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        removed: Vec<(std::ffi::OsString, std::ffi::OsString)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Rig {
        fn new() -> Self {
            let lock = crate::config::test_config_env_lock()
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Short: its socket paths have to fit.
            let root = PathBuf::from("/tmp").join(format!(
                "hbl-{}-{}",
                std::process::id(),
                crate::browser::session::random_hex(6)
            ));
            let mut rig = Self {
                root: root.clone(),
                saved: Vec::new(),
                removed: Vec::new(),
                _lock: lock,
            };
            for dir in ["home", "bin", "remote", "sockets"] {
                std::fs::create_dir_all(root.join(dir)).unwrap();
            }
            for (path, text) in [("bin/ssh", SSH), ("remote/herdr", HERDR)] {
                let path = root.join(path);
                std::fs::write(&path, text.replace("@ROOT@", root.to_str().unwrap())).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let path = std::env::var("PATH").unwrap_or_default();
            rig.set("PATH", format!("{}:{path}", root.join("bin").display()));
            rig.set("HOME", root.join("home").display().to_string());
            for name in ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"] {
                rig.set(name, root.join("home").join(name).display().to_string());
            }
            for (name, value) in std::env::vars_os() {
                if name.to_string_lossy().starts_with("HERDR_") {
                    rig.removed.push((name.clone(), value));
                    std::env::remove_var(name);
                }
            }
            rig
        }

        fn set(&mut self, name: &'static str, value: String) {
            self.saved.push((name, std::env::var_os(name)));
            std::env::set_var(name, value);
        }

        fn link(&self) -> Arc<SavedSshLink> {
            machine_ssh(PROFILE, TARGET, "work", Arc::new(AtomicBool::new(false))).unwrap()
        }

        fn master(&self) -> BrowserMaster {
            BrowserMaster::new(
                self.link(),
                &self.root.join("sockets"),
                "0011223344556677",
                0,
            )
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.root.join(name)).unwrap_or_default()
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.root.join(name), text).unwrap();
        }

        /// The argument vectors ssh ran with.
        fn calls(&self) -> Vec<Vec<String>> {
            let mut calls = vec![Vec::new()];
            for line in self.read("ssh-args").lines() {
                if line == "--" {
                    calls.push(Vec::new());
                } else if let Some(call) = calls.last_mut() {
                    call.push(line.to_owned());
                }
            }
            calls.pop();
            calls
        }

        fn metadata(&self) -> crate::client::endpoint::SshMetadataCache {
            crate::client::endpoint::SshMetadataCache::new(PROFILE, TARGET, "work").unwrap()
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            for (name, value) in self.removed.drain(..) {
                std::env::set_var(name, value);
            }
            while let Some((name, value)) = self.saved.pop() {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn output(code: i32, stderr: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn destinations_have_one_spelling_and_no_other_reaches_ssh() {
        for (host, port, printed) in [
            ("localhost", 3000, "localhost:3000"),
            ("LocalHost", 80, "localhost:80"),
            ("127.0.0.1", 1, "127.0.0.1:1"),
            ("a", 65535, "a:65535"),
            ("_dmarc.x_y-z.example", 443, "_dmarc.x_y-z.example:443"),
            ("localhost.", 80, "localhost:80"),
            ("[::1]", 8080, "[::1]:8080"),
            ("[0:0::0001]", 8080, "[::1]:8080"),
            ("[FE80::1]", 1, "[fe80::1]:1"),
        ] {
            let to = Destination::new(host, port).unwrap_or_else(|| panic!("{host}"));
            assert_eq!(to.to_string(), printed);
            assert_eq!((to.port(), to.master() < MASTERS), (port, true));
        }
        let long = "a".repeat(254);
        for host in [
            "",
            "-oProxyCommand=x",
            "-l",
            "a-",
            ".a",
            "a..",
            "localhost..",
            ".",
            "/tmp/x",
            "a/b",
            "a b",
            "a:b",
            "a%h",
            "~a",
            "${x}",
            "a\nb",
            "a\0b",
            "é.example",
            "[::1",
            "::1",
            "[fe80::1%en0]",
            "[]",
            "[1.2.3.4]",
            "[::1]x",
            "a@b",
            "*",
            &long,
        ] {
            assert_eq!(Destination::new(host, 80), None, "{host:?}");
        }
        assert_eq!(Destination::new("localhost", 0), None);
        assert!(Destination::new(&"a".repeat(253), 1).is_some());
        // Every process sends a host to the same master, and both get some.
        let masters = (1..200)
            .map(|port| Destination::new("localhost", port).unwrap().master())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(masters, (0..MASTERS).collect());
    }

    #[test]
    fn control_answers_are_judged_by_their_text() {
        // Texts of OpenSSH 10.2 as measured (spike S7 and its rig).
        let gone = "Control socket connect(/tmp/x): No such file or directory\r\n";
        let full = "muxclient: master hello exchange failed\r\nConnection closed by UNKNOWN port 65535\r\n";
        let full_session = "mux_client_request_session: read from master failed: Broken pipe\r\nConnection closed by UNKNOWN port 65535\r\n";
        let refused = "mux_client_forward: forwarding request failed: Port forwarding failed\r\nmuxclient: master forward request failed\r\n";
        let missing = "mux_client_forward: forwarding request failed: port not forwarded\r\nmuxclient: master cancel forward request failed\r\n";
        assert_eq!(
            parse_check(&output(0, "Master running (pid=89588)\r\n")),
            Ok(89588)
        );
        assert!(matches!(
            parse_check(&output(255, gone)),
            Err(ControlError::Gone(_))
        ));
        assert!(matches!(
            parse_check(&output(255, full)),
            Err(ControlError::Full(_))
        ));
        assert!(matches!(
            parse_check(&output(0, "")),
            Err(ControlError::Failed(_))
        ));
        assert!(matches!(
            parse_check(&output(0, "Master running (pid=x)")),
            Err(ControlError::Failed(_))
        ));
        assert_eq!(parse_silent(&output(0, "")), Ok(()));
        // A failed cancel exits 0.
        assert_eq!(
            parse_silent(&output(0, missing)),
            Err(ControlError::NotForwarded)
        );
        assert!(matches!(
            parse_silent(&output(255, refused)),
            Err(ControlError::Failed(_))
        ));
        assert!(matches!(
            parse_silent(&output(255, full_session)),
            Err(ControlError::Full(_))
        ));
        assert!(matches!(
            parse_silent(&output(0, "something new")),
            Err(ControlError::Failed(_))
        ));
        assert!(matches!(
            parse_silent(&output(1, "")),
            Err(ControlError::Failed(_))
        ));
        assert_eq!(parse_exit(&output(0, "Exit request sent.\r\n")), Ok(()));
        assert!(matches!(
            parse_exit(&output(255, gone)),
            Err(ControlError::Gone(_))
        ));
    }

    #[test]
    fn keys_name_one_home_target_and_session() {
        let key = machine_key(Path::new("/state"), "host", "default");
        assert_eq!(key.len(), 16);
        assert!(key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(key, machine_key(Path::new("/state"), "host", "default"));
        for other in [
            machine_key(Path::new("/other"), "host", "default"),
            machine_key(Path::new("/state"), "other", "default"),
            machine_key(Path::new("/state"), "host", "work"),
            machine_key(Path::new("/state"), "hostd", "efault"),
        ] {
            assert_ne!(key, other);
        }
    }

    #[test]
    fn the_clock_advances() {
        let first = clock_ns().unwrap();
        std::thread::sleep(Duration::from_millis(2));
        assert!(clock_ns().unwrap() > first);
    }

    #[test]
    fn the_link_child_strips_the_preamble_and_shares_the_clients_connection() {
        let rig = Rig::new();
        let link = rig.link();
        let mut child = open_link(&link).unwrap();
        child.writer.write_all(b"{\"hello\":1}\nnext\n").unwrap();
        child.writer.flush().unwrap();
        let mut line = String::new();
        child.reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"hello\":1}\n");
        line.clear();
        child.reader.read_line(&mut line).unwrap();
        assert_eq!(line, "next\n");
        let end = end_link(child.writer, child.process);
        assert_eq!(end.code, Some(0), "{end:?}");
        line.clear();
        assert_eq!(child.reader.read_line(&mut line).unwrap(), 0);

        // The same control path, options and target as the client's own ssh.
        let calls = rig.calls();
        let call = calls.last().unwrap();
        let client =
            crate::platform::shared_ssh_control_path(&crate::config::config_path(), TARGET)
                .unwrap();
        let at = call.iter().position(|arg| arg == "-S").unwrap();
        assert_eq!(Path::new(&call[at + 1]), client);
        for wanted in [
            "-C",
            "-F",
            "ControlMaster=auto",
            "ControlPersist=600",
            "BatchMode=yes",
        ] {
            assert!(call.iter().any(|arg| arg == wanted), "{wanted}: {call:?}");
        }
        assert_eq!(&call[call.len() - 3..call.len() - 1], ["-T", TARGET]);
        assert!(!call.iter().any(|arg| arg.contains("ProxyCommand")));

        // The second one takes the cached binary: one ssh, no discovery.
        let before = calls.len();
        let child = open_link(&link).unwrap();
        assert_eq!(rig.calls().len(), before + 1);
        end_link(child.writer, child.process);
    }

    #[test]
    fn stale_metadata_is_discovered_again_once() {
        let rig = Rig::new();
        let link = rig.link();
        let stale = crate::client::endpoint::SshMachineMetadata {
            os: "linux".into(),
            executable: "/nonexistent/herdr".into(),
        };
        rig.metadata().store(&stale);
        let mut child = open_link(&link).unwrap();
        child.writer.write_all(b"ok\n").unwrap();
        let mut line = String::new();
        child.reader.read_line(&mut line).unwrap();
        assert_eq!(line, "ok\n");
        end_link(child.writer, child.process);
        let found = rig.metadata().load().unwrap().executable;
        assert_eq!(Path::new(&found), rig.root.join("remote/herdr"));

        // A machine whose discovered binary does not serve either: one
        // rediscovery, then the error.
        rig.metadata().store(&stale);
        rig.write(
            "remote/herdr",
            &format!(
                "#!/bin/sh\nn=$(cat {0}/n 2>/dev/null); echo x$n > {0}/n\n[ \"$n\" ] || echo herdr-api-bridge-v1\n",
                rig.root.display()
            ),
        );
        let before = rig.calls().len();
        let error = open_link(&link).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("herdr-machine-metadata-stale-v1"),
            "{error}"
        );
        let bridges = rig.calls()[before..]
            .iter()
            .filter(|call| call.last().unwrap().contains("base64"))
            .count();
        assert_eq!(bridges, 2);
    }

    #[test]
    fn a_silent_remote_side_ends_in_time() {
        let rig = Rig::new();
        rig.metadata()
            .store(&crate::client::endpoint::SshMachineMetadata {
                os: "linux".into(),
                executable: rig.root.join("remote/herdr").display().to_string(),
            });
        rig.write("silent", "");
        let started = Instant::now();
        let error = open_link_within(&rig.link(), Duration::from_millis(300))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_master_is_its_own_connection_and_ends_with_its_input() {
        let rig = Rig::new();
        let master = rig.master();
        assert_eq!(master.running(), None);
        let info = master.ensure().unwrap();
        assert_eq!(master.ensure().unwrap(), info);
        assert_eq!(master.running(), Some(info));
        assert_eq!(master.check(), Ok(info.pid));
        assert!(info.file_limit >= MASTER_FILE_LIMIT, "{info:?}");
        assert_eq!(rig.read("limit").trim(), info.file_limit.to_string());

        let calls = rig.calls();
        let starts = calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == "ControlMaster=yes"))
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 1);
        let start = starts[0];
        let at = start.iter().position(|arg| arg == "-S").unwrap();
        assert_eq!(Path::new(&start[at + 1]), master.control_path());
        let client =
            crate::platform::shared_ssh_control_path(&crate::config::config_path(), TARGET)
                .unwrap();
        assert_ne!(client, master.control_path());
        assert_eq!(&start[start.len() - 3..], ["-T", TARGET, "cat"]);
        let index = |wanted: &str| start.iter().position(|arg| arg == wanted);
        for option in MASTER_OPTIONS {
            assert!(index(option).is_some(), "{option}");
        }
        // The first value wins in OpenSSH.
        assert!(index("ServerAliveInterval=5") < index("ServerAliveInterval=15"));
        assert!(index("BatchMode=yes").is_some() && index("StrictHostKeyChecking=yes").is_some());

        drop(master);
        until("the master ends with its input", || {
            rig.root.join("ended").exists()
        });
        assert!(!rig.root.join("exited").exists());
    }

    #[test]
    fn control_commands_never_log_in() {
        let rig = Rig::new();
        let master = rig.master();
        master.ensure().unwrap();
        let before = rig.calls().len();
        let socket = rig.root.join("sockets/0123");
        let to = Destination::new("[::1]", 3000).unwrap();
        assert_eq!(master.forward(&socket, &to), Ok(()));
        rig.write(
            "cancel-text",
            "mux_client_forward: forwarding request failed: port not forwarded\r\n",
        );
        assert_eq!(master.cancel(&socket, &to), Err(ControlError::NotForwarded));
        master.check().unwrap();
        master.close();
        assert_eq!(master.running(), None);
        assert!(rig.root.join("exited").exists());

        let control = master.control_path().display().to_string();
        let spec = format!("{}:[::1]:3000", socket.display());
        let base = |operation: &str, forward: Option<&str>| {
            let mut argv = vec!["-F", "/dev/null", "-S", &control];
            for option in [
                "ControlMaster=no",
                "ControlPersist=no",
                "ProxyCommand=/usr/bin/false",
                "BatchMode=yes",
                "LogLevel=ERROR",
            ] {
                argv.extend(["-o", option]);
            }
            argv.extend(["-O", operation]);
            if let Some(forward) = forward {
                argv.extend(["-L", forward]);
            }
            argv.push(TARGET);
            argv.into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        assert_eq!(
            rig.calls()[before..],
            [
                base("forward", Some(&spec)),
                base("cancel", Some(&spec)),
                base("check", None),
                base("exit", None),
            ]
        );
    }

    #[test]
    fn a_failed_master_is_reported_and_not_retried_at_once() {
        let rig = Rig::new();
        rig.write("refuse", "Permission denied (publickey).\r\n");
        let master = rig.master();
        let error = master.ensure().unwrap_err();
        assert_eq!(
            error,
            MasterError::Failed("Permission denied (publickey).".into())
        );
        let before = rig.calls().len();
        assert_eq!(master.ensure().unwrap_err(), error);
        assert_eq!(rig.calls().len(), before);
        assert!(matches!(master.check(), Err(ControlError::Gone(_))));
    }

    #[test]
    fn a_cleaned_temporary_directory_costs_nothing() {
        let rig = Rig::new();
        let link = rig.link();
        end_link_of(&link);
        let config = |call: &[String]| {
            let at = call.iter().position(|arg| arg == "-F").unwrap();
            PathBuf::from(&call[at + 1])
        };
        let first = config(rig.calls().last().unwrap());
        std::fs::remove_dir_all(first.parent().unwrap()).unwrap();
        end_link_of(&link);
        assert!(config(rig.calls().last().unwrap()).is_file());
        BrowserMaster::new(link, &rig.root.join("sockets"), "77", 0)
            .ensure()
            .unwrap();
    }

    fn end_link_of(link: &SavedSshLink) {
        let child = open_link(link).unwrap();
        end_link(child.writer, child.process);
    }

    #[test]
    fn a_dead_helpers_master_is_ended_and_a_foreign_file_is_not() {
        let rig = Rig::new();
        let master = rig.master();
        drop(UnixListener::bind(master.control_path()).unwrap());
        master.ensure().unwrap();
        assert!(rig.root.join("exited").exists());
        drop(master);

        let master = BrowserMaster::new(rig.link(), &rig.root.join("sockets"), "77", 1);
        std::fs::write(master.control_path(), b"not a socket").unwrap();
        let error = master.ensure().unwrap_err();
        assert!(error.to_string().contains("not a socket"), "{error}");
        assert!(master.control_path().is_file());
    }

    #[test]
    fn without_managed_ssh_there_is_no_master() {
        let rig = Rig::new();
        let config = crate::config::config_path();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[remote]\nmanage_ssh_config = false\n").unwrap();
        let error = rig.master().ensure().unwrap_err();
        assert_eq!(error, MasterError::Unmanaged);
        assert!(rig.calls().is_empty());
    }

    /// Against a real OpenSSH: `HERDR_SSH_FIXTURE_HOME` is a home whose
    /// `.ssh/config` names a private sshd as `spike`, in a temporary directory.
    #[test]
    #[ignore = "requires a private loopback OpenSSH fixture"]
    fn a_real_master_fixture() {
        use std::io::Read as _;

        let home = std::env::var("HERDR_SSH_FIXTURE_HOME").expect("fixture home");
        let mut rig = Rig::new();
        std::fs::remove_file(rig.root.join("bin/ssh")).unwrap();
        rig.set("HOME", home);
        let link = machine_ssh(PROFILE, "spike", "work", Arc::new(AtomicBool::new(false))).unwrap();
        let dir = rig.root.join("sockets");
        let master = BrowserMaster::new(Arc::clone(&link), &dir, "0011223344556677", 0);
        let info = master.ensure().unwrap();
        assert_eq!(master.check(), Ok(info.pid));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let to = Destination::new("127.0.0.1", listener.local_addr().unwrap().port()).unwrap();
        let socket = dir.join("001122330011223300112233");
        master.forward(&socket, &to).unwrap();
        let mode = std::fs::symlink_metadata(&socket)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "the user's own forwards stay out"
        );
        let mut near = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        let (mut far, _) = listener.accept().unwrap();
        far.write_all(b"hello").unwrap();
        let mut got = [0; 5];
        near.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello");
        master.cancel(&socket, &to).unwrap();
        assert_eq!(master.cancel(&socket, &to), Err(ControlError::NotForwarded));
        far.write_all(b"again").unwrap();
        near.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"again", "a cancelled forward keeps its connection");
        let control = master.control_path().to_owned();
        // Another helper's master on the same path takes over.
        let second = BrowserMaster::new(link, &dir, "0011223344556677", 0);
        assert_ne!(second.ensure().unwrap().pid, info.pid);
        assert_eq!(master.running(), None);
        second.close();
        assert!(!control.exists());
        assert!(matches!(second.check(), Err(ControlError::Gone(_))));
    }

    /// The link child, and pages through the proxy, the forwards and two
    /// masters, against a real OpenSSH.
    #[test]
    #[ignore = "requires a private loopback OpenSSH fixture"]
    fn a_real_link_and_pages_fixture() {
        use super::super::{forward, proxy};
        use std::io::Read as _;

        struct Remote;
        impl proxy::Classifier for Remote {
            fn canonical(&self, host: &str) -> Result<String, String> {
                Ok(host.to_owned())
            }
            fn route(&self, _host: &str, _port: u16) -> proxy::Route {
                proxy::Route::Remote
            }
        }

        let home = std::env::var("HERDR_SSH_FIXTURE_HOME").expect("fixture home");
        let mut rig = Rig::new();
        std::fs::remove_file(rig.root.join("bin/ssh")).unwrap();
        rig.set("HOME", home.clone());
        let link = machine_ssh(PROFILE, "spike", "work", Arc::new(AtomicBool::new(false))).unwrap();
        let cache = crate::client::endpoint::SshMetadataCache::new(PROFILE, "spike", "work");
        cache
            .unwrap()
            .store(&crate::client::endpoint::SshMachineMetadata {
                os: "macos".into(),
                executable: rig.root.join("remote/herdr").display().to_string(),
            });
        let mut child = open_link(&link).unwrap();
        child.writer.write_all(b"over ssh\n").unwrap();
        let mut line = String::new();
        child.reader.read_line(&mut line).unwrap();
        assert_eq!(line, "over ssh\n");
        assert_eq!(end_link(child.writer, child.process).code, Some(0));
        // The connection the link made is shared and stays: end this one.
        let shared =
            crate::platform::shared_ssh_control_path(&crate::config::config_path(), "spike");
        let config = Path::new(&home).join(".ssh/config");
        let ssh = |arguments: &[&str]| {
            let output = Command::new("ssh")
                .arg("-F")
                .arg(&config)
                .arg("-S")
                .arg(shared.as_ref().unwrap())
                .args(arguments)
                .arg("spike")
                .stdin(Stdio::null())
                .output()
                .unwrap();
            output_text(&output)
        };
        assert_eq!(ssh(&["-O", "exit"]), "Exit request sent.");

        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in server.incoming() {
                let mut stream = stream.unwrap();
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
                stream.write_all(&head).unwrap();
            }
        });
        let dir = rig.root.join("sockets");
        let key = "0011223344556677";
        let masters = (0..MASTERS)
            .map(|index| {
                let master = BrowserMaster::new(Arc::clone(&link), &dir, key, index);
                Arc::new(master) as Arc<dyn forward::Master>
            })
            .collect();
        let forwards =
            Arc::new(forward::Forwards::new(&dir, &rig.root.join("home"), key, masters).unwrap());
        forwards.set_link(true);
        let proxy = proxy::Proxy::start(
            0,
            proxy::ProxyOptions {
                label: "spike".into(),
                instance: "i".into(),
                secret: "s".into(),
                remote_only: false,
                classifier: Arc::new(Remote),
                dial: Arc::clone(&forwards) as _,
            },
        )
        .unwrap();
        let get = |host: &str, port: u16| {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
            let request = format!(
                "GET http://{host}:{port}/x HTTP/1.1\r\nProxy-Authorization: Basic aGVyZHI6cw==\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        };
        for host in ["localhost", "127.0.0.1", "LOCALHOST."] {
            let answer = get(host, port);
            assert!(
                answer.contains("\r\n\r\nGET /x HTTP/1.1\r\n"),
                "{host}: {answer}"
            );
        }
        // Enough hosts for both masters and for evictions.
        let started = Instant::now();
        for nothing in 200..280_u16 {
            let answer = get("127.0.0.1", nothing);
            assert!(answer.contains("did not connect to 127.0.0.1:"), "{answer}");
        }
        eprintln!("80 refused dials in {:?}", started.elapsed());
        let sockets = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(sockets, 64 + MASTERS);
        assert!(get("localhost", port).contains("\r\n\r\nGET /x HTTP/1.1\r\n"));
        drop(proxy);
        forwards.close();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    /// Fork (remote browser): the tests' own socket directory is made 0700
    /// and refused when it is relative, open to others or a symlink.
    #[test]
    fn the_tests_socket_dir_is_private_or_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = super::super::api_socket::tests::short_dir();
        let dir = root.path().join("s");
        assert_eq!(private_dir(&dir).unwrap(), dir);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o700);
        assert!(private_dir(Path::new("relative/dir")).is_err());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&dir).is_err());
        let link = root.path().join("l");
        std::os::unix::fs::symlink(root.path(), &link).unwrap();
        assert!(private_dir(&link).is_err());
    }
}
