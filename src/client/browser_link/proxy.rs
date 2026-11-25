//! The HTTP proxy of a machine's browser: 127.0.0.1, with a secret. Every page
//! of that browser goes through it, and it is the only way out.
//!
//! Who dials is decided per request, never per connection: this Mac an address
//! the [`Classifier`] checked and pinned, the remote machine everything else
//! (by name, over [`Dial`]). There is no fallback from one to the other. No
//! upstream connection is used for a second host:port; plain http gets one
//! request per connection, `Connection: close` on both sides.

use std::io::{self, Read as _, Write as _};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;

use super::forward::{Dial, DialError, Permit, RemoteConnection};
use super::ssh::Destination;

/// The user name of the proxy's Basic authentication.
pub(crate) const PROXY_USER: &str = "herdr";
/// The name the browser asks for until the proxy answers; answered here.
pub(crate) const WARM_HOST: &str = "herdr-warm.invalid";
/// The header of that answer.
pub(crate) const WARM_HEADER: &str = "x-herdr-proxy";

/// Chromium holds up to 151 http sockets per proxy and 255 WebSockets.
const CONNECTIONS: usize = 512;
/// Two files per connection, and the helper's own.
const FILE_LIMIT: u64 = 2048;
const THREAD_STACK: usize = 256 * 1024;
const HEAD_MAX: usize = 64 * 1024;
const RESPONSE_HEAD_MAX: usize = 256 * 1024;
const CHUNK_LINE_MAX: usize = 4096;
const BUFFER: usize = 32 * 1024;
/// An open connection that asks for nothing: Chromium's preconnects.
const HEAD_TIMEOUT: Duration = Duration::from_secs(120);
/// For all addresses of a name together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The next address of a name is tried beside the one that does not answer.
const CONNECT_STAGGER: Duration = Duration::from_millis(250);
/// One side of a tunnel ended; the other may be silent this long.
const HALF_CLOSED_IDLE: Duration = Duration::from_secs(30);
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);
/// After an answer, what the client still sends is read this long, or the
/// reset takes the answer with it.
const LINGER: Duration = Duration::from_secs(1);
const NO_DESTINATION: &str = "not a host and port this browser can open";

/// Who dials a destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// This Mac, one of these addresses (checked; the name is not looked up
    /// again), in this order.
    Direct(Vec<SocketAddr>),
    /// The remote machine, by name.
    Remote,
    /// Nobody; why, for the error page.
    Refuse(String),
}

/// Decides it for one request.
pub(crate) trait Classifier: Send + Sync {
    /// A host's one spelling (`127.1` is `127.0.0.1`, an IPv6 address has
    /// brackets), or why it is none. Pure, no lookup.
    fn canonical(&self, host: &str) -> Result<String, String>;
    /// `host` is canonical. May block for a lookup.
    fn route(&self, host: &str, port: u16) -> Route;
}

/// What a proxy is started with.
pub(crate) struct ProxyOptions {
    /// The machine's name, for the error pages.
    pub(crate) label: String,
    /// The companion launch, the warm-up answer's header value.
    pub(crate) instance: String,
    /// Fixed for the companion's life, as the port is.
    pub(crate) secret: String,
    /// `egress = "remote"`: the remote machine dials everything.
    pub(crate) remote_only: bool,
    pub(crate) classifier: Arc<dyn Classifier>,
    pub(crate) dial: Arc<dyn Dial>,
}

/// A running proxy; dropping it closes its port and its connections.
pub(crate) struct Proxy {
    port: u16,
    stop: Option<UnixStream>,
    accept: Option<JoinHandle<()>>,
}

struct Shared {
    options: ProxyOptions,
    /// Readable once the proxy stops.
    stop: UnixStream,
    open: AtomicUsize,
}

