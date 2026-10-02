//! The coordinator's dashboard HTTP server on 127.0.0.1, run by the herdr
//! server's coordinator worker ([`DashboardServer`]) once it holds the
//! watcher lock.
//!
//! Read-only and local: GET only, HTTP/1.0 with `Connection: close`, one
//! thread per connection. It serves the coordinator agent's page
//! (`dashboard/index.html`), the live data herdr writes, the board the
//! coordinator writes, and a few built-in icons. Files under `dashboard/` are
//! resolved through a canonical-prefix check, so `..`, encoded dots and
//! symlinks cannot reach anything outside it.

use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::{board_path, dashboard_dir, live_path, memory_index_path, wakeups_path};

/// Largest body the server sends; bigger files answer 413.
const MAX_BODY: u64 = 2 * 1024 * 1024;
/// `/wakeups` shows this many trailing lines of `wakeups.log`.
const WAKEUP_LINES: usize = 200;
/// `/wakeups` reads at most this many trailing bytes before taking lines.
const WAKEUP_TAIL_BYTES: u64 = 256 * 1024;
/// Request head (request line plus headers) read limit.
const MAX_HEAD: u64 = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the stoppable accept loop checks its stop flag when idle.
const ACCEPT_POLL: Duration = Duration::from_millis(200);

const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";
const PNG: &str = "image/png";

/// Built-in icons, served under `/icons/<name>` (generated with Codex image
/// generation, Dusk palette, transparent backgrounds).
const ICONS: &[(&str, &[u8])] = &[
    ("favicon.png", include_bytes!("assets/icons/favicon.png")),
    (
        "coordinator.png",
        include_bytes!("assets/icons/coordinator.png"),
    ),
    ("agent.png", include_bytes!("assets/icons/agent.png")),
    ("empty.png", include_bytes!("assets/icons/empty.png")),
];

