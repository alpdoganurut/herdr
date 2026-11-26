use std::io;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::attach::{find_installed_remote_herdr, RemoteSsh, SshStdioBridge};
#[cfg(unix)]
use super::process::{CancellablePipe, OwnedSshChild, PipeControl};

pub(crate) struct SavedSshBridge {
    _bridge: SshStdioBridge,
}

pub(crate) struct SavedSshStream {
    pub(crate) stream: crate::ipc::LocalStream,
    pub(crate) bridge: SavedSshBridge,
}

pub(crate) fn connect_saved_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
    cancelled: Arc<AtomicBool>,
) -> io::Result<SavedSshStream> {
    let mut ssh = validated_saved_ssh(profile_id, target, session)?;
    ssh.cancelled = Arc::clone(&cancelled);
    ssh.prepare_shared_transport()?;
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    let metadata = remote_herdr.machine_metadata();
    if cancelled.load(Ordering::Acquire) {
        return Err(super::process::cancelled());
    }
    let path = saved_bridge_path(profile_id);
    let bridge = SshStdioBridge::start(
        target.to_owned(),
        remote_herdr,
        path.clone(),
        session.to_owned(),
        ssh.options(),
        true,
        Arc::clone(&cancelled),
    )?;
    let stream = crate::ipc::connect_local_stream_cancellable(
        &path,
        &cancelled,
        Instant::now() + Duration::from_secs(5),
    )?;
    if let Some(metadata) = metadata {
        crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?
            .store(&metadata);
    }
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge { _bridge: bridge },
    })
}

pub(crate) struct SavedSshApiBridge {
    path: PathBuf,
    bridge: SshStdioBridge,
    metadata_cache: crate::client::endpoint::SshMetadataCache,
    ssh: RemoteSsh,
    pub(crate) can_retry: bool,
    pub(crate) used_cached_metadata: bool,
}

impl SavedSshApiBridge {
    pub(crate) fn start(
        profile_id: &str,
        target: &str,
        session: &str,
        use_cached_metadata: bool,
        allow_retry: bool,
    ) -> io::Result<Self> {
        let ssh = validated_saved_ssh(profile_id, target, session)?;
        let mut can_retry = allow_retry;
        let metadata_cache =
            crate::client::endpoint::SshMetadataCache::new(profile_id, target, session)?;
        let cached = use_cached_metadata.then(|| metadata_cache.load()).flatten();
        let used_cached_metadata = cached.is_some();
        let metadata = match cached {
            Some(metadata) => metadata,
            None => {
                let discover = || super::attach::discover_remote_api_metadata(&ssh, session);
                let metadata = match discover() {
                    Err(_) if can_retry && ssh.prepare_shared_transport().unwrap_or(false) => {
                        can_retry = false;
                        discover()?
                    }
                    result => result?,
                };
                metadata_cache.store(&metadata);
                metadata
            }
        };
        let command = super::attach::cached_remote_api_command(&metadata, session);
        let path = crate::platform::remote_bridge_endpoint_path(
            &format!("herdr-api-ssh-{}-{profile_id}.sock", std::process::id()),
            &format!(
                "herdr-api-{}-{}.sock",
                std::process::id(),
                &profile_id[..16]
            ),
        );
        let bridge = SshStdioBridge::start_command(
            target.to_owned(),
            command,
            path.clone(),
            ssh.options(),
            true,
            Arc::new(AtomicBool::new(false)),
        )?;
        Ok(Self {
            path,
            bridge,
            metadata_cache,
            ssh,
            can_retry,
            used_cached_metadata,
        })
    }

    pub(crate) fn socket_path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn reported_failure(&self) -> Option<io::Error> {
        self.bridge.reported_failure()
    }

    pub(crate) fn invalidate_metadata(&self) {
        self.metadata_cache.invalidate();
    }

    pub(crate) fn recover_transport(&self) -> bool {
        self.can_retry && self.ssh.prepare_shared_transport().unwrap_or(false)
    }

    pub(crate) fn stale_metadata_failure(error: &io::Error) -> bool {
        error
            .to_string()
            .contains(super::attach::STALE_API_METADATA)
    }
}

/// Fork: ssh as the browser link's helper uses it: the link's own child on
/// the client's shared connection, and the options for the browser's own
/// connections (`crate::client::browser_link::ssh`).
#[cfg(unix)]
pub(crate) struct SavedSshLink {
    target: String,
    session: String,
    /// The last one is in force. One whose configuration is lost stays: the
    /// next one may have the same directory, and its end would remove it.
    ssh: Mutex<Vec<Arc<RemoteSsh>>>,
    metadata_cache: crate::client::endpoint::SshMetadataCache,
    cancelled: Arc<AtomicBool>,
}