impl Proxy {
    /// Listens on 127.0.0.1 alone: on `port`, or with 0 on a free one.
    pub(crate) fn start(port: u16, options: ProxyOptions) -> io::Result<Self> {
        if options.secret.is_empty() || header_text(&options.instance).is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the browser's proxy needs a secret and an instance",
            ));
        }
        let limit = crate::platform::raise_soft_file_limit(FILE_LIMIT)?;
        if limit < FILE_LIMIT {
            tracing::warn!(limit, "the file limit is low for the browser's proxy");
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (stop, stopped) = UnixStream::pair()?;
        let shared = Arc::new(Shared {
            options,
            stop: stopped,
            open: AtomicUsize::new(0),
        });
        let accept = std::thread::Builder::new()
            .name("browser-proxy".into())
            .spawn(move || accept_loop(&listener, &shared))?;
        Ok(Self {
            port,
            stop: Some(stop),
            accept: Some(accept),
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    loop {
        let mut fds = [
            poll_fd(listener.as_raw_fd(), libc::POLLIN),
            poll_fd(shared.stop.as_raw_fd(), libc::POLLIN),
        ];
        if poll(&mut fds, None).is_err() || fds[1].revents != 0 {
            return;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => {
                tracing::debug!(%error, "the browser's proxy could not accept");
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let open = Open::new(shared);
        if shared.open.load(Ordering::Acquire) > CONNECTIONS {
            tracing::warn!("too many connections to the browser's proxy");
            continue;
        }
        let started = std::thread::Builder::new()
            .name("browser-proxy-conn".into())
            .stack_size(THREAD_STACK)
            .spawn(move || {
                // macOS gives an accepted socket its listener's mode.
                let ready = stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_nodelay(true))
                    .and_then(|()| stream.set_write_timeout(Some(ANSWER_TIMEOUT)));
                if ready.is_ok() {
                    serve(&stream, &open.0);
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "no thread for a connection to the browser's proxy");
        }
    }
}

/// Counts one client connection.
struct Open(Arc<Shared>);

impl Open {
    fn new(shared: &Arc<Shared>) -> Self {
        shared.open.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(shared))
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// One client connection

/// An answer of the proxy itself.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Answer {
    fn text(status: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn bad(text: &str) -> Self {
        Self::text("400 Bad Request", text)
    }

    fn gateway(text: impl Into<String>) -> Self {
        Self::text("502 Bad Gateway", text)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {}\r\n", self.status);
        for (name, value) in &self.headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        if !self.status.starts_with("204") {
            out.push_str(&format!(
                "Content-Type: text/plain; charset=utf-8\r\nX-Content-Type-Options: nosniff\r\nContent-Length: {}\r\n",
                self.body.len()
            ));
        }
        out.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
        out.push_str(&self.body);
        out.into_bytes()
    }
}

/// What ended before a whole request was there.
enum HeadError {
    Closed,
    Answer(Answer),
}

fn serve(client: &TcpStream, shared: &Shared) {
    let answer = match read_head(client, shared) {
        Ok((head, rest)) => match handle(client, shared, &head, rest) {
            Ok(()) => return,
            Err(answer) => answer,
        },
        Err(HeadError::Answer(answer)) => answer,
        Err(HeadError::Closed) => return,
    };
    let mut writer = client;
    if writer.write_all(&answer.bytes()).is_ok() {
        linger(client, shared);
    }
}

/// Lets the client read its answer although it is still sending.
fn linger(client: &TcpStream, shared: &Shared) {
    let _ = client.shutdown(Shutdown::Write);
    let mut reader = client;
    let mut buffer = [0_u8; 4096];
    let mut left = 64;
    while left > 0 && wait_readable(client.as_raw_fd(), shared, LINGER) {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(_) => left -= 1,
        }
    }
}

/// The bytes up to the empty line, and what came after it.
fn read_head(client: &TcpStream, shared: &Shared) -> Result<(Vec<u8>, Vec<u8>), HeadError> {
    let mut reader = client;
    let mut head = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if let Some(end) = find(&head, b"\r\n\r\n") {
            let rest = head.split_off(end + 4);
            head.truncate(end);
            return Ok((head, rest));
        }
        if head.len() > HEAD_MAX {
            return Err(HeadError::Answer(Answer::text(
                "431 Request Header Fields Too Large",
                "the request's header is too large",
            )));
        }
        if !wait_readable(client.as_raw_fd(), shared, HEAD_TIMEOUT) {
            return Err(HeadError::Closed);
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Err(HeadError::Closed),
            Ok(read) => head.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(HeadError::Closed),
        }
    }
}

fn find(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes
        .windows(wanted.len())
        .position(|window| window == wanted)
}

fn handle(client: &TcpStream, shared: &Shared, head: &[u8], rest: Vec<u8>) -> Result<(), Answer> {
    let request = Request::parse(head)?;
    if !authorized(&request, &shared.options.secret) {
        return Err(Answer {
            headers: vec![("Proxy-Authenticate", "Basic realm=\"herdr\"".to_owned())],
            ..Answer::text(
                "407 Proxy Authentication Required",
                "the proxy of this browser needs its secret",
            )
        });
    }
    if request.method == "CONNECT" {
        let (host, port) =
            connect_target(&request.target).ok_or_else(|| Answer::bad(NO_DESTINATION))?;
        let to = shared.destination(host, port)?;
        let upstream = shared.upstream(&to)?;
        let mut writer = client;
        // The client is gone: so is the connection.
        if writer
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .is_err()
        {
            return Ok(());
        }
        tunnel(client, &upstream, rest, shared);
        return Ok(());
    }
    let (host, port, path) = absolute_target(&request.target)?;
    if host.eq_ignore_ascii_case(WARM_HOST) && port == 80 {
        return Err(Answer {
            headers: vec![(WARM_HEADER, header_text(&shared.options.instance))],
            ..Answer::text("204 No Content", "")
        });
    }
    let to = shared.destination(host, port)?;
    let (head, body) = request.upstream_head(&to, &path)?;
    let upstream = shared.upstream(&to)?;
    exchange(client, &upstream, head, body, rest, shared, &to)
}

impl Shared {
    fn label(&self) -> String {
        let label = header_text(&self.options.label);
        if label.is_empty() {
            "the remote machine".to_owned()
        } else {
            label
        }
    }

    /// What a request names. The proxy's own names and every other under
    /// `.invalid` end here.
    fn destination(&self, host: &str, port: u16) -> Result<Destination, Answer> {
        let name = host.to_ascii_lowercase();
        let name = name.strip_suffix('.').unwrap_or(&name);
        if name == "invalid" || name.ends_with(".invalid") {
            return Err(Answer::gateway(format!(
                "{} does not exist",
                header_text(name)
            )));
        }
        let host = self
            .options
            .classifier
            .canonical(host)
            .map_err(|reason| Answer::bad(&header_text(&reason)))?;
        Destination::new(&host, port).ok_or_else(|| Answer::bad(NO_DESTINATION))
    }

    fn upstream(&self, to: &Destination) -> Result<Upstream, Answer> {
        let route = if self.options.remote_only {
            Route::Remote
        } else {
            self.options.classifier.route(to.host(), to.port())
        };
        match route {
            Route::Direct(addresses) => match connect_any(&addresses, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    let _ = stream.set_nodelay(true);
                    Ok(Upstream::Direct(stream))
                }
                Err(error) => Err(Answer::gateway(format!(
                    "could not connect to {to}: {error}"
                ))),
            },
            Route::Remote => self
                .options
                .dial
                .dial(to)
                .map(Upstream::remote)
                .map_err(|error| Answer::gateway(self.dial_text(error))),
            Route::Refuse(reason) => Err(Answer::gateway(format!("{to}: {reason}"))),
        }
    }

    fn dial_text(&self, error: DialError) -> String {
        let label = self.label();
        match error {
            DialError::Down => format!("{label} is not connected"),
            DialError::TooMany => {
                format!("too many open connections through {label}; close some pages")
            }
            DialError::Unmanaged => format!(
                "browsing through {label} needs herdr's managed ssh connection (remote.manage_ssh_config)"
            ),
            DialError::Failed(text) => {
                tracing::warn!(error = %text, "the browser's ssh connection failed");
                format!("{label} could not be reached: {}", header_text(&text))
            }
        }
    }
}

/// Fits a header value and a text page: one line, no control character.
fn header_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Tries the addresses in turn, each one after the last had a moment; all of
/// them within `timeout`.
fn connect_any(addresses: &[SocketAddr], timeout: Duration) -> io::Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    let (done, results) = mpsc::channel();
    let mut failure = io::Error::new(io::ErrorKind::TimedOut, "no address answered");
    let mut running = 0;
    let mut waiting = addresses.iter();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let next = waiting.next().filter(|_| !left.is_zero());
        if let Some(&address) = next {
            let done = done.clone();
            std::thread::Builder::new()
                .name("browser-proxy-dial".into())
                .stack_size(THREAD_STACK)
                .spawn(move || {
                    let _ = done.send(TcpStream::connect_timeout(&address, left));
                })?;
            running += 1;
        }
        if running == 0 || left.is_zero() {
            return Err(failure);
        }
        let pause = if waiting.len() > 0 {
            CONNECT_STAGGER.min(left)
        } else {
            left
        };
        match results.recv_timeout(pause) {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => {
                running -= 1;
                failure = error;
            }
            // The next address, or the end of the time.
            Err(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Requests

struct Request {
    method: String,
    target: String,
    /// Names lowercased, values as sent.
    headers: Vec<(String, Vec<u8>)>,
}

impl Request {
    /// Strict: what two parsers could read differently is refused.
    fn parse(head: &[u8]) -> Result<Self, Answer> {
        let mut lines = lines(head).ok_or_else(|| Answer::bad("a malformed request"))?;
        let line = lines.next().unwrap_or_default();
        let line =
            std::str::from_utf8(line).map_err(|_| Answer::bad("a malformed request line"))?;
        let mut parts = line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Answer::bad("a malformed request line"));
        };
        let visible = |text: &str| text.bytes().all(|byte| (0x21..=0x7e).contains(&byte));
        if !is_token(method.as_bytes())
            || target.is_empty()
            || !visible(target)
            || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
        {
            return Err(Answer::bad("a malformed request line"));
        }
        let mut headers = Vec::new();
        for line in lines {
            let (name, value) =
                split_header(line).ok_or_else(|| Answer::bad("a malformed header"))?;
            headers.push((name, value.to_vec()));
        }
        Ok(Self {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
        })
    }

    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a [u8]> {
        self.headers
            .iter()
            .filter(move |(header, _)| header == name)
            .map(|(_, value)| value.as_slice())
    }

    /// The head the upstream gets, in origin form, and how the body is framed.
    fn upstream_head(&self, to: &Destination, path: &str) -> Result<(Vec<u8>, Body), Answer> {
        let body = self.body()?;
        let named = connection_options(self.values("connection"));
        let mut head = format!("{} {path} HTTP/1.1\r\nHost: {}", self.method, to.host());
        if to.port() != 80 {
            head.push_str(&format!(":{}", to.port()));
        }
        head.push_str("\r\n");
        let mut head = head.into_bytes();
        for (name, value) in &self.headers {
            let ours = matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "expect"
            );
            if ours || hop_by_hop(name, &named) {
                continue;
            }
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(value);
            head.extend_from_slice(b"\r\n");
        }
        match &body {
            Body::None => {}
            Body::Length(length) => {
                head.extend_from_slice(format!("content-length: {length}\r\n").as_bytes());
            }
            Body::Chunked(_) => head.extend_from_slice(b"transfer-encoding: chunked\r\n"),
        }
        head.extend_from_slice(b"connection: close\r\n\r\n");
        Ok((head, body))
    }

    fn body(&self) -> Result<Body, Answer> {
        let mut lengths = self.values("content-length");
        let mut encodings = self.values("transfer-encoding");
        match (lengths.next(), encodings.next()) {
            (None, None) => Ok(Body::None),
            (None, Some(encoding)) => {
                if encoding.eq_ignore_ascii_case(b"chunked") && encodings.next().is_none() {
                    Ok(Body::Chunked(Chunks::default()))
                } else {
                    Err(Answer::bad("a transfer encoding this proxy does not know"))
                }
            }
            (Some(length), None) => {
                if lengths.next().is_some()
                    || length.is_empty()
                    || !length.iter().all(u8::is_ascii_digit)
                {
                    return Err(Answer::bad("a malformed content length"));
                }
                std::str::from_utf8(length)
                    .ok()
                    .and_then(|length| length.parse().ok())
                    .map(Body::Length)
                    .ok_or_else(|| Answer::bad("a malformed content length"))
            }
            (Some(_), Some(_)) => Err(Answer::bad("a body with two framings")),
        }
    }
}

/// The lines of a head; `None` with a bare CR or LF or a NUL in it.
fn lines(head: &[u8]) -> Option<impl Iterator<Item = &[u8]>> {
    let mut rest = head;
    let mut lines = Vec::new();
    loop {
        let (line, next) = match find(rest, b"\r\n") {
            Some(end) => (&rest[..end], Some(&rest[end + 2..])),
            None => (rest, None),
        };
        if line.iter().any(|byte| matches!(byte, b'\r' | b'\n' | 0)) {
            return None;
        }
        lines.push(line);
        match next {
            Some(next) => rest = next,
            None => return Some(lines.into_iter()),
        }
    }
}

/// A header line's lowercased name and its trimmed value.
fn split_header(line: &[u8]) -> Option<(String, &[u8])> {
    let colon = line.iter().position(|&byte| byte == b':')?;
    let name = &line[..colon];
    if !is_token(name) {
        return None;
    }
    let name = std::str::from_utf8(name).ok()?.to_ascii_lowercase();
    Some((name, line[colon + 1..].trim_ascii()))
}

fn is_token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// The header names `Connection` lists.
fn connection_options<'a>(values: impl Iterator<Item = &'a [u8]>) -> Vec<String> {
    values
        .flat_map(|value| value.split(|&byte| byte == b','))
        .filter_map(|name| std::str::from_utf8(name.trim_ascii()).ok())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Headers between two parties only; `Transfer-Encoding` is not among them
/// because a response's body passes as it is framed.
fn hop_by_hop(name: &str, named: &[String]) -> bool {
    matches!(
        name,
        "connection" | "keep-alive" | "te" | "trailer" | "upgrade"
    ) || name.starts_with("proxy-")
        || (named.iter().any(|other| other == name)
            && !matches!(name, "content-length" | "transfer-encoding"))
}

fn authorized(request: &Request, secret: &str) -> bool {
    let mut values = request.values("proxy-authorization");
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Some((scheme, token)) = value
        .iter()
        .position(|&byte| byte == b' ')
        .map(|space| (&value[..space], value[space + 1..].trim_ascii()))
    else {
        return false;
    };
    let Ok(sent) = base64::engine::general_purpose::STANDARD.decode(token) else {
        return false;
    };
    scheme.eq_ignore_ascii_case(b"basic")
        && !secret.is_empty()
        && same_bytes(&sent, format!("{PROXY_USER}:{secret}").as_bytes())
}

/// Takes the same time wherever they differ.
fn same_bytes(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0, |different, (a, b)| different | (a ^ b))
            == 0
}