fn spawn_connection(dir: &Path, stream: TcpStream) {
    let dir = dir.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("herdr-coordinator-http".into())
        .spawn(move || {
            if let Err(err) = connection(&dir, stream) {
                tracing::debug!(error = %err, "coordinator: dashboard connection failed");
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(error = %err, "coordinator: dashboard could not spawn a connection thread");
    }
}

/// The dashboard served on a background thread until dropped. The listener
/// is nonblocking and the accept loop polls a stop flag every
/// [`ACCEPT_POLL`], so dropping it releases the port promptly (the drop
/// joins the accept loop). Connections already accepted finish on their own threads,
/// bounded by the 5 s IO timeout.
pub struct DashboardServer {
    port: u16,
    stop: Arc<AtomicBool>,
    /// Accept-loop passes (a no-spin check for tests).
    #[cfg(test)]
    polls: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

impl DashboardServer {
    /// Bind `127.0.0.1:<port>` (`0` picks a free port) and start serving.
    /// A bind failure (port in use) is returned, not logged: the caller
    /// reports it once.
    pub fn start(dir: PathBuf, port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let counter = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        let polls = counter.clone();
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("herdr-coordinator-http".into())
            .spawn(move || accept_until_stopped(&dir, &listener, &flag, &counter))?;
        tracing::info!(port, "coordinator: dashboard listening");
        Ok(Self {
            port,
            stop,
            #[cfg(test)]
            polls,
            handle: Some(handle),
        })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    #[cfg(test)]
    fn polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::warn!("coordinator: dashboard accept thread panicked");
            }
            tracing::info!(port = self.port, "coordinator: dashboard stopped");
        }
    }
}

impl Drop for DashboardServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `http://127.0.0.1:<port>/`.
pub fn dashboard_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

fn accept_until_stopped(
    dir: &Path,
    listener: &TcpListener,
    stop: &AtomicBool,
    polls: &AtomicUsize,
) {
    while !stop.load(Ordering::Relaxed) {
        polls.fetch_add(1, Ordering::Relaxed);
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets may inherit the listener's nonblocking
                // mode (BSD/macOS); the connection code relies on timeouts.
                if let Err(err) = stream.set_nonblocking(false) {
                    tracing::debug!(error = %err, "coordinator: dashboard connection setup failed");
                    continue;
                }
                spawn_connection(dir, stream);
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(err) => {
                tracing::debug!(error = %err, "coordinator: dashboard accept failed");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

fn connection(dir: &Path, stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new((&stream).take(MAX_HEAD));
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    // Drain the headers before answering: closing a socket with unread bytes
    // makes some stacks reset the connection and truncate the response.
    let mut host = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("host") {
                host = Some(value.trim().to_string());
            }
        }
    }
    let (status, content_type, body) = if host.as_deref().is_some_and(|h| !host_allowed(h)) {
        // A foreign Host is a DNS-rebinding page trying to read local data.
        error(403, "forbidden_host")
    } else {
        handle(dir, request_line.trim_end())
    };
    let mut stream = stream;
    stream.write_all(&response_head(status, content_type, body.len()))?;
    stream.write_all(&body)?;
    stream.flush()
}

/// Only loopback names may address the dashboard (the port is not checked).
fn host_allowed(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

fn response_head(status: u16, content_type: &str, len: usize) -> Vec<u8> {
    format!(
        "HTTP/1.0 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\n\
         Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data:\r\n\r\n",
        reason(status)
    )
    .into_bytes()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

fn error(status: u16, code: &str) -> (u16, &'static str, Vec<u8>) {
    let body = serde_json::json!({ "error": code }).to_string();
    (status, JSON, body.into_bytes())
}

/// Answer one request line: `(status, content type, body)`.
pub fn handle(dir: &Path, request_line: &str) -> (u16, &'static str, Vec<u8>) {
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return error(400, "bad_request");
    };
    if method != "GET" {
        return error(405, "method_not_allowed");
    }
    let path = target.split(['?', '#']).next().unwrap_or_default();
    if !path.starts_with('/') || !path_is_plain(path) {
        return error(400, "bad_path");
    }
    match path {
        "/" | "/index.html" => dashboard_file(dir, "index.html"),
        "/board.json" => dashboard_file(dir, "board.json"),
        "/live.json" => file(&live_path(dir), JSON),
        "/memory" => file(&memory_index_path(dir), TEXT),
        "/wakeups" => wakeups(dir),
        "/favicon.ico" => icon("favicon.png"),
        _ => {
            if let Some(name) = path.strip_prefix("/icons/") {
                icon(name)
            } else if let Some(rel) = path.strip_prefix("/dashboard/") {
                dashboard_file(dir, rel)
            } else {
                error(404, "not_found")
            }
        }
    }
}

/// No traversal or encoding tricks: no `.`/`..` or empty segments, no
/// backslashes, no percent-encoding at all (dashboard files have plain
/// names), no control characters. `path` starts with `/`.
fn path_is_plain(path: &str) -> bool {
    !path.contains(['\\', '%'])
        && !path.chars().any(char::is_control)
        && path[1..].split('/').enumerate().all(|(index, segment)| {
            segment != ".." && segment != "." && (index == 0 || !segment.is_empty())
        })
}

fn icon(name: &str) -> (u16, &'static str, Vec<u8>) {
    ICONS.iter().find(|(icon, _)| *icon == name).map_or_else(
        || error(404, "not_found"),
        |(_, bytes)| (200, PNG, bytes.to_vec()),
    )
}

/// A file under `dashboard/`, only if its canonical path stays there.
fn dashboard_file(dir: &Path, rel: &str) -> (u16, &'static str, Vec<u8>) {
    if rel.is_empty() || rel.starts_with('/') {
        return error(400, "bad_path");
    }
    let Ok(root) = dashboard_dir(dir).canonicalize() else {
        return error(404, "not_found");
    };
    let requested = if rel == "board.json" {
        board_path(dir)
    } else {
        dashboard_dir(dir).join(rel)
    };
    match requested.canonicalize() {
        Ok(path) if path.starts_with(&root) && path.is_file() => {
            let content_type = content_type(&path);
            file(&path, content_type)
        }
        _ => error(404, "not_found"),
    }
}

fn file(path: &Path, content_type: &'static str) -> (u16, &'static str, Vec<u8>) {
    let handle = match std::fs::File::open(path) {
        Ok(handle) => handle,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return error(404, "not_found"),
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "coordinator: dashboard read failed");
            return error(500, "read_failed");
        }
    };
    if handle.metadata().is_ok_and(|meta| meta.len() > MAX_BODY) {
        return error(413, "too_large");
    }
    let mut body = Vec::new();
    match (&handle).take(MAX_BODY + 1).read_to_end(&mut body) {
        Ok(_) if body.len() as u64 > MAX_BODY => error(413, "too_large"),
        Ok(_) => (200, content_type, body),
        Err(err) => {
            tracing::debug!(path = %path.display(), error = %err, "coordinator: dashboard read failed");
            error(500, "read_failed")
        }
    }
}

/// The last [`WAKEUP_LINES`] lines of `wakeups.log` (empty when absent).
fn wakeups(dir: &Path) -> (u16, &'static str, Vec<u8>) {
    let Ok(mut handle) = std::fs::File::open(wakeups_path(dir)) else {
        return (200, TEXT, Vec::new());
    };
    let len = handle.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(WAKEUP_TAIL_BYTES);
    let mut tail = Vec::new();
    if handle.seek(SeekFrom::Start(start)).is_err() || handle.read_to_end(&mut tail).is_err() {
        return error(500, "read_failed");
    }
    let text = String::from_utf8_lossy(&tail);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the cut may have landed mid-line
    }
    let skip = lines.len().saturating_sub(WAKEUP_LINES);
    let mut body = lines[skip..].join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    (200, TEXT, body.into_bytes())
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => JSON,
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "md" | "txt" => TEXT,
        "svg" => "image/svg+xml",
        "png" => PNG,
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::{seed, test_dir, DASHBOARD_TEMPLATE};

    fn seeded(name: &str) -> PathBuf {
        let dir = test_dir(name);
        seed(&dir).unwrap();
        dir
    }

    fn get(dir: &Path, path: &str) -> (u16, &'static str, Vec<u8>) {
        handle(dir, &format!("GET {path} HTTP/1.1"))
    }

    #[test]
    fn serves_the_page_live_data_and_board() {
        let dir = seeded("serve-routes");
        std::fs::write(live_path(&dir), r#"{"generated_unix":1}"#).unwrap();
        let (status, content_type, body) = get(&dir, "/");
        assert_eq!((status, content_type), (200, "text/html; charset=utf-8"));
        assert_eq!(body, DASHBOARD_TEMPLATE.as_bytes());
        assert_eq!(
            get(&dir, "/index.html?x=1").2,
            DASHBOARD_TEMPLATE.as_bytes()
        );
        assert_eq!(
            get(&dir, "/live.json"),
            (200, JSON, br#"{"generated_unix":1}"#.to_vec())
        );
        assert_eq!(get(&dir, "/board.json"), (200, JSON, b"{}\n".to_vec()));
        let (status, content_type, body) = get(&dir, "/memory");
        assert_eq!((status, content_type), (200, TEXT));
        assert!(String::from_utf8(body).unwrap().starts_with("# MEMORY"));
        std::fs::write(dashboard_dir(&dir).join("app.css"), "body{}").unwrap();
        assert_eq!(
            get(&dir, "/dashboard/app.css"),
            (200, "text/css; charset=utf-8", b"body{}".to_vec())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serves_the_built_in_icons() {
        let dir = test_dir("serve-icons");
        for name in ["favicon", "coordinator", "agent", "empty"] {
            let (status, content_type, body) = get(&dir, &format!("/icons/{name}.png"));
            assert_eq!((status, content_type), (200, PNG), "{name}");
            assert!(body.starts_with(b"\x89PNG\r\n\x1a\n"), "{name}");
            assert!(body.len() < 64 * 1024, "{name} is {} bytes", body.len());
        }
        assert_eq!(get(&dir, "/favicon.ico").0, 200);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_are_json_404s() {
        let dir = seeded("serve-missing");
        let (status, content_type, body) = get(&dir, "/live.json");
        assert_eq!((status, content_type), (404, JSON));
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["error"], "not_found");
        assert_eq!(get(&dir, "/nope").0, 404);
        assert_eq!(get(&dir, "/dashboard/nope.html").0, 404);
        assert_eq!(get(&dir, "/icons/nope.png").0, 404);
        // Unseeded directory: no dashboard at all.
        let bare = test_dir("serve-bare");
        assert_eq!(get(&bare, "/").0, 404);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn traversal_and_encoding_tricks_are_refused() {
        let dir = seeded("serve-traversal");
        std::fs::write(dir.join("managed.json"), "secret").unwrap();
        for path in [
            "/dashboard/../managed.json",
            "/dashboard/%2e%2e/managed.json",
            "/dashboard/%2E%2E/managed.json",
            "/dashboard/..%2fmanaged.json",
            "/dashboard/..\\managed.json",
            "/dashboard//etc/passwd",
            "/dashboard/./index.html",
            "/../managed.json",
            "/icons/../managed.json",
            "managed.json",
            "http://127.0.0.1/managed.json",
        ] {
            let (status, _, body) = get(&dir, path);
            assert!(status == 400 || status == 404, "{path}: {status}");
            assert_ne!(body, b"secret", "{path}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_escaping_the_dashboard_are_404() {
        let dir = seeded("serve-symlink");
        std::fs::write(dir.join("managed.json"), "secret").unwrap();
        std::os::unix::fs::symlink(
            dir.join("managed.json"),
            dashboard_dir(&dir).join("leak.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(&dir, dashboard_dir(&dir).join("up")).unwrap();
        assert_eq!(get(&dir, "/dashboard/leak.json").0, 404);
        assert_eq!(get(&dir, "/dashboard/up/managed.json").0, 404);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_get_is_allowed() {
        let dir = seeded("serve-methods");
        for method in ["POST", "PUT", "DELETE", "HEAD"] {
            assert_eq!(
                handle(&dir, &format!("{method} / HTTP/1.1")).0,
                405,
                "{method}"
            );
        }
        assert_eq!(handle(&dir, "").0, 400);
        assert_eq!(handle(&dir, "GET").0, 400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wakeups_returns_the_last_200_lines() {
        let dir = seeded("serve-wakeups");
        assert_eq!(get(&dir, "/wakeups"), (200, TEXT, Vec::new()));
        let log: String = (0..250).map(|n| format!("line {n}\n")).collect();
        std::fs::write(wakeups_path(&dir), log).unwrap();
        let (status, _, body) = get(&dir, "/wakeups");
        let text = String::from_utf8(body).unwrap();
        assert_eq!(status, 200);
        assert_eq!(text.lines().count(), 200);
        assert_eq!(text.lines().next(), Some("line 50"));
        assert_eq!(text.lines().last(), Some("line 249"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_files_are_refused() {
        let dir = seeded("serve-large");
        std::fs::write(live_path(&dir), vec![b' '; MAX_BODY as usize + 1]).unwrap();
        assert_eq!(get(&dir, "/live.json").0, 413);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_types_follow_the_extension() {
        for (name, expected) in [
            ("a.html", "text/html; charset=utf-8"),
            ("a.JSON", JSON),
            ("a.js", "text/javascript; charset=utf-8"),
            ("a.md", TEXT),
            ("a.txt", TEXT),
            ("a.svg", "image/svg+xml"),
            ("a.png", PNG),
            ("a.ico", "image/x-icon"),
            ("a.bin", "application/octet-stream"),
        ] {
            assert_eq!(content_type(Path::new(name)), expected, "{name}");
        }
    }

    #[test]
    fn only_loopback_hosts_are_allowed() {
        for host in [
            "127.0.0.1:7718",
            "localhost:7718",
            "LOCALHOST",
            "[::1]:7718",
            "127.0.0.1",
        ] {
            assert!(host_allowed(host), "{host}");
        }
        for host in [
            "evil.example:7718",
            "127.0.0.1.evil.example",
            "",
            "[::2]:7718",
        ] {
            assert!(!host_allowed(host), "{host}");
        }
    }

    #[test]
    fn dashboard_html_renders_data_as_text_and_keeps_the_contract() {
        for unsafe_sink in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
        ] {
            assert!(!DASHBOARD_TEMPLATE.contains(unsafe_sink), "{unsafe_sink}");
        }
        assert!(DASHBOARD_TEMPLATE.contains(
            "Contract: GET /live.json (LiveData: generated_unix, agents[], offline[], groups[], unmanaged_count,"
        ));
        assert!(DASHBOARD_TEMPLATE.contains("Render all data with textContent only."));
        // Self-contained: no external requests.
        for external in ["http://", "https://", "@import"] {
            assert!(!DASHBOARD_TEMPLATE.contains(external), "{external}");
        }
    }

    #[test]
    fn real_socket_on_loopback() {
        let dir = seeded("serve-socket");
        std::fs::write(live_path(&dir), r#"{"generated_unix":7}"#).unwrap();
        let server = DashboardServer::start(dir.clone(), 0).unwrap();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], server.port()));
        let fetch = |request: &str| {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let response = fetch("GET /live.json HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: */*\r\n\r\n");
        assert!(response.starts_with("HTTP/1.0 200 OK\r\n"), "{response}");
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains("Cache-Control: no-store\r\n"));
        assert!(response.contains("Connection: close\r\n"));
        assert!(
            response.ends_with("\r\n\r\n{\"generated_unix\":7}"),
            "{response}"
        );
        let rebinding = fetch("GET /live.json HTTP/1.1\r\nHost: evil.example:80\r\n\r\n");
        assert!(rebinding.starts_with("HTTP/1.0 403 "), "{rebinding}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fetch_live(port: u16) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET /live.json HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn a_stopped_server_releases_its_port() {
        let dir = seeded("serve-stop");
        std::fs::write(live_path(&dir), r#"{"generated_unix":3}"#).unwrap();
        let server = DashboardServer::start(dir.clone(), 0).unwrap();
        let port = server.port();
        assert_ne!(port, 0);
        assert_eq!(dashboard_url(port), format!("http://127.0.0.1:{port}/"));
        let response = fetch_live(port);
        assert!(response.starts_with("HTTP/1.0 200 OK\r\n"), "{response}");
        assert!(response.ends_with("{\"generated_unix\":3}"), "{response}");
        // A second bind of the same port fails while it serves.
        assert!(DashboardServer::start(dir.clone(), port).is_err());
        let started = std::time::Instant::now();
        drop(server);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "stop waits at most one poll interval"
        );
        let again = DashboardServer::start(dir.clone(), port).expect("port released");
        assert_eq!(
            fetch_live(again.port()).lines().next(),
            Some("HTTP/1.0 200 OK")
        );
        drop(again);
        assert!(
            TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "drop releases the port too"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_idle_accept_loop_sleeps_between_polls() {
        // The loop must not spin: over 600 ms idle it polls a handful of
        // times (every 200 ms), never thousands.
        let dir = seeded("serve-idle");
        let server = DashboardServer::start(dir.clone(), 0).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let polls = server.polls();
        drop(server);
        assert!((1..=6).contains(&polls), "{polls}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
