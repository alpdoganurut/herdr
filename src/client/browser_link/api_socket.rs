//! The helper's API socket (`<key>.api`, 0600): where the machine's companion
//! talks to herdr, as it does to a local herdr. One request per connection,
//! answered and closed, except `events.subscribe`, whose connection stays
//! open for the stream. What a method does is the [`ApiHandler`]'s.

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

/// The longest request line.
const REQUEST_MAX: usize = 1024 * 1024;
/// A connection that sends no request in this long is closed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A companion that does not read its answer for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// A failed accept (out of descriptors, say) is not retried before this.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// What a method answers.
pub(crate) enum Answer {
    /// `result`, with its `type`.
    Result(Value),
    Error {
        code: String,
        message: String,
    },
    /// The stream's first answer, then each line the receiver gives until it
    /// ends or the companion goes.
    Stream(Value, Receiver<String>),
}

impl Answer {
    pub(crate) fn error(code: &str, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.to_owned(),
            message: message.into(),
        }
    }

    pub(crate) fn ok() -> Self {
        Self::Result(json!({"type": "ok"}))
    }
}

/// The helper's side of the socket. Runs on the connection's thread: it may
/// block (a long poll, a call over the link).
pub(crate) trait ApiHandler: Send + Sync {
    fn call(&self, method: &str, params: Value) -> Answer;
}

/// A bound socket; dropping it stops accepting and removes the file.
pub(crate) struct ApiSocket {
    path: PathBuf,
    stop: Option<UnixStream>,
    accept: Option<JoinHandle<()>>,
}

impl ApiSocket {
    /// Binds `path` (0600); a leftover file of a dead helper is replaced, a
    /// live one is an error.
    pub(crate) fn bind(path: &Path, handler: Arc<dyn ApiHandler>) -> io::Result<Self> {
        let listener = bind_private(path)?;
        let (stop, stopped) = UnixStream::pair()?;
        let accept = std::thread::Builder::new()
            .name("browser-link-api".into())
            .spawn(move || {
                accept_loop(&listener, &stopped, move |stream| serve(stream, &handler))
            })?;
        Ok(Self {
            path: path.to_owned(),
            stop: Some(stop),
            accept: Some(accept),
        })
    }
}

impl Drop for ApiSocket {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A unix socket only this user reaches: bound under a 0177 umask-free
/// chmod, a dead helper's file replaced, a live helper's kept.
pub(crate) fn bind_private(path: &Path) -> io::Result<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is in use", path.display()),
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a socket", path.display()),
            ))
        }
        Ok(_) => std::fs::remove_file(path)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Accepts until `stopped` is readable (its other end dropped); each
/// connection on a thread of its own.
pub(crate) fn accept_loop(
    listener: &UnixListener,
    stopped: &UnixStream,
    serve: impl Fn(UnixStream) + Clone + Send + 'static,
) {
    accept_loop_with(
        listener,
        stopped,
        || listener.accept().map(|(stream, _)| stream),
        serve,
    );
}

fn accept_loop_with(
    listener: &UnixListener,
    stopped: &UnixStream,
    mut accept: impl FnMut() -> io::Result<UnixStream>,
    serve: impl Fn(UnixStream) + Clone + Send + 'static,
) {
    loop {
        let mut fds = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stopped.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid pollfd entries for the length given.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if fds[1].revents != 0 {
            return;
        }
        let stream = match accept() {
            Ok(stream) => stream,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
            Err(err) => {
                tracing::debug!(%err, "browser link socket accept failed");
                std::thread::sleep(ACCEPT_BACKOFF);
                continue;
            }
        };
        let serve = serve.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-link-conn".into())
            .stack_size(256 * 1024)
            .spawn(move || serve(stream));
        if let Err(err) = spawned {
            tracing::warn!(%err, "no thread for a browser link connection");
        }
    }
}

fn serve(stream: UnixStream, handler: &Arc<dyn ApiHandler>) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(REQUEST_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        return;
    }
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half).take(REQUEST_MAX as u64 + 1);
    let mut line = Vec::new();
    if reader.read_until(b'\n', &mut line).is_err() || line.len() > REQUEST_MAX {
        return;
    }
    let mut stream = stream;
    let request: Value = match serde_json::from_slice(&line) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_line(
                &mut stream,
                &error_line(&Value::Null, "invalid_request", "not a JSON request"),
            );
            return;
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        let _ = write_line(
            &mut stream,
            &error_line(&id, "invalid_request", "no method"),
        );
        return;
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    match handler.call(method, params) {
        Answer::Result(result) => {
            let _ = write_line(&mut stream, &result_line(&id, result));
        }
        Answer::Error { code, message } => {
            let _ = write_line(&mut stream, &error_line(&id, &code, &message));
        }
        Answer::Stream(first, lines) => {
            if write_line(&mut stream, &result_line(&id, first)).is_err() {
                return;
            }
            for line in lines {
                if write_line(&mut stream, &line).is_err() {
                    return;
                }
            }
        }
    }
}