#[cfg(unix)]
impl SavedSshLink {
    pub(crate) fn new(
        profile_id: &str,
        target: &str,
        session: &str,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        super::validate_remote_target(target)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut ssh = validated_saved_ssh(profile_id, target, session)?;
        ssh.cancelled = Arc::clone(&cancelled);
        Ok(Self {
            target: target.to_owned(),
            session: session.to_owned(),
            ssh: Mutex::new(vec![Arc::new(ssh)]),
            metadata_cache: crate::client::endpoint::SshMetadataCache::new(
                profile_id, target, session,
            )?,
            cancelled,
        })
    }

    /// The options in force, written again when the helper outlived them.
    fn ssh(&self) -> Arc<RemoteSsh> {
        let mut all = self.ssh.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ssh) = all.last().filter(|ssh| !ssh.configuration_lost()) {
            return Arc::clone(ssh);
        }
        let mut ssh = RemoteSsh::new_noninteractive(self.target.clone());
        ssh.cancelled = Arc::clone(&self.cancelled);
        let ssh = Arc::new(ssh);
        all.push(Arc::clone(&ssh));
        ssh
    }

    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// The link child's command and whether it came from cached metadata.
    pub(crate) fn api_command(&self, use_cached: bool) -> io::Result<(Command, bool)> {
        let cached = use_cached.then(|| self.metadata_cache.load()).flatten();
        let used_cached = cached.is_some();
        let metadata = match cached {
            Some(metadata) => metadata,
            None => {
                let ssh = self.ssh();
                let discover = || super::attach::discover_remote_api_metadata(&ssh, &self.session);
                let metadata = match discover() {
                    Err(_) if self.recover_transport() => discover()?,
                    result => result?,
                };
                self.metadata_cache.store(&metadata);
                metadata
            }
        };
        let mut command = self.ssh().command();
        command.arg(super::attach::cached_remote_api_command(
            &metadata,
            &self.session,
        ));
        Ok((command, used_cached))
    }

    pub(crate) fn invalidate_metadata(&self) {
        self.metadata_cache.invalidate();
    }

    pub(crate) fn recover_transport(&self) -> bool {
        self.ssh().prepare_shared_transport().unwrap_or(false)
    }

    /// The managed options without any control path, or `None` when herdr
    /// does not manage the ssh configuration.
    pub(crate) fn unshared_command(&self, first: &[&str]) -> Option<Command> {
        self.ssh().unshared_command(first)
    }

    /// Runs a command with stdin closed and its output bounded; one that does
    /// not end in time is given up on.
    pub(crate) fn bounded_output(
        &self,
        command: Command,
        timeout: Duration,
    ) -> io::Result<std::process::Output> {
        self.ssh().bounded_control_output(command, timeout)
    }

    /// Starts a link child and returns once its ready marker was read: what
    /// the reader gives is the remote bridge's output alone.
    pub(crate) fn connect(
        &self,
        mut command: Command,
        timeout: Duration,
    ) -> Result<SavedSshLinkChild, SavedSshLinkFailure> {
        use crate::platform::ChildPipe;
        use std::process::Stdio;

        let mut child = OwnedSshChild(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let control = PipeControl::new(Arc::clone(&self.cancelled));
        let missing = || io::Error::other("SSH link pipe was not captured");
        let stdin = child.0.stdin.take().ok_or_else(missing)?;
        let stdout = child.0.stdout.take().ok_or_else(missing)?;
        let stderr = child.0.stderr.take().ok_or_else(missing)?;
        let writer = CancellablePipe::new(ChildPipe::new_writer(stdin)?, &control);
        let mut reader = io::BufReader::new(CancellablePipe::new(
            ChildPipe::new_reader(stdout)?,
            &control,
        ));
        let stderr = CancellablePipe::new(ChildPipe::new_reader(stderr)?, &control);
        let stderr = std::thread::Builder::new()
            .name("browser-link-ssh-err".into())
            .spawn(move || super::attach::capture_ssh_stderr(stderr))?;
        let process = SavedSshLinkProcess {
            child,
            control: LinkControl(control),
            stderr,
        };
        let (ready, waiting) = std::sync::mpsc::channel::<()>();
        let watched = Arc::clone(&process.control.0);
        std::thread::Builder::new()
            .name("browser-link-ssh-wait".into())
            .spawn(move || {
                let waited = waiting.recv_timeout(timeout);
                if waited == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                    watched.stop();
                }
            })?;
        let started = Instant::now();
        let preamble = super::attach::discard_remote_output_preamble(&mut reader);
        drop(ready);
        match preamble {
            Ok(()) => Ok(SavedSshLinkChild {
                reader: Box::new(reader),
                writer: Box::new(writer),
                process,
            }),
            Err(error) => {
                drop(writer);
                let timed_out = started.elapsed() >= timeout;
                let end = process.finish(Duration::from_secs(if timed_out { 0 } else { 2 }));
                Err(SavedSshLinkFailure {
                    stale: end.code == Some(78)
                        || end.stderr.contains(super::attach::STALE_API_METADATA),
                    error: match (timed_out, end.stderr.is_empty()) {
                        (true, _) => io::Error::new(
                            io::ErrorKind::TimedOut,
                            "the remote command did not start in time",
                        ),
                        (false, true) => error,
                        (false, false) => io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            format!("remote SSH connection failed: {}", end.stderr),
                        ),
                    },
                })
            }
        }
    }
}