fn connect_target(target: &str) -> Option<(&str, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    Some((host, parse_port(port)?))
}

fn parse_port(port: &str) -> Option<u16> {
    (!port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| port.parse().ok())
        .flatten()
        .filter(|port| *port != 0)
}

/// An absolute `http:` URL's host, port and path with the query.
fn absolute_target(target: &str) -> Result<(&str, u16, String), Answer> {
    let rest = target
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .map(|_| &target[7..])
        .ok_or_else(|| Answer::bad("this proxy takes http:// URLs and CONNECT"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let colon = authority
        .rfind(':')
        .filter(|&colon| authority.rfind(']').is_none_or(|bracket| bracket < colon));
    let (host, port) = match colon {
        Some(colon) => (&authority[..colon], parse_port(&authority[colon + 1..])),
        None => (authority, Some(80)),
    };
    let port = port.ok_or_else(|| Answer::bad(NO_DESTINATION))?;
    if path.contains('#') {
        return Err(Answer::bad("a fragment in the request"));
    }
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    Ok((host, port, path))
}

/// How a request's body is framed.
#[derive(Debug, PartialEq, Eq)]
enum Body {
    None,
    Length(u64),
    Chunked(Chunks),
}

/// A chunked body on its way through, written again by us.
#[derive(Debug, Default, PartialEq, Eq)]
struct Chunks {
    line: Vec<u8>,
    state: ChunkState,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum ChunkState {
    #[default]
    Size,
    Data(u64),
    /// The line end after a chunk's data.
    DataEnd,
    Trailer,
    Done,
}

impl Body {
    /// Passes on what of `input` belongs to the body, and nothing behind it.
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), ()> {
        match self {
            Self::None => Ok(()),
            Self::Length(left) => {
                let take = usize::try_from(*left)
                    .unwrap_or(usize::MAX)
                    .min(input.len());
                out.extend_from_slice(&input[..take]);
                *left -= take as u64;
                Ok(())
            }
            Self::Chunked(chunks) => chunks.feed(input, out),
        }
    }
}

impl Chunks {
    fn feed(&mut self, mut input: &[u8], out: &mut Vec<u8>) -> Result<(), ()> {
        while !input.is_empty() {
            match &mut self.state {
                ChunkState::Done => return Ok(()),
                ChunkState::Data(left) => {
                    let take = usize::try_from(*left)
                        .unwrap_or(usize::MAX)
                        .min(input.len());
                    out.extend_from_slice(&input[..take]);
                    input = &input[take..];
                    *left -= take as u64;
                    if *left == 0 {
                        self.state = ChunkState::DataEnd;
                    }
                }
                _ => {
                    let byte = input[0];
                    input = &input[1..];
                    self.line.push(byte);
                    if self.line.len() > CHUNK_LINE_MAX {
                        return Err(());
                    }
                    if byte == b'\n' {
                        let line = std::mem::take(&mut self.line);
                        let line = line.strip_suffix(b"\r\n").ok_or(())?;
                        self.end_of_line(line, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn end_of_line(&mut self, line: &[u8], out: &mut Vec<u8>) -> Result<(), ()> {
        match self.state {
            ChunkState::Size => {
                let size = line.split(|&byte| byte == b';').next().unwrap_or_default();
                let size = std::str::from_utf8(size.trim_ascii()).map_err(|_| ())?;
                if size.is_empty()
                    || size.len() > 16
                    || !size.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(());
                }
                let size = u64::from_str_radix(size, 16).map_err(|_| ())?;
                if size == 0 {
                    self.state = ChunkState::Trailer;
                } else {
                    out.extend_from_slice(format!("{size:x}\r\n").as_bytes());
                    self.state = ChunkState::Data(size);
                }
            }
            ChunkState::DataEnd => {
                if !line.is_empty() {
                    return Err(());
                }
                out.extend_from_slice(b"\r\n");
                self.state = ChunkState::Size;
            }
            // Trailers are dropped.
            ChunkState::Trailer if line.is_empty() => {
                out.extend_from_slice(b"0\r\n\r\n");
                self.state = ChunkState::Done;
            }
            ChunkState::Trailer | ChunkState::Data(_) | ChunkState::Done => {}
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Responses

/// A response on its way to the client: its head rewritten, its body as it is.
#[derive(Default)]
struct Response {
    head: Vec<u8>,
    /// The head is through, the rest passes.
    passing: bool,
}

/// Why an upstream's answer does not reach the client.
#[derive(Debug, PartialEq, Eq)]
enum ResponseError {
    /// It asks for a login to a proxy: only ours may.
    ProxyLogin,
    NotHttp,
}

impl Response {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), ResponseError> {
        if self.passing {
            out.extend_from_slice(input);
            return Ok(());
        }
        self.head.extend_from_slice(input);
        while let Some(end) = find(&self.head, b"\r\n\r\n") {
            let rest = self.head.split_off(end + 4);
            self.head.truncate(end);
            let head = std::mem::replace(&mut self.head, rest);
            match rewrite_response_head(&head)? {
                Some(head) => {
                    out.extend_from_slice(&head);
                    out.extend_from_slice(&std::mem::take(&mut self.head));
                    self.passing = true;
                    return Ok(());
                }
                // An informational head: the real one follows.
                None => continue,
            }
        }
        let start = b"HTTP/1.";
        let same = self.head.len().min(start.len());
        if self.head.len() > RESPONSE_HEAD_MAX || self.head[..same] != start[..same] {
            return Err(ResponseError::NotHttp);
        }
        Ok(())
    }
}

/// `None` for an informational (1xx) head.
fn rewrite_response_head(head: &[u8]) -> Result<Option<Vec<u8>>, ResponseError> {
    let mut lines = lines(head).ok_or(ResponseError::NotHttp)?;
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .strip_prefix(b"HTTP/1.")
        .and_then(|rest| rest.get(1..5))
        .filter(|rest| rest[0] == b' ')
        .and_then(|rest| std::str::from_utf8(&rest[1..]).ok())
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|_| matches!(status_line.get(12), None | Some(b' ')))
        .ok_or(ResponseError::NotHttp)?;
    match status {
        407 => return Err(ResponseError::ProxyLogin),
        // No upgrade was asked for.
        101 => return Err(ResponseError::NotHttp),
        100..=199 => return Ok(None),
        200..=599 => {}
        _ => return Err(ResponseError::NotHttp),
    }
    let headers = lines.collect::<Vec<_>>();
    let named = connection_options(
        headers
            .iter()
            .filter_map(|line| split_header(line))
            .filter(|(name, _)| name == "connection")
            .map(|(_, value)| value),
    );
    let mut out = status_line.to_vec();
    out.extend_from_slice(b"\r\n");
    let mut dropped = false;
    for line in headers {
        // A folded line belongs to the header before it.
        let folded = matches!(line.first(), Some(b' ' | b'\t'));
        if !folded {
            dropped = match split_header(line) {
                Some((name, _)) => hop_by_hop(&name, &named) || name == WARM_HEADER,
                None => true,
            };
        }
        if !dropped {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// Moving bytes

enum Upstream {
    Direct(TcpStream),
    Remote {
        stream: UnixStream,
        /// Held while the connection is open.
        _permit: Permit,
    },
}

impl Upstream {
    fn remote(connection: RemoteConnection) -> Self {
        Self::Remote {
            stream: connection.stream,
            _permit: connection.permit,
        }
    }
}

/// A connected stream socket of either kind.
trait Socket {
    fn fd(&self) -> RawFd;
    fn receive(&self, buffer: &mut [u8]) -> io::Result<usize>;
    fn send(&self, bytes: &[u8]) -> io::Result<usize>;
    fn end_write(&self);
    fn unblock(&self) -> io::Result<()>;
}

macro_rules! socket {
    ($kind:ty) => {
        impl Socket for $kind {
            fn fd(&self) -> RawFd {
                self.as_raw_fd()
            }
            fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
                (&mut &*self).read(buffer)
            }
            fn send(&self, bytes: &[u8]) -> io::Result<usize> {
                (&mut &*self).write(bytes)
            }
            fn end_write(&self) {
                let _ = self.shutdown(Shutdown::Write);
            }
            fn unblock(&self) -> io::Result<()> {
                self.set_nonblocking(true)
            }
        }
    };
}
socket!(TcpStream);
socket!(UnixStream);

impl Upstream {
    fn socket(&self) -> &dyn Socket {
        match self {
            Self::Direct(stream) => stream,
            Self::Remote { stream, .. } => stream,
        }
    }
}

/// Bytes on their way from one socket to the other.
#[derive(Default)]
struct Flow {
    buffer: Vec<u8>,
    sent: usize,
    ended: bool,
}

impl Flow {
    fn pending(&self) -> bool {
        self.sent < self.buffer.len()
    }

    fn done(&self) -> bool {
        self.ended && !self.pending()
    }

    fn send(&mut self, to: &dyn Socket) -> io::Result<()> {
        while self.pending() {
            match to.send(&self.buffer[self.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(sent) => self.sent += sent,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        self.buffer.clear();
        self.sent = 0;
        Ok(())
    }
}

/// `None` when there is nothing to read yet.
fn receive(from: &dyn Socket, buffer: &mut [u8]) -> io::Result<Option<usize>> {
    match from.receive(buffer) {
        Ok(read) => Ok(Some(read)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// What the two sockets of a connection are waited for.
fn interest(fd: RawFd, read: bool, write: bool) -> libc::pollfd {
    let events = if read { libc::POLLIN } else { 0 } | if write { libc::POLLOUT } else { 0 };
    // A socket nothing is wanted of would report its hang-up over and over.
    poll_fd(if events == 0 { -1 } else { fd }, events)
}

fn ready(fd: &libc::pollfd) -> bool {
    fd.revents != 0
}

/// A CONNECT: both directions as they come, ends passed on.
fn tunnel(client: &TcpStream, upstream: &Upstream, rest: Vec<u8>, shared: &Shared) {
    let client: &dyn Socket = client;
    let upstream = upstream.socket();
    if client.unblock().is_err() || upstream.unblock().is_err() {
        return;
    }
    let mut up = Flow {
        buffer: rest,
        ..Flow::default()
    };
    let mut down = Flow::default();
    let mut buffer = vec![0_u8; BUFFER];
    while !(up.done() && down.done()) {
        let mut fds = [
            interest(client.fd(), !up.ended && !up.pending(), down.pending()),
            interest(upstream.fd(), !down.ended && !down.pending(), up.pending()),
            poll_fd(shared.stop.as_raw_fd(), libc::POLLIN),
        ];
        let timeout = (up.ended || down.ended).then_some(HALF_CLOSED_IDLE);
        match poll(&mut fds, timeout) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if ready(&fds[2]) {
            return;
        }
        for (flow, from, to, polled) in [
            (&mut up, client, upstream, &fds[0]),
            (&mut down, upstream, client, &fds[1]),
        ] {
            if flow.pending() || flow.ended {
                if flow.send(to).is_err() {
                    return;
                }
                continue;
            }
            if !ready(polled) {
                continue;
            }
            match receive(from, &mut buffer) {
                Ok(None) => {}
                Ok(Some(0)) => {
                    flow.ended = true;
                    to.end_write();
                }
                Ok(Some(read)) => {
                    flow.buffer.extend_from_slice(&buffer[..read]);
                    if flow.send(to).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }
}

/// One plain http request and its response. An `Err` is answered to the
/// client, which got nothing so far.
fn exchange(
    client: &TcpStream,
    upstream: &Upstream,
    head: Vec<u8>,
    mut body: Body,
    rest: Vec<u8>,
    shared: &Shared,
    to: &Destination,
) -> Result<(), Answer> {
    let remote = matches!(upstream, Upstream::Remote { .. });
    let client_socket: &dyn Socket = client;
    let upstream = upstream.socket();
    let io_failed = |error: io::Error| Answer::gateway(format!("{to}: {error}"));
    upstream.unblock().map_err(io_failed)?;
    client_socket.unblock().map_err(io_failed)?;
    let answer = |answer: Answer| {
        let _ = client.set_nonblocking(false);
        Err(answer)
    };

    let mut up = Flow {
        buffer: head,
        ..Flow::default()
    };
    if body.feed(&rest, &mut up.buffer).is_err() {
        return answer(Answer::bad("a malformed chunked body"));
    }
    let mut down = Flow::default();
    let mut response = Response::default();
    let mut got_bytes = false;
    let mut buffer = vec![0_u8; BUFFER];
    loop {
        if down.done() {
            return Ok(());
        }
        let mut fds = [
            // Read on after the body too: the client's end ends the exchange,
            // and what it sends behind its request is thrown away.
            interest(client_socket.fd(), !up.pending(), down.pending()),
            interest(
                upstream.fd(),
                !down.ended && !down.pending(),
                up.pending() && !down.ended,
            ),
            poll_fd(shared.stop.as_raw_fd(), libc::POLLIN),
        ];
        if poll(&mut fds, None).is_err() || ready(&fds[2]) {
            return Ok(());
        }
        if down.pending() && down.send(client_socket).is_err() {
            return Ok(());
        }
        if ready(&fds[0]) && !up.pending() {
            match receive(client_socket, &mut buffer) {
                Ok(None) => {}
                Ok(Some(0)) | Err(_) => return Ok(()),
                Ok(Some(read)) => {
                    if body.feed(&buffer[..read], &mut up.buffer).is_err() {
                        if response.passing {
                            return Ok(());
                        }
                        return answer(Answer::bad("a malformed chunked body"));
                    }
                }
            }
        }
        // An upstream that answers early may stop reading: its answer counts.
        if up.pending() && !down.ended && up.send(upstream).is_err() {
            up = Flow::default();
            body = Body::None;
        }
        if ready(&fds[1]) && !down.ended && !down.pending() {
            match receive(upstream, &mut buffer) {
                Ok(None) => {}
                Ok(Some(read @ 1..)) => {
                    got_bytes = true;
                    match response.feed(&buffer[..read], &mut down.buffer) {
                        Ok(()) => {}
                        Err(ResponseError::ProxyLogin) => {
                            return answer(Answer::gateway(format!(
                                "{to} asked for a proxy login; only this browser's own proxy may"
                            )));
                        }
                        Err(ResponseError::NotHttp) => {
                            return answer(Answer::gateway(format!("{to} does not speak http")));
                        }
                    }
                    if down.send(client_socket).is_err() {
                        return Ok(());
                    }
                }
                Ok(Some(_)) | Err(_) => {
                    if !response.passing {
                        return answer(Answer::gateway(match (got_bytes, remote) {
                            (true, _) => format!("{to} ended its answer too early"),
                            (false, true) => format!(
                                "{} did not connect to {to} (nothing listens there, or its sshd refuses forwarding)",
                                shared.label()
                            ),
                            (false, false) => format!("{to} closed the connection without an answer"),
                        }));
                    }
                    down.ended = true;
                }
            }
        }
    }
}

fn poll_fd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

/// How many are ready; 0 after `timeout`.
fn poll(fds: &mut [libc::pollfd], timeout: Option<Duration>) -> io::Result<usize> {
    let timeout = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
    });
    loop {
        // `fds` is a valid array for the call's length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready >= 0 {
            return Ok(ready as usize);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Whether the socket has something to read (its end too) before the time is
/// over or the proxy stops.
fn wait_readable(fd: RawFd, shared: &Shared, timeout: Duration) -> bool {
    let mut fds = [
        poll_fd(fd, libc::POLLIN),
        poll_fd(shared.stop.as_raw_fd(), libc::POLLIN),
    ];
    matches!(poll(&mut fds, Some(timeout)), Ok(1..)) && fds[1].revents == 0
}

#[cfg(test)]
mod tests {
    use super::super::forward::Connections;
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const SECRET: &str = "s3cret~";

    type Handler = Arc<dyn Fn(&Destination, UnixStream) + Send + Sync>;

    /// The remote machine: every dial is a socket pair whose far end a thread
    /// serves.
    struct FakeDial {
        handler: Handler,
        error: Mutex<Option<DialError>>,
        log: Mutex<Vec<String>>,
        connections: Arc<Connections>,
    }

    impl Dial for FakeDial {
        fn dial(&self, to: &Destination) -> Result<RemoteConnection, DialError> {
            self.log.lock().unwrap().push(to.to_string());
            if let Some(error) = self.error.lock().unwrap().clone() {
                return Err(error);
            }
            let permit = self
                .connections
                .enter(Duration::ZERO)
                .ok_or(DialError::TooMany)?;
            let (near, far) = UnixStream::pair().unwrap();
            let (handler, to) = (Arc::clone(&self.handler), to.clone());
            std::thread::spawn(move || handler(&to, far));
            Ok(RemoteConnection {
                stream: near,
                permit,
            })
        }
    }

    /// Everything goes to the remote machine, except what a test says.
    #[derive(Default)]
    struct FakeClassifier {
        routes: Mutex<HashMap<String, Route>>,
        log: Mutex<Vec<String>>,
    }

    impl Classifier for FakeClassifier {
        fn canonical(&self, host: &str) -> Result<String, String> {
            match host {
                "127.1" => Ok("127.0.0.1".into()),
                "refused" => Err("no\r\nhost".into()),
                _ => Ok(host.trim_end_matches('.').to_ascii_lowercase()),
            }
        }

        fn route(&self, host: &str, port: u16) -> Route {
            self.log.lock().unwrap().push(format!("{host}:{port}"));
            self.routes
                .lock()
                .unwrap()
                .get(host)
                .cloned()
                .unwrap_or(Route::Remote)
        }
    }

    struct Rig {
        dial: Arc<FakeDial>,
        classifier: Arc<FakeClassifier>,
        proxy: Proxy,
    }

    impl Rig {
        fn new(handler: impl Fn(&Destination, UnixStream) + Send + Sync + 'static) -> Self {
            Self::with(false, handler)
        }

        fn with(
            remote_only: bool,
            handler: impl Fn(&Destination, UnixStream) + Send + Sync + 'static,
        ) -> Self {
            let dial = Arc::new(FakeDial {
                handler: Arc::new(handler),
                error: Mutex::default(),
                log: Mutex::default(),
                connections: Connections::new(200),
            });
            let classifier = Arc::new(FakeClassifier::default());
            let proxy = Proxy::start(
                0,
                ProxyOptions {
                    label: "build\nbox".into(),
                    instance: "i-1".into(),
                    secret: SECRET.into(),
                    remote_only,
                    classifier: Arc::clone(&classifier) as _,
                    dial: Arc::clone(&dial) as _,
                },
            )
            .unwrap();
            Self {
                dial,
                classifier,
                proxy,
            }
        }

        /// Every connection gets `answer` once its request's head is there.
        fn answering(answer: &'static str) -> Self {
            Self::new(move |_, mut far| {
                read_until(&mut far, b"\r\n\r\n");
                let _ = far.write_all(answer.as_bytes());
            })
        }

        fn connect(&self) -> TcpStream {
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.proxy.port())).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
        }

        /// Sends a request and reads to the end of the connection.
        fn send(&self, request: &str) -> String {
            let mut stream = self.connect();
            stream.write_all(request.as_bytes()).unwrap();
            read_all(&mut stream)
        }

        fn dialed(&self) -> Vec<String> {
            self.dial.log.lock().unwrap().clone()
        }

        fn classified(&self) -> Vec<String> {
            self.classifier.log.lock().unwrap().clone()
        }

        fn route(&self, host: &str, route: Route) {
            self.classifier
                .routes
                .lock()
                .unwrap()
                .insert(host.into(), route);
        }
    }

    fn auth() -> String {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{PROXY_USER}:{SECRET}"));
        format!("Proxy-Authorization: Basic {token}\r\n")
    }

    fn get(url: &str) -> String {
        format!("GET {url} HTTP/1.1\r\nHost: x\r\n{}\r\n", auth())
    }

    fn connect(to: &str) -> String {
        format!("CONNECT {to} HTTP/1.1\r\nHost: {to}\r\n{}\r\n", auth())
    }

    fn read_all(stream: &mut impl io::Read) -> String {
        let mut text = Vec::new();
        // A reset after the last byte is an end too.
        let _ = stream.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    }

    fn read_until(stream: &mut impl io::Read, end: &[u8]) -> Vec<u8> {
        let mut text = Vec::new();
        let mut byte = [0];
        while !text.ends_with(end) {
            match stream.read(&mut byte) {
                Ok(1) => text.push(byte[0]),
                _ => break,
            }
        }
        text
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// An address that refuses connections as long as the stream lives: its
    /// own local one.
    fn closed_port() -> (TcpStream, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let held = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let address = held.local_addr().unwrap();
        (held, address)
    }

    const HELLO: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";

    #[test]
    fn without_the_secret_nothing_but_407() {
        let rig = Rig::answering(HELLO);
        let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
        let wrong = [
            String::new(),
            format!("Proxy-Authorization: Basic {}\r\n", encode("herdr:other")),
            format!("Proxy-Authorization: Basic {}\r\n", encode("herdr:")),
            format!(
                "Proxy-Authorization: Basic {}\r\n",
                encode(&format!("user:{SECRET}"))
            ),
            format!(
                "Proxy-Authorization: Bearer {}\r\n",
                encode(&format!("herdr:{SECRET}"))
            ),
            "Proxy-Authorization: Basic ***\r\n".to_owned(),
            "Proxy-Authorization: Basic\r\n".to_owned(),
            format!("{0}{0}", auth()),
            auth().replace("Proxy-", ""),
        ];
        for target in [
            "GET http://localhost/",
            "GET http://herdr-warm.invalid/",
            "CONNECT localhost:443",
            "GET /",
        ] {
            for wrong in &wrong {
                let answer = rig.send(&format!("{target} HTTP/1.1\r\nHost: x\r\n{wrong}\r\n"));
                assert!(
                    answer.starts_with("HTTP/1.1 407 "),
                    "{target} {wrong}: {answer}"
                );
                assert!(answer.contains("\r\nProxy-Authenticate: Basic realm=\"herdr\"\r\n"));
                assert!(!answer.contains(WARM_HEADER));
            }
        }
        assert!(rig.dialed().is_empty() && rig.classified().is_empty());
        assert!(rig.send(&get("http://localhost/")).ends_with("hello"));
    }

    #[test]
    fn the_warm_up_is_answered_here_and_no_invalid_name_leaves() {
        let rig = Rig::answering(HELLO);
        for url in [
            "http://herdr-warm.invalid/",
            "http://HERDR-warm.invalid/x?y",
        ] {
            let answer = rig.send(&get(url));
            assert!(answer.starts_with("HTTP/1.1 204 "), "{answer}");
            assert!(answer.contains("\r\nx-herdr-proxy: i-1\r\n"), "{answer}");
            assert!(answer.ends_with("\r\n\r\n"));
        }
        for request in [
            get("http://herdr-proxy.invalid/"),
            get("http://herdr-warm.invalid:81/"),
            get("http://a.b.invalid/"),
            get("http://invalid/"),
            get("http://x.INVALID./"),
            connect("herdr-warm.invalid:443"),
            connect("herdr-warm.invalid:80"),
        ] {
            let answer = rig.send(&request);
            assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
        }
        assert!(rig.dialed().is_empty() && rig.classified().is_empty());
    }

    #[test]
    fn a_plain_request_is_rewritten_for_its_server() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rig = Rig::new({
            let seen = Arc::clone(&seen);
            move |_, mut far| {
                let head = read_until(&mut far, b"\r\n\r\n");
                seen.lock().unwrap().push(String::from_utf8(head).unwrap());
                far.write_all(HELLO.as_bytes()).unwrap();
            }
        });
        let answer = rig.send(&format!(
            "GET http://LocalHost:3000/a/b?c=%20d HTTP/1.1\r\nHost: other.example\r\n{}Proxy-Connection: keep-alive\r\nConnection: keep-alive, X-Hop\r\nX-Hop: 1\r\nKeep-Alive: 5\r\nTE: trailers\r\nUpgrade: websocket\r\nUpgrade-Insecure-Requests: 1\r\nCookie: a=b\r\nExpect: 100-continue\r\nX-Text: \u{e9}\r\n\r\n",
            auth()
        ));
        assert_eq!(
            seen.lock().unwrap()[0],
            "GET /a/b?c=%20d HTTP/1.1\r\nHost: localhost:3000\r\nupgrade-insecure-requests: 1\r\ncookie: a=b\r\nx-text: \u{e9}\r\nconnection: close\r\n\r\n"
        );
        assert_eq!(
            answer,
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
        );
        for (url, first) in [
            ("http://localhost", "GET / HTTP/1.1\r\nHost: localhost\r\n"),
            (
                "http://localhost:80?x",
                "GET /?x HTTP/1.1\r\nHost: localhost\r\n",
            ),
            (
                "HTTP://[::1]:8080/",
                "GET / HTTP/1.1\r\nHost: [::1]:8080\r\n",
            ),
            ("http://127.1/", "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n"),
            (
                "http://Example.COM./",
                "GET / HTTP/1.1\r\nHost: example.com\r\n",
            ),
        ] {
            rig.send(&get(url));
            let seen = seen.lock().unwrap();
            assert!(seen.last().unwrap().starts_with(first), "{seen:?}");
        }
        assert_eq!(
            rig.dialed(),
            [
                "localhost:3000",
                "localhost:80",
                "localhost:80",
                "[::1]:8080",
                "127.0.0.1:80",
                "example.com:80"
            ]
        );
        assert_eq!(rig.classified(), rig.dialed());
        // What is no host is refused in the classifier's words, on one line.
        for request in [get("http://refused/"), connect("refused:443")] {
            let answer = rig.send(&request);
            assert!(answer.starts_with("HTTP/1.1 400 "), "{answer}");
            assert!(answer.ends_with("\r\n\r\nno  host"), "{answer}");
        }
    }

    #[test]
    fn every_request_is_routed_by_its_own_host() {
        let rig = Rig::answering(HELLO);
        // The second, pipelined request on a connection is never read.
        let answer = rig.send(&format!("{}{}", get("http://one/"), get("http://two/")));
        assert_eq!(answer.matches("HTTP/1.1").count(), 1);
        assert_eq!(rig.dialed(), ["one:80"]);
        rig.send(&get("http://two/"));
        // No upstream connection is used twice, not even for the same host.
        rig.send(&get("http://two/"));
        assert_eq!(rig.dialed(), ["one:80", "two:80", "two:80"]);
        assert_eq!(rig.classified(), rig.dialed());
    }

    #[test]
    fn bodies_go_as_framed_and_not_a_byte_more() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rig = Rig::new({
            let seen = Arc::clone(&seen);
            move |_, mut far| {
                let mut head = String::from_utf8(read_until(&mut far, b"\r\n\r\n")).unwrap();
                let body = if head.contains("chunked") {
                    read_until(&mut far, b"0\r\n\r\n")
                } else {
                    let mut body = vec![0; 5];
                    far.read_exact(&mut body).unwrap();
                    body
                };
                head.push_str(std::str::from_utf8(&body).unwrap());
                // Whatever follows would be a second request.
                far.set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut more = [0; 64];
                if let Ok(read @ 1..) = far.read(&mut more) {
                    head.push_str(&format!("MORE {read}"));
                }
                seen.lock().unwrap().push(head);
                far.write_all(HELLO.as_bytes()).unwrap();
            }
        });
        let post = |framing: &str, body: &str| {
            format!(
                "POST http://h/ HTTP/1.1\r\n{}{framing}\r\n\r\n{body}",
                auth()
            )
        };
        let smuggled = "GET http://evil/ HTTP/1.1\r\n\r\n";
        let answer = rig.send(&post("Content-Length: 5", &format!("12345{smuggled}")));
        assert!(answer.ends_with("hello"), "{answer}");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap(),
            "POST / HTTP/1.1\r\nHost: h\r\ncontent-length: 5\r\nconnection: close\r\n\r\n12345"
        );
        // Chunks are written anew: extensions and trailers stay behind.
        let chunked = "3;x=y\r\nabc\r\n0A \r\n0123456789\r\n0\r\nX-Trailer: 1\r\n\r\n";
        let answer = rig.send(&post(
            "Transfer-Encoding: Chunked",
            &format!("{chunked}{smuggled}"),
        ));
        assert!(answer.ends_with("hello"), "{answer}");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap(),
            "POST / HTTP/1.1\r\nHost: h\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n3\r\nabc\r\na\r\n0123456789\r\n0\r\n\r\n"
        );
        // Piece by piece as well.
        let mut stream = rig.connect();
        for byte in post("Transfer-Encoding: chunked", "4\r\nabcd\r\n0\r\n\r\n").bytes() {
            stream.write_all(&[byte]).unwrap();
        }
        assert!(read_all(&mut stream).ends_with("hello"));
        assert!(seen
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .ends_with("4\r\nabcd\r\n0\r\n\r\n"));
        let dialed = rig.dialed().len();
        for framing in [
            "Content-Length: 5\r\nTransfer-Encoding: chunked",
            "Content-Length: 5\r\nContent-Length: 5",
            "Content-Length: 5, 5",
            "Content-Length: +5",
            "Content-Length: -1",
            "Content-Length:",
            "Content-Length: 99999999999999999999",
            "Transfer-Encoding: gzip, chunked",
            "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked",
            "Transfer-Encoding: identity",
            " Content-Length: 5",
            "Content-Length : 5",
            "X: a\r\n b",
        ] {
            let answer = rig.send(&post(framing, "12345"));
            assert!(answer.starts_with("HTTP/1.1 400 "), "{framing}: {answer}");
        }
        assert_eq!(rig.dialed().len(), dialed);
        for body in ["x\r\n", "\r\n", "-1\r\n", "3\r\nabcd\r\n", "3\nabc\r\n"] {
            let answer = rig.send(&post("Transfer-Encoding: chunked", body));
            assert!(answer.starts_with("HTTP/1.1 400 "), "{body:?}: {answer}");
        }
    }

    #[test]
    fn malformed_requests_reach_nobody() {
        let rig = Rig::answering(HELLO);
        for (line, status) in [
            ("GET / HTTP/1.1", "400"),
            ("GET https://h/ HTTP/1.1", "400"),
            ("GET ftp://h/ HTTP/1.1", "400"),
            ("GET http:///x HTTP/1.1", "400"),
            ("GET http://u@h/ HTTP/1.1", "400"),
            ("GET http://u:p@h/ HTTP/1.1", "400"),
            ("GET http://h:/ HTTP/1.1", "400"),
            ("GET http://h:0/ HTTP/1.1", "400"),
            ("GET http://h:65536/ HTTP/1.1", "400"),
            ("GET http://h:8a/ HTTP/1.1", "400"),
            ("GET http://h/#f HTTP/1.1", "400"),
            ("GET http://-h/ HTTP/1.1", "400"),
            ("GET http://h%2f/ HTTP/1.1", "400"),
            ("GET http://[::1/ HTTP/1.1", "400"),
            ("GET http://[fe80::1%25en0]/ HTTP/1.1", "400"),
            ("GET  http://h/ HTTP/1.1", "400"),
            ("GET http://h/ HTTP/2.0", "400"),
            ("GET http://h/ x HTTP/1.1", "400"),
            ("G\u{e9}T http://h/ HTTP/1.1", "400"),
            ("GET http://h/\u{e9} HTTP/1.1", "400"),
            ("CONNECT h HTTP/1.1", "400"),
            ("CONNECT h: HTTP/1.1", "400"),
            ("CONNECT h:0 HTTP/1.1", "400"),
            ("CONNECT h:70000 HTTP/1.1", "400"),
            ("CONNECT /tmp/x:80 HTTP/1.1", "400"),
            ("CONNECT -oX:80 HTTP/1.1", "400"),
            ("CONNECT ::1:80 HTTP/1.1", "400"),
            ("CONNECT http://h:80 HTTP/1.1", "400"),
        ] {
            let answer = rig.send(&format!("{line}\r\n{}\r\n", auth()));
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status} ")),
                "{line}: {answer}"
            );
        }
        for head in [
            "GET http://h/ HTTP/1.1\r\nA: b\nC: d\r\n",
            "GET http://h/ HTTP/1.1\r\nA: b\rc\r\n",
            "GET http://h/ HTTP/1.1\r\nA: \0\r\n",
            "GET http://h/ HTTP/1.1\r\nA b: c\r\n",
            "GET http://h/ HTTP/1.1\r\nnothing\r\n",
        ] {
            let answer = rig.send(&format!("{head}{}\r\n", auth()));
            assert!(answer.starts_with("HTTP/1.1 400 "), "{head:?}: {answer}");
        }
        let answer = rig.send(&format!(
            "GET http://h/ HTTP/1.1\r\nA: {}",
            "x".repeat(80_000)
        ));
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");
        assert!(rig.dialed().is_empty() && rig.classified().is_empty());
    }

    #[test]
    fn response_heads_are_rewritten_and_bodies_are_not() {
        for (sent, got) in [
            (
                "HTTP/1.1 200 OK\r\nConnection: keep-alive, X-A\r\nX-A: 1\r\nKeep-Alive: 5\r\nUpgrade: h2c\r\nProxy-Authenticate: Basic\r\nProxy-X: 1\r\nx-herdr-proxy: fake\r\n folded\r\nTransfer-Encoding: chunked\r\nSet-Cookie: a\r\nSet-Cookie: b\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nSet-Cookie: a\r\nSet-Cookie: b\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
            ),
            (
                "HTTP/1.0 404 Not Found\r\n\r\nraw \r\n\r\n HTTP/1.1 407 x\r\n\r\n",
                "HTTP/1.0 404 Not Found\r\nConnection: close\r\n\r\nraw \r\n\r\n HTTP/1.1 407 x\r\n\r\n",
            ),
            (
                "HTTP/1.1 103 Early Hints\r\nLink: x\r\n\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204\r\nA: b\r\n\r\n",
                "HTTP/1.1 204\r\nA: b\r\nConnection: close\r\n\r\n",
            ),
        ] {
            let rig = Rig::new(move |_, mut far| {
                read_until(&mut far, b"\r\n\r\n");
                for piece in sent.as_bytes().chunks(7) {
                    let _ = far.write_all(piece);
                }
            });
            assert_eq!(rig.send(&get("http://h/")), got);
        }
    }

    #[test]
    fn an_upstream_proxy_login_never_reaches_the_browser() {
        for sent in [
            "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"herdr\"\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 407 x\r\nProxy-Authenticate: Basic\r\n\r\n",
        ] {
            let answer = Rig::answering(sent).send(&get("http://h/"));
            assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
            assert!(!answer.to_lowercase().contains("proxy-authenticate"), "{answer}");
        }
        for sent in [
            "HTTP/1.1 101 Switching Protocols\r\n\r\n",
            "HTTP/1.1 999 x\r\n\r\n",
            "HTTP/1.1 2000 x\r\n\r\n",
            "HTTP/2 200 x\r\n\r\n",
            "SSH-2.0-OpenSSH_10.2\r\n",
            "HTTP/1.1 200 OK\r\nA: b\nC: d\r\n\r\n",
        ] {
            let answer = Rig::answering(sent).send(&get("http://h/"));
            assert!(answer.starts_with("HTTP/1.1 502 "), "{sent:?}: {answer}");
            assert!(answer.ends_with("h:80 does not speak http"), "{answer}");
        }
        let answer = Rig::answering("HTTP/1.1 200 OK\r\nA:").send(&get("http://h/"));
        assert!(
            answer.ends_with("h:80 ended its answer too early"),
            "{answer}"
        );
    }

    #[test]
    fn a_failed_dial_is_an_error_page() {
        let rig = Rig::new(|_, far| drop(far));
        let answer = rig.send(&get("http://localhost:3000/"));
        assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
        assert!(
            answer.ends_with("\r\n\r\nbuild box did not connect to localhost:3000 (nothing listens there, or its sshd refuses forwarding)"),
            "{answer}"
        );
        // A tunnel is open before the dial is known: it ends.
        assert_eq!(
            rig.send(&connect("localhost:3000")),
            "HTTP/1.1 200 Connection Established\r\n\r\n"
        );
        assert_eq!(rig.dialed().len(), 2, "never retried");
        for (error, text) in [
            (DialError::Down, "build box is not connected"),
            (DialError::TooMany, "too many open connections through build box; close some pages"),
            (DialError::Unmanaged, "browsing through build box needs herdr's managed ssh connection (remote.manage_ssh_config)"),
            (DialError::Failed("a\r\nb: c".into()), "build box could not be reached: a  b: c"),
        ] {
            *rig.dial.error.lock().unwrap() = Some(error);
            for request in [get("http://localhost/"), connect("localhost:443")] {
                let answer = rig.send(&request);
                assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
                assert!(answer.ends_with(&format!("\r\n\r\n{text}")), "{answer}");
                assert!(!answer.contains("\r\nb: c"));
            }
        }
    }

    #[test]
    fn the_answer_to_a_large_upload_arrives() {
        let rig = Rig::new(|_, far| drop(far));
        let mut stream = rig.connect();
        let mut reader = stream.try_clone().unwrap();
        let reading = std::thread::spawn(move || read_all(&mut reader));
        let head = "POST http://h/ HTTP/1.1\r\nContent-Length: 100000\r\n\r\n";
        let _ = stream.write_all(head.as_bytes());
        for _ in 0..100 {
            let _ = stream.write_all(&[b'x'; 1000]);
        }
        let answer = reading.join().unwrap();
        assert!(answer.starts_with("HTTP/1.1 407 "), "{answer}");
    }

    #[test]
    fn a_tunnel_carries_both_directions_and_their_ends() {
        let rig = Rig::new(|_, mut far| {
            let mut text = Vec::new();
            far.read_to_end(&mut text).unwrap();
            far.write_all(b"got ").unwrap();
            far.write_all(&text).unwrap();
        });
        let mut stream = rig.connect();
        // What comes with the head belongs to the tunnel.
        stream
            .write_all(format!("{}early ", connect("[::1]:443")).as_bytes())
            .unwrap();
        stream.write_all(b"late").unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        assert_eq!(
            read_all(&mut stream),
            "HTTP/1.1 200 Connection Established\r\n\r\ngot early late"
        );
        assert_eq!(rig.dialed(), ["[::1]:443"]);
    }

    #[test]
    fn a_tunnel_does_not_wait_for_one_direction() {
        const SIZE: usize = 4 * 1024 * 1024;
        // Both ends write everything before they read anything.
        let rig = Rig::new(|_, far| {
            let mut writer = far.try_clone().unwrap();
            let writing = std::thread::spawn(move || writer.write_all(&vec![b'd'; SIZE]).unwrap());
            std::thread::sleep(Duration::from_millis(100));
            let mut got = 0;
            let mut buffer = vec![0; 65536];
            let mut far = far;
            while let Ok(read @ 1..) = far.read(&mut buffer) {
                got += read;
            }
            assert_eq!(got, SIZE);
            writing.join().unwrap();
        });
        let mut stream = rig.connect();
        stream.write_all(connect("h:443").as_bytes()).unwrap();
        read_until(&mut stream, b"\r\n\r\n");
        let mut writer = stream.try_clone().unwrap();
        let writing = std::thread::spawn(move || {
            writer.write_all(&vec![b'u'; SIZE]).unwrap();
            writer.shutdown(Shutdown::Write).unwrap();
        });
        let mut got = vec![0; SIZE];
        stream.read_exact(&mut got).unwrap();
        assert!(got.iter().all(|&byte| byte == b'd'));
        writing.join().unwrap();
    }

    #[test]
    fn a_stream_arrives_as_it_is_sent() {
        let rig = Rig::new(|_, mut far| {
            read_until(&mut far, b"\r\n\r\n");
            far.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: 1\n\n")
                .unwrap();
            // Ends with its client.
            let mut byte = [0];
            let _ = far.read(&mut byte);
        });
        let mut stream = rig.connect();
        stream.write_all(get("http://h/events").as_bytes()).unwrap();
        let text = read_until(&mut stream, b"data: 1\n\n");
        assert!(text.ends_with(b"data: 1\n\n"));
        let open = || rig.dial.connections.enter(Duration::ZERO).is_some();
        assert!(open());
        drop(stream);
        // The client is gone: so is the upstream connection and its place.
        until("the upstream ends", || {
            Arc::strong_count(&rig.dial.connections) == 1
        });
    }

    #[test]
    fn public_destinations_are_dialed_here_at_the_pinned_address() {
        let rig = Rig::answering("HTTP/1.1 200 OK\r\n\r\nremote");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                read_until(&mut stream, b"\r\n\r\n");
                stream.write_all(b"HTTP/1.1 200 OK\r\n\r\ndirect").unwrap();
            }
        });
        // The first address refuses: the next one is tried.
        let (_held, closed) = closed_port();
        rig.route("public.example", Route::Direct(vec![closed, address]));
        assert!(rig.send(&get("http://public.example/")).ends_with("direct"));
        let mut stream = rig.connect();
        stream
            .write_all(format!("{}x\r\n\r\n", connect("public.example:443")).as_bytes())
            .unwrap();
        assert!(read_all(&mut stream).ends_with("direct"));
        assert!(rig.dialed().is_empty());

        // Neither side stands in for the other.
        rig.route("public.example", Route::Direct(vec![closed]));
        for request in [get("http://public.example/"), connect("public.example:443")] {
            let answer = rig.send(&request);
            assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
            assert!(
                answer.contains("could not connect to public.example:"),
                "{answer}"
            );
        }
        rig.route("none.example", Route::Direct(Vec::new()));
        assert!(rig
            .send(&get("http://none.example/"))
            .starts_with("HTTP/1.1 502 "));
        rig.route("bad.example", Route::Refuse("not for browsers".into()));
        assert!(rig
            .send(&get("http://bad.example/"))
            .ends_with("bad.example:80: not for browsers"));
        assert!(rig
            .send(&connect("bad.example:443"))
            .starts_with("HTTP/1.1 502 "));
        assert!(rig.dialed().is_empty());
        assert!(rig.send(&get("http://localhost:3000/")).ends_with("remote"));
        assert_eq!(rig.dialed(), ["localhost:3000"]);
        // Plain http routes a name each time it is asked for.
        assert_eq!(
            rig.classified()
                .iter()
                .filter(|c| c.as_str() == "public.example:80")
                .count(),
            2
        );
    }

    #[test]
    fn the_time_for_a_dial_is_one_for_all_addresses() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        assert!(connect_any(&[address], Duration::from_secs(5)).is_ok());
        listener.accept().unwrap();
        listener.set_nonblocking(true).unwrap();
        let error = connect_any(&[address; 100], Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(listener.accept().is_err(), "nothing was dialed");
        let (_held, closed) = closed_port();
        let started = Instant::now();
        let error = connect_any(&[closed; 100], Duration::from_secs(60)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn addresses_that_never_answer_share_the_time() {
        // A listener whose backlog is full drops what comes: connects hang.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let mut queued = Vec::new();
        while let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_millis(200)) {
            queued.push(stream);
            assert!(queued.len() < 10_000);
        }
        let started = Instant::now();
        let error = connect_any(&[address; 20], Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let took = started.elapsed();
        assert!(
            took >= Duration::from_secs(2) && took < Duration::from_millis(4500),
            "{took:?}"
        );
    }

    #[test]
    fn an_empty_secret_opens_nothing() {
        let request = |line: &str| Request::parse(line.as_bytes()).ok().unwrap();
        let token = base64::engine::general_purpose::STANDARD.encode("herdr:");
        let empty = request(&format!(
            "GET http://h/ HTTP/1.1\r\nProxy-Authorization: Basic {token}"
        ));
        assert!(!authorized(&empty, ""));
        let token = base64::engine::general_purpose::STANDARD.encode("herdr:x");
        let set = request(&format!(
            "GET http://h/ HTTP/1.1\r\nProxy-Authorization: basic  {token}"
        ));
        assert!(authorized(&set, "x"));
        assert!(!authorized(&set, "y"));
    }

    #[test]
    fn remote_egress_sends_everything_to_the_remote_machine() {
        let rig = Rig::with(true, |_, mut far| {
            read_until(&mut far, b"\r\n\r\n");
            let _ = far.write_all(HELLO.as_bytes());
        });
        rig.route("public.example", Route::Direct(Vec::new()));
        rig.route("bad.example", Route::Refuse("no".into()));
        for host in ["public.example", "bad.example"] {
            assert!(rig
                .send(&get(&format!("http://{host}/")))
                .ends_with("hello"));
        }
        assert_eq!(rig.dialed(), ["public.example:80", "bad.example:80"]);
        assert!(rig.classified().is_empty());
    }

    #[test]
    fn the_port_is_kept_and_only_loopback_listens() {
        let rig = Rig::answering(HELLO);
        let port = rig.proxy.port();
        let mut open = rig.connect();
        open.write_all(connect("h:443").as_bytes()).unwrap();
        read_until(&mut open, b"\r\n\r\n");
        assert!(
            TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err(),
            "the port is taken"
        );
        let Rig {
            dial,
            classifier,
            proxy,
        } = rig;
        drop(proxy);
        // Its connections end with it.
        assert_eq!(read_all(&mut open), "");
        let proxy = Proxy::start(
            port,
            ProxyOptions {
                label: String::new(),
                instance: "i-2".into(),
                secret: SECRET.into(),
                remote_only: false,
                classifier: Arc::clone(&classifier) as _,
                dial: Arc::clone(&dial) as _,
            },
        )
        .unwrap();
        assert_eq!(proxy.port(), port);
        let rig = Rig {
            dial,
            classifier,
            proxy,
        };
        assert!(rig
            .send(&get("http://herdr-warm.invalid/"))
            .contains("x-herdr-proxy: i-2"));
        for (secret, instance) in [("", "i"), ("s", ""), ("s", " \n ")] {
            let refused = Proxy::start(
                0,
                ProxyOptions {
                    label: String::new(),
                    instance: instance.into(),
                    secret: secret.into(),
                    remote_only: false,
                    classifier: Arc::clone(&rig.classifier) as _,
                    dial: Arc::clone(&rig.dial) as _,
                },
            );
            assert_eq!(refused.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn many_connections_at_once() {
        let rig = Arc::new(Rig::answering(HELLO));
        let sends = (0..100)
            .map(|n| {
                let rig = Arc::clone(&rig);
                std::thread::spawn(move || rig.send(&get(&format!("http://host{n}/"))))
            })
            .collect::<Vec<_>>();
        for send in sends {
            assert!(send.join().unwrap().ends_with("hello"));
        }
        assert_eq!(rig.dialed().len(), 100);
        until("every place is free again", || {
            Arc::strong_count(&rig.dial.connections) == 1
        });
    }
}