fn write_line(stream: &mut UnixStream, line: &str) -> io::Result<()> {
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

pub(crate) fn result_line(id: &Value, result: Value) -> String {
    json!({"id": id, "result": result}).to_string()
}

pub(crate) fn error_line(id: &Value, code: &str, message: &str) -> String {
    json!({"id": id, "error": {"code": code, "message": message}}).to_string()
}

/// What an `events.subscribe` stream sends: one event of herdr's
/// (`{event, data}`).
pub(crate) fn event_line(event: &Value) -> String {
    event.to_string()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Echo(mpsc::Sender<mpsc::Sender<String>>);

    impl ApiHandler for Echo {
        fn call(&self, method: &str, params: Value) -> Answer {
            match method {
                "echo" => Answer::Result(json!({"type": "echo", "params": params})),
                "stream" => {
                    let (tx, rx) = mpsc::channel();
                    let _ = self.0.send(tx);
                    Answer::Stream(json!({"type": "subscription_started"}), rx)
                }
                _ => Answer::error("unknown_method", method),
            }
        }
    }

    /// A private directory short enough for socket names (macOS's temp
    /// directory is not); removed with everything in it on drop.
    pub(crate) struct ShortDir(PathBuf);

    impl ShortDir {
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ShortDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn short_dir() -> ShortDir {
        let dir = PathBuf::from("/tmp").join(format!(
            "hbl-{}-{}",
            std::process::id(),
            crate::browser::session::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        ShortDir(dir)
    }

    fn ask(path: &Path, line: &str) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(format!("{line}\n").as_bytes()).unwrap();
        stream
    }

    fn read_lines(stream: &mut UnixStream, n: usize) -> Vec<Value> {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        (0..n)
            .map(|_| {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                serde_json::from_str(&line).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_failing_accept_is_not_retried_at_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = short_dir();
        let path = dir.path().join("k.api");
        let listener = bind_private(&path).unwrap();
        // a connection waits, so the socket stays readable while accept fails
        let _waiting = UnixStream::connect(&path).unwrap();
        let (stop, stopped) = UnixStream::pair().unwrap();
        let tries = Arc::new(AtomicUsize::new(0));
        let accept = {
            let tries = Arc::clone(&tries);
            std::thread::spawn(move || {
                accept_loop_with(
                    &listener,
                    &stopped,
                    || {
                        tries.fetch_add(1, Ordering::Relaxed);
                        Err(io::Error::other("too many open files"))
                    },
                    |_| {},
                )
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        drop(stop);
        accept.join().unwrap();
        let tries = tries.load(Ordering::Relaxed);
        assert!((1..=100).contains(&tries), "{tries}");
    }

    #[test]
    fn one_request_per_connection_and_a_stream() {
        let dir = short_dir();
        let path = dir.path().join("k.api");
        let (streams, opened) = mpsc::channel();
        let socket = ApiSocket::bind(&path, Arc::new(Echo(streams))).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let mut stream = ask(&path, r#"{"id":"c1","method":"echo","params":{"a":1}}"#);
        let answer = read_lines(&mut stream, 1).remove(0);
        assert_eq!(
            answer,
            json!({"id": "c1", "result": {"type": "echo", "params": {"a": 1}}})
        );
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "closed after one answer");

        let mut stream = ask(&path, r#"{"id":7,"method":"nope"}"#);
        assert_eq!(
            read_lines(&mut stream, 1)[0]["error"]["code"],
            "unknown_method"
        );
        assert_eq!(
            read_lines(&mut ask(&path, "nonsense"), 1)[0]["error"]["code"],
            "invalid_request"
        );

        let mut stream = ask(&path, r#"{"id":"e1","method":"stream"}"#);
        let events = opened.recv_timeout(Duration::from_secs(5)).unwrap();
        events
            .send(event_line(&json!({"event": "herdr.link_up", "data": {}})))
            .unwrap();
        let lines = read_lines(&mut stream, 2);
        assert_eq!(lines[0]["result"]["type"], "subscription_started");
        assert_eq!(lines[1]["event"], "herdr.link_up");

        // a live socket is not taken over; a dead one's file is
        assert!(ApiSocket::bind(&path, Arc::new(Echo(mpsc::channel().0))).is_err());
        drop(socket);
        assert!(!path.exists());
        let _ = UnixListener::bind(&path).unwrap();
        drop(events);
        assert!(ApiSocket::bind(&path, Arc::new(Echo(mpsc::channel().0))).is_ok());
    }
}