#[cfg(unix)]
pub(crate) struct SavedSshLinkChild {
    pub(crate) reader: Box<dyn io::BufRead + Send>,
    /// Dropping it is the remote bridge's end of input: it exits.
    pub(crate) writer: Box<dyn io::Write + Send>,
    pub(crate) process: SavedSshLinkProcess,
}

/// The ssh process behind a link child. Dropping it ends this invocation
/// only, never the shared connection.
#[cfg(unix)]
pub(crate) struct SavedSshLinkProcess {
    // Dropped first: then nothing writes to the pipes the control ends.
    child: OwnedSshChild,
    control: LinkControl,
    stderr: std::thread::JoinHandle<io::Result<Vec<u8>>>,
}

/// Ends the pipes' reads once the child is gone, even when a process it left
/// behind holds them open; what is in them is read first.
#[cfg(unix)]
struct LinkControl(Arc<PipeControl>);

#[cfg(unix)]
impl Drop for LinkControl {
    fn drop(&mut self) {
        self.0.finish_reads();
    }
}

/// How a link child ended: its exit code and what it said on stderr.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SavedSshLinkEnd {
    pub(crate) code: Option<i32>,
    pub(crate) stderr: String,
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct SavedSshLinkFailure {
    /// The cached metadata named a binary that is gone: discover again.
    pub(crate) stale: bool,
    pub(crate) error: io::Error,
}

#[cfg(unix)]
impl From<io::Error> for SavedSshLinkFailure {
    fn from(error: io::Error) -> Self {
        Self {
            stale: false,
            error,
        }
    }
}

#[cfg(unix)]
impl SavedSshLinkProcess {
    /// Waits for the child to exit on its own (the writer is dropped), at
    /// most `grace`; then a blocked read of the link ends too.
    pub(crate) fn finish(mut self, grace: Duration) -> SavedSshLinkEnd {
        let deadline = Instant::now() + grace;
        let mut code = None;
        loop {
            match self.child.0.try_wait() {
                Ok(Some(status)) => {
                    code = status.code();
                    // What it wrote last is still in the pipe.
                    self.control.0.finish_reads();
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    self.control.0.stop();
                    break;
                }
            }
        }
        drop(self.child);
        let stderr = match self.stderr.join() {
            Ok(Ok(stderr)) => String::from_utf8_lossy(&stderr).trim().to_owned(),
            Ok(Err(_)) | Err(_) => String::new(),
        };
        SavedSshLinkEnd { code, stderr }
    }
}

pub(crate) fn saved_ssh_bootstrap_command(target: &str, session: &str) -> String {
    format!(
        "herdr --remote {} --session {}",
        super::shell_quote(target),
        super::shell_quote(session)
    )
}

pub(crate) fn saved_ssh_failure_needs_attention(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::Unsupported
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "permission denied",
        "host key verification failed",
        "remote host identification has changed",
        "no matching host key",
        "unsupported remote platform",
        "not ready",
        "install or update",
        "protocol",
        "handshake",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn saved_bridge_path(profile_id: &str) -> PathBuf {
    let pid = std::process::id();
    let readable = format!("herdr-ssh-{pid}-{profile_id}.sock");
    let short = format!("herdr-s-{pid}-{}.sock", &profile_id[..16]);
    crate::platform::remote_bridge_endpoint_path(&readable, &short)
}

fn validated_saved_ssh(profile_id: &str, target: &str, session: &str) -> io::Result<RemoteSsh> {
    validate_profile_path_id(profile_id)?;
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    Ok(RemoteSsh::new_noninteractive(target.to_owned()))
}

fn validate_profile_path_id(profile_id: &str) -> io::Result<()> {
    if profile_id.len() == 32
        && profile_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH endpoint profile id",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_use_profile_identity_not_target_or_session() {
        let first = saved_bridge_path("0123456789abcdef0123456789abcdef");
        let second = saved_bridge_path("fedcba9876543210fedcba9876543210");
        assert_ne!(first, second);
        assert!(!first.to_string_lossy().contains("example.com"));
        assert!(!first.to_string_lossy().contains("default"));
    }

    #[test]
    fn bootstrap_command_preserves_the_explicit_remote_session() {
        assert_eq!(
            saved_ssh_bootstrap_command("build host", "agent work"),
            "herdr --remote 'build host' --session 'agent work'"
        );
    }

    #[test]
    fn prompt_and_compatibility_failures_require_attention() {
        for message in [
            "Permission denied (publickey)",
            "Host key verification failed",
            "matching Herdr is not ready; install or update",
            "handshake rejected",
        ] {
            assert!(saved_ssh_failure_needs_attention(&io::Error::other(
                message
            )));
        }
        assert!(!saved_ssh_failure_needs_attention(&io::Error::new(
            io::ErrorKind::TimedOut,
            "network timed out"
        )));
    }
}
