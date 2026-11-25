//! The browser link, version 1 (fork): what a herdr server (B) and the helper
//! on the user's Mac (A, `herdr browser-link <machine>`) say to each other,
//! as types and data only. No I/O, no clock, no platform code.
//!
//! The link is opened with `browser.link_open` on B's API socket; after the
//! answer the connection carries JSON lines ([`HelperFrame`] A→B,
//! [`ServerFrame`] B→A). The server validates nothing A must protect: every
//! op passes A's gate, whose rules are [`OPS`], [`PRESS_TABLE`] and
//! [`RefuseRule`].
//!
//! Frozen as link protocol 1 with the fixtures in
//! `tests/fixtures/browser-link-v1/`: a later frame, op or argument needs a
//! feature string ([`BrowserLinkOpenParams::features`]), a changed one a new
//! [`LINK_PROTOCOL`].

// the users of these types (server, helper, shell) land after them; X1 takes
// this out
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::host::{HostError, HostReply, PageInfo};
use super::state::HostTab;

// ---------------------------------------------------------------------------
// Names

/// The frames' version, `link_protocol` in `browser.link_open`.
pub const LINK_PROTOCOL: u32 = 1;
/// The endpoint capability (`EndpointServerWelcome.capabilities`) a client
/// looks for before it starts a helper; never detected by an error code.
pub const LINK_CAPABILITY: &str = "browser-link.v1";
/// The API method that turns its connection into the link.
pub const LINK_OPEN_METHOD: &str = "browser.link_open";
/// Reserved, unused in version 1.
pub const LINK_DATA_METHOD: &str = "browser.link_data";
/// The features this build offers and accepts (none in version 1).
pub const LINK_FEATURES: &[&str] = &[];

/// The API methods B runs for the companion (`api` frames); every other is
/// refused. B puts its own lease id in.
pub const API_METHODS: &[&str] = &[
    "browser.host_state",
    "browser.host_sync",
    "browser.host_report",
    "browser.send_page",
    "tab.focus",
    "events.subscribe",
];

/// The event kinds an `events.subscribe` over the link may name
/// (`EVENT_KINDS` in the companion's `host.js`).
pub const EVENT_KINDS: &[&str] = &[
    "tab.focused",
    "workspace.focused",
    "tab.created",
    "tab.closed",
    "tab.renamed",
    "tab.moved",
    "workspace.closed",
    "workspace.renamed",
];

/// The event the helper itself puts on the companion's events stream when
/// the link is back: a full resync, no new registration.
pub const EVENT_LINK_UP: &str = "herdr.link_up";

/// The scheme of a shown file's page: `herdr-file://<token>/<rel>`.
pub const FILE_SCHEME: &str = "herdr-file";
/// A file token's length ([`is_file_token`]).
pub const FILE_TOKEN_LEN: usize = 24;

/// `[a-z][a-z0-9]{23}`: a URL host that neither folds nor reads as an address.
pub fn is_file_token(token: &str) -> bool {
    token.len() == FILE_TOKEN_LEN
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Limits

/// A pings this often.
pub const PING_MS: u64 = 3_000;
/// B drops the link after this much silence.
pub const TTL_MS: u64 = 15_000;
/// A drops the link after this much silence.
pub const HELPER_SILENCE_MS: u64 = 12_000;
/// A does not run an `op` or `ensure` whose `echo` ping is older than this,
/// on a clock that counts sleep.
pub const STALE_ECHO_MS: u64 = 15_000;
/// B waits this long for an `ensure_re`.
pub const ENSURE_MS: u64 = 45_000;
/// B waits an op's deadline, the sidecar's `REPLY_GRACE` and this.
pub const REPLY_EXTRA_MS: u64 = 2_000;
/// The read timeout of B's link thread.
pub const READ_POLL_MS: u64 = 250;
/// A's first and longest wait before it opens a dropped link again.
pub const RECONNECT_FIRST_MS: u64 = 500;
pub const RECONNECT_MAX_MS: u64 = 5_000;
/// A's wait after `link_in_use`.
pub const IN_USE_RETRY_MS: u64 = 30_000;

/// The longest line, without its newline; a longer one drops the link.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// The bytes one `blob` or `file_re` carries, before base64.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// The unacknowledged `blob` chunks on the link, all ops together: past it
/// the next one waits for a `blob_ack`, however late (amendment A5, measured
/// on a relayed link of about 120 ms).
pub const BLOB_WINDOW: u64 = 4;
/// The unanswered `file` requests on the link: the answers pace the next
/// ones (amendment A5).
pub const FILE_WINDOW: usize = 4;
/// The largest blob, all chunks.
pub const BLOB_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The largest file B serves.
pub const FILE_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The companion's pulls the helper keeps, on the link or waiting for it.
pub const FILE_PULLS: usize = 16;
/// Ops waiting for their answer; one more is `host_busy`.
pub const OPS_IN_FLIGHT: usize = 32;
/// Bytes waiting in B's writer; more is `host_busy`.
pub const QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// The longest `eval` expression.
pub const EVAL_MAX_BYTES: usize = 256 * 1024;
/// The most keys of a `release`.
pub const RELEASE_MAX_KEYS: usize = 64;
/// The longest an activity frame may stay (longer is cut to it).
pub const ACTIVITY_MAX_MS: u64 = 600_000;
/// The longest string of an activity directive (longer is cut to it).
pub const ACTIVITY_MAX_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// `browser.link_open` and what `browser.get` says about the link

/// Whether A can run a browser (`browser.link_open`). Append-closed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLinkReady {
    #[default]
    Ready,
    /// The companion is not installed on A.
    NotInstalled,
    /// It is, but cannot start; `detail` says why.
    Failed,
    /// A newer helper's value: treated as `failed`.
    #[serde(other)]
    Unknown,
}

/// `browser.link_open`: A offers its browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserLinkOpenParams {
    pub link_protocol: u32,
    /// Random per Mac, kept; the same one replaces its own link.
    pub host_id: String,
    /// Random per companion launch; another one is a new set of pages.
    pub instance: String,
    /// The Mac's name, for texts.
    #[serde(default)]
    pub label: String,
    /// The helper's herdr version.
    #[serde(default)]
    pub herdr: String,
    #[serde(default)]
    pub features: Vec<String>,
    pub ready: BrowserLinkReady,
    /// Why it is not ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Reserved, always false in version 1.
    #[serde(default, skip_serializing_if = "is_false")]
    pub takeover: bool,
}

/// The opened link (`BrowserLinkOpened { link }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserLinkInfo {
    pub link_id: String,
    /// Per server process, one more per accepted open. Not on any frame: both
    /// sides key what is pending by `(epoch, id)`.
    pub epoch: u64,
    /// Random per server process.
    pub boot_id: String,
    pub link_protocol: u32,
    /// The features both sides have.
    #[serde(default)]
    pub features: Vec<String>,
    pub ping_ms: u64,
    pub ttl_ms: u64,
}

/// `BrowserGetInfo.link`: the Mac this server's browser runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserLinkStatus {
    /// `linked`, `reconnecting` (the link dropped, its lease still lives) or
    /// `gone` (the last one); absent while there never was one.
    pub state: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub herdr: String,
    /// When the state was entered.
    #[serde(default)]
    pub since_unix: u64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

// ---------------------------------------------------------------------------
// Frames

/// An error in a reply frame: the socket error code, its text and the tab a
/// failed `open` still created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinkError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl LinkError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            target: None,
        }
    }
}

/// The page after an op (the sidecar's `page`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinkPage {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub dialog_open: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// `ensure`: B needs the browser (A starts its companion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ensure {
    pub id: u64,
    /// The last ping B saw, 0 for none ([`STALE_ECHO_MS`]).
    pub echo: u64,
    /// What needs it, for A's log.
    #[serde(default)]
    pub reason: String,
}

/// `ensure_re`: the browser is up, or why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnsureReply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LinkError>,
}

/// `host_up`: a companion and its sidecar serve ops; its open pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostUp {
    /// The companion launch: the same one after a link drop reopens the same
    /// targets.
    pub instance: String,
    /// The companion's app version.
    #[serde(default)]
    pub version: String,
    /// Its Electron version.
    #[serde(default)]
    pub electron: String,
    #[serde(default)]
    pub tabs: Vec<HostTab>,
}

/// `op`: one sidecar request (`HostRequest` without `profile`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkOp {
    pub id: u64,
    /// The last ping B saw, 0 for none ([`STALE_ECHO_MS`]).
    pub echo: u64,
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// [`OPS`] names them; null means absent, here and in every argument.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub args: Value,
    pub deadline_ms: u64,
    /// A [`LinkActivity`], kept open so that a bad one is dropped by the
    /// gate, not the link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Value>,
}

/// `re`: an op's answer (`HostReply`); all its blobs came before it and are
/// not in its `result`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkReply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<LinkPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LinkError>,
}

/// Which picture of a screenshot a blob is: `result.full` or `result.inline`
/// of the sidecar's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobName {
    Full,
    Inline,
    /// A newer helper's: acknowledged and dropped.
    #[serde(other)]
    Unknown,
}

/// `blob`: one chunk of a picture. The sidecar answers a screenshot with
/// base64 in `result.full` and `result.inline`; the helper takes both out of
/// the result, sends their bytes as chunks ([`BLOB_WINDOW`]) and the `re`
/// after the last ack. No file is written on A.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobChunk {
    /// The id of the `op` it belongs to.
    pub op: u64,
    pub name: BlobName,
    /// From 0 per op and name.
    pub seq: u64,
    /// At most [`CHUNK_BYTES`], base64 on the wire.
    #[serde(with = "chunk")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub last: bool,
    /// The whole blob's bytes, the same in every chunk.
    pub size: u64,
}

/// `blob_ack`: B has written this chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobAck {
    pub op: u64,
    pub name: BlobName,
    pub seq: u64,
}

/// `file`: A pulls the next chunk of a shown file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRequest {
    pub id: u64,
    /// The entry's current `serve` token.
    pub token: String,
    /// The URL's path, still percent-encoded, without the leading `/`.
    pub path: String,
    pub offset: u64,
}

/// `file_re`: the chunk at that offset, or an HTTP status that is not 200.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReply {
    pub id: u64,
    /// 200, 403 or 404.
    pub status: u16,
    #[serde(default)]
    pub mime: String,
    /// The whole file's bytes.
    #[serde(default)]
    pub size: u64,
    /// At most [`CHUNK_BYTES`], base64 on the wire.
    #[serde(default, with = "chunk")]
    pub data: Vec<u8>,
    #[serde(default)]
    pub eof: bool,
}

/// `api`: a call of the companion ([`API_METHODS`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiRequest {
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

/// `api_re`: the `result` object of herdr's API answer (with its `type`), or
/// its error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiReply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LinkError>,
}

/// `bye`: the sender ends the link ([`bye`]); nothing follows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// Why a side ends the link (`Bye.code`).
pub mod bye {
    /// The other side sent a line that is no frame ([`super::FrameError`]).
    pub const PROTOCOL: &str = "protocol";
    /// B: the same Mac opened a newer link.
    pub const REPLACED: &str = "replaced";
    /// B: the server stops or hands off.
    pub const STOPPING: &str = "stopping";
    /// A: the helper ends (the last client quit).
    pub const QUIT: &str = "quit";
    /// B: the config turned the browser or its link off; reopening gets
    /// `browser_disabled` or `link_disabled` until it is on again.
    pub const DISABLED: &str = "disabled";
}

/// A frame the helper sends (A→B).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HelperFrame {
    /// Counts from 1; B's `op` and `ensure` echo the last one.
    Ping {
        n: u64,
    },
    EnsureRe(EnsureReply),
    HostUp(HostUp),
    /// The companion or its sidecar is gone; `reason` for B's log.
    HostDown {
        #[serde(default)]
        reason: String,
    },
    Re(LinkReply),
    /// A sidecar event (`HostEvent`), as it came; B forces its `profile`.
    Ev {
        event: Value,
    },
    Blob(BlobChunk),
    File(FileRequest),
    Api(ApiRequest),
    Bye(Bye),
    /// A newer helper's frame: ignored.
    #[serde(other)]
    Unknown,
}

/// A frame the server sends (B→A).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    Pong {
        n: u64,
    },
    Ensure(Ensure),
    /// The answer to `host_up`.
    HostOk {
        lease_id: String,
    },
    Op(LinkOp),
    BlobAck(BlobAck),
    FileRe(FileReply),
    ApiRe(ApiReply),
    /// One line of B's events stream (`EventEnvelope`), as it came.
    Event {
        event: Value,
    },
    Bye(Bye),
    /// A newer server's frame: ignored.
    #[serde(other)]
    Unknown,
}

/// The writer's order: everything waiting in an earlier lane goes first;
/// within a lane the order stays. A `re` is queued only after its blobs were
/// acknowledged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WriteLane {
    /// `ping`, `pong`.
    Alive,
    /// Everything else.
    Work,
    /// `blob`, `file`, `file_re`.
    Bulk,
}

/// Why a line is no frame; either one drops the link (`bye` `protocol`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Over [`MAX_LINE_BYTES`].
    TooLong,
    /// No JSON object, no `t`, or a known frame with a wrong field.
    Malformed(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLong => write!(f, "a line over {MAX_LINE_BYTES} bytes"),
            FrameError::Malformed(why) => write!(f, "not a link frame: {why}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl HelperFrame {
    /// One line (its newline may still be on it). An unknown `t` is
    /// [`HelperFrame::Unknown`], unknown fields are ignored.
    pub fn decode(line: &[u8]) -> Result<Self, FrameError> {
        decode(line)
    }

    /// The line with its newline; too long is the caller's to answer.
    pub fn encode(&self) -> Result<String, FrameError> {
        encode(self)
    }

    pub fn lane(&self) -> WriteLane {
        match self {
            HelperFrame::Ping { .. } => WriteLane::Alive,
            HelperFrame::Blob(_) | HelperFrame::File(_) => WriteLane::Bulk,
            _ => WriteLane::Work,
        }
    }
}

impl ServerFrame {
    /// One line (its newline may still be on it). An unknown `t` is
    /// [`ServerFrame::Unknown`], unknown fields are ignored.
    pub fn decode(line: &[u8]) -> Result<Self, FrameError> {
        decode(line)
    }

    /// The line with its newline; too long is the caller's to answer.
    pub fn encode(&self) -> Result<String, FrameError> {
        encode(self)
    }

    pub fn lane(&self) -> WriteLane {
        match self {
            ServerFrame::Pong { .. } => WriteLane::Alive,
            ServerFrame::FileRe(_) => WriteLane::Bulk,
            _ => WriteLane::Work,
        }
    }
}

fn decode<F: serde::de::DeserializeOwned>(line: &[u8]) -> Result<F, FrameError> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    if line.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong);
    }
    // serde reads a tagged enum from an array too; a frame is an object
    if line.trim_ascii_start().first() != Some(&b'{') {
        return Err(FrameError::Malformed("no JSON object".into()));
    }
    serde_json::from_slice(line).map_err(|err| FrameError::Malformed(err.to_string()))
}

fn encode<F: Serialize>(frame: &F) -> Result<String, FrameError> {
    let mut line =
        serde_json::to_string(frame).map_err(|err| FrameError::Malformed(err.to_string()))?;
    if line.len() > MAX_LINE_BYTES {
        return Err(FrameError::TooLong);
    }
    line.push('\n');
    Ok(line)
}

/// The bytes read from a link, cut into lines. It keeps a partial line over a
/// read that times out.
#[derive(Debug, Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
    /// Where the next line starts.
    start: usize,
    /// Searched for a newline up to here.
    scanned: usize,
}

impl LineBuffer {
    /// Add what a read returned; take the lines before the next read.
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.scanned -= self.start;
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole line without its newline, blank ones skipped; `None`
    /// until more arrives. `TooLong` is final: the link is dropped.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            let Some(at) = self.buf[self.scanned..].iter().position(|b| *b == b'\n') else {
                self.scanned = self.buf.len();
                if self.buf.len() - self.start > MAX_LINE_BYTES {
                    return Err(FrameError::TooLong);
                }
                return Ok(None);
            };
            let end = self.scanned + at;
            let line = &self.buf[self.start..end];
            if line.len() > MAX_LINE_BYTES {
                return Err(FrameError::TooLong);
            }
            let line = line.trim_ascii().to_vec();
            self.start = end + 1;
            self.scanned = self.start;
            if !line.is_empty() {
                return Ok(Some(line));
            }
        }
    }
}

/// Bytes as base64 text, at most [`CHUNK_BYTES`].
mod chunk {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(data))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        let data = STANDARD.decode(text).map_err(serde::de::Error::custom)?;
        if data.len() > super::CHUNK_BYTES {
            return Err(serde::de::Error::custom(format!(
                "a chunk over {} bytes",
                super::CHUNK_BYTES
            )));
        }
        Ok(data)
    }
}

impl From<HostReply> for LinkReply {
    /// The sidecar's answer; the caller puts the link's `id` in.
    fn from(reply: HostReply) -> Self {
        Self {
            id: reply.id,
            ok: reply.ok,
            result: reply.result,
            page: reply.page.map(|page| LinkPage {
                url: page.url,
                title: page.title,
                dialog_open: page.dialog_open,
                status: page.status,
            }),
            error: reply.error.map(|error| LinkError {
                code: error.code,
                message: error.message,
                target: error.target,
            }),
        }
    }
}

impl From<LinkReply> for HostReply {
    /// What the hub's ops read.
    fn from(reply: LinkReply) -> Self {
        Self {
            id: reply.id,
            ok: reply.ok,
            result: reply.result,
            page: reply.page.map(|page| PageInfo {
                url: page.url,
                title: page.title,
                dialog_open: page.dialog_open,
                status: page.status,
            }),
            error: reply.error.map(|error| HostError {
                code: error.code,
                message: error.message,
                target: error.target,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The op table (A's gate)

/// What A does with an op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpGate {
    /// Checked against its arguments and sent to the sidecar.
    Allowed,
    /// The helper answers it itself.
    Helper,
    /// `host_refused` ([`RefuseRule::LocalOp`]); so is an op not in [`OPS`]
    /// ([`RefuseRule::UnknownOp`]).
    Refused,
}

/// What an argument must be; another type is `host_refused`, null is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgKind {
    Text,
    Bool,
    /// A whole number from 0.
    Count,
    /// A whole number with a sign.
    Integer,
    /// `http://…` or `https://…` (any case, no control character) or exactly
    /// `about:blank` ([`RefuseRule::Url`]).
    WebUrl,
    /// At most [`EVAL_MAX_BYTES`] ([`RefuseRule::TooLarge`]).
    EvalText,
    /// A spelling of [`PRESS_TABLE`] once its modifiers are in order
    /// ([`RefuseRule::Key`]).
    Key,
    /// At most [`RELEASE_MAX_KEYS`] strings ([`RefuseRule::TooLarge`]).
    TextList,
    /// [`is_file_token`].
    FileToken,
    /// Percent-encoded URL path segments: no leading `/`, no `\`, `?`, `#` or
    /// NUL, no `.` or `..` segment, plain or encoded.
    RelPath,
    /// One of these strings.
    OneOf(&'static [&'static str]),
    /// A's own: B's value is dropped, whatever it is. `allow_password` and
    /// `guard_passwords` from A's config, `user` false, `path` and
    /// `inline_path` left out (A writes no file).
    Own,
}

/// One argument of an op; one that is not listed is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ArgRule {
    pub name: &'static str,
    pub kind: ArgKind,
    #[serde(skip_serializing_if = "is_false")]
    pub required: bool,
}

/// One op of link ops v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OpRule {
    pub op: &'static str,
    pub gate: OpGate,
    /// It may have happened when its link is lost ([`text::link_lost`]).
    #[serde(skip_serializing_if = "is_false")]
    pub driving: bool,
    /// It works on a page: `target` is required.
    #[serde(skip_serializing_if = "is_false")]
    pub target: bool,
    #[serde(skip_serializing_if = "<[ArgRule]>::is_empty")]
    pub args: &'static [ArgRule],
}

const fn arg(name: &'static str, kind: ArgKind) -> ArgRule {
    ArgRule {
        name,
        kind,
        required: false,
    }
}

const fn need(name: &'static str, kind: ArgKind) -> ArgRule {
    ArgRule {
        name,
        kind,
        required: true,
    }
}

const fn bare(op: &'static str, gate: OpGate) -> OpRule {
    OpRule {
        op,
        gate,
        driving: false,
        target: false,
        args: &[],
    }
}

/// An op that looks at a page or at none.
const fn look(op: &'static str, target: bool, args: &'static [ArgRule]) -> OpRule {
    OpRule {
        op,
        gate: OpGate::Allowed,
        driving: false,
        target,
        args,
    }
}

/// An op that may have happened when its link is lost.
const fn drive(op: &'static str, target: bool, args: &'static [ArgRule]) -> OpRule {
    OpRule {
        op,
        gate: OpGate::Allowed,
        driving: true,
        target,
        args,
    }
}

/// The kinds of an `act`.
pub const ACT_KINDS: &[&str] = &["click", "hover", "fill", "select", "type", "press"];

/// Link ops v1: every op of the sidecar and what A does with it.
pub const OPS: &[OpRule] = {
    use ArgKind::{
        Bool, Count, EvalText, FileToken, Integer, Key, OneOf, Own, RelPath, Text, TextList, WebUrl,
    };
    &[
        bare("hello", OpGate::Helper),
        bare("ping", OpGate::Helper),
        bare("attach", OpGate::Refused),
        bare("detach", OpGate::Refused),
        bare("close_browser", OpGate::Refused),
        bare("control_quit", OpGate::Refused),
        bare("dashboard", OpGate::Refused),
        bare("ntp", OpGate::Refused),
        bare("show", OpGate::Refused),
        bare("watch", OpGate::Refused),
        bare("unwatch", OpGate::Refused),
        drive(
            "show_remote",
            false,
            &[
                arg("tab_id", Text),
                arg("opened_by", Text),
                need("token", FileToken),
                need("rel", RelPath),
                arg("title", Text),
                arg("focus", Bool),
            ],
        ),
        drive(
            "open",
            false,
            &[
                need("url", WebUrl),
                arg("wait", Text),
                arg("background", Bool),
                arg("tab_id", Text),
                arg("opened_by", Text),
            ],
        ),
        drive("navigate", true, &[need("url", WebUrl), arg("wait", Text)]),
        drive(
            "history",
            true,
            &[need("action", OneOf(&["back", "forward", "reload"]))],
        ),
        look(
            "read",
            true,
            &[
                arg("format", Text),
                arg("selector", Text),
                arg("ref", Text),
                arg("interactive", Bool),
            ],
        ),
        look("links", true, &[arg("filter", Text), arg("max", Count)]),
        look(
            "console",
            true,
            &[arg("level", Text), arg("since", Count), arg("max", Count)],
        ),
        look(
            "network",
            true,
            &[
                arg("failed", Bool),
                arg("match", Text),
                arg("type", Text),
                arg("since", Count),
                arg("max", Count),
            ],
        ),
        look(
            "wait",
            true,
            &[
                arg("text", Text),
                arg("gone", Text),
                arg("selector", Text),
                arg("url", Text),
                arg("load", Text),
                arg("timeout_ms", Count),
            ],
        ),
        look("scroll", true, &[arg("to", Text), arg("by", Integer)]),
        look("tabs", false, &[]),
        look("select", true, &[]),
        drive("close", true, &[]),
        drive("dialog", true, &[arg("accept", Bool), arg("text", Text)]),
        drive(
            "act",
            true,
            &[
                need("kind", OneOf(ACT_KINDS)),
                arg("ref", Text),
                arg("selector", Text),
                arg("text", Text),
                arg("submit", Bool),
                arg("clear", Bool),
                arg("value", Text),
                // required when `kind` is `press`
                arg("key", Key),
                arg("allow_password", Own),
            ],
        ),
        drive(
            "eval",
            true,
            &[need("expr", EvalText), arg("guard_passwords", Own)],
        ),
        look(
            "screenshot",
            true,
            &[
                arg("full", Bool),
                arg("ref", Text),
                arg("selector", Text),
                arg("format", Text),
                arg("quality", Count),
                arg("max_px", Count),
                arg("path", Own),
                arg("inline_path", Own),
            ],
        ),
        look("focus", true, &[arg("user", Own)]),
        look("control_raise", false, &[arg("user", Own)]),
        look("release", false, &[arg("keys", TextList)]),
    ]
};

/// The rule of an op, none for an unknown one.
pub fn op_rule(op: &str) -> Option<&'static OpRule> {
    OPS.iter().find(|rule| rule.op == op)
}

/// The activity directive of an op (`activity::directive`), as A lets it
/// through: unknown fields dropped, control characters stripped, strings cut
/// to [`ACTIVITY_MAX_CHARS`], times to [`ACTIVITY_MAX_MS`]. One that is not
/// this, or whose `color` is not `#rrggbb` ([`is_activity_color`]), is
/// dropped and its op runs without.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinkActivity {
    #[serde(default)]
    pub frame: bool,
    #[serde(default = "default_true")]
    pub animate: bool,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub linger_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<LinkActivityGroup>,
}

/// The pane an activity belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LinkActivityGroup {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    /// A group colour's name.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub collapse_ms: u64,
}

fn default_true() -> bool {
    true
}

/// `^#[0-9a-f]{6}$`.
pub fn is_activity_color(color: &str) -> bool {
    color.strip_prefix('#').is_some_and(|hex| {
        hex.len() == 6 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Why the gate refuses; its text is in the parentheses of [`text::refused`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefuseRule {
    /// Not in [`OPS`].
    UnknownOp,
    /// [`OpGate::Refused`].
    LocalOp,
    /// A wrong type or value, a missing required argument or target.
    Argument,
    TooLarge,
    Url,
    Key,
    /// A's `[browser] allow_eval`.
    EvalOff,
}

impl RefuseRule {
    pub fn text(self) -> &'static str {
        match self {
            RefuseRule::UnknownOp => "unknown operation",
            RefuseRule::LocalOp => "only herdr+ on that machine may do it",
            RefuseRule::Argument => "missing, or not what it must be",
            RefuseRule::TooLarge => "too large",
            RefuseRule::Url => "only http, https and about:blank pages",
            RefuseRule::Key => "not one of the allowed keys",
            RefuseRule::EvalOff => "[browser] allow_eval = false there",
        }
    }
}

/// The gate's refusal of one op (`host_refused`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub rule: RefuseRule,
    /// The op (`UnknownOp`, `LocalOp`, `EvalOff`), the op and the argument
    /// (`open url`, `read target`) or the key (`the key Meta+v`); from B, so
    /// control characters are stripped and it is cut to
    /// [`ACTIVITY_MAX_CHARS`].
    pub what: String,
}

impl Refusal {
    /// The error A answers with; `label` is A's own.
    pub fn error(&self, label: &str) -> LinkError {
        LinkError::new(
            code::HOST_REFUSED,
            text::refused(label, &self.what, self.rule),
        )
    }
}

// ---------------------------------------------------------------------------
// The press allowlist

/// The order the gate puts modifiers in; `Meta` and `ControlOrMeta` share the
/// last place.
pub const PRESS_MODIFIERS: [&str; 5] = ["Shift", "Alt", "Control", "Meta", "ControlOrMeta"];

/// A group of keys of [`PRESS_TABLE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressKeys {
    /// The 95 printable ASCII characters, U+0020 to U+007E.
    Chars,
    /// [`PRESS_CODES`], `KeyA`-`KeyZ` and `Digit0`-`Digit9`.
    Codes,
    /// `F1`-`F12`.
    Function,
    /// These spellings.
    Named(&'static [&'static str]),
}

/// The physical keys of [`PressKeys::Codes`] that are no letter or digit.
pub const PRESS_CODES: [&str; 12] = [
    "Backquote",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Comma",
    "Period",
    "Slash",
    "Space",
];

/// The keys one set of modifiers allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressRow {
    /// In the order of [`PRESS_MODIFIERS`].
    pub modifiers: &'static [&'static str],
    pub keys: &'static [PressKeys],
}

const ARROWS: PressKeys = PressKeys::Named(&["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);
const PAGING: PressKeys = PressKeys::Named(&["Home", "End", "PageUp", "PageDown"]);
const EDITING: PressKeys = PressKeys::Named(&["Enter", "Tab", "Escape", "Backspace"]);
const DELETE: PressKeys = PressKeys::Named(&["Delete"]);
const META: &[PressKeys] = &[
    PressKeys::Named(&["a", "A", "KeyA", "z", "KeyZ", "Enter", "Backspace"]),
    ARROWS,
];
const SHIFT_META: &[PressKeys] = &[PressKeys::Named(&["z", "Z", "KeyZ", "Enter"]), ARROWS];

/// Every key spelling an `act` `press` may use, as measured (spike S3: no
/// clipboard message, no yank). Exact spellings: no alias is folded, and what
/// is not here is refused.
pub const PRESS_TABLE: &[PressRow] = &[
    PressRow {
        modifiers: &[],
        keys: &[
            PressKeys::Chars,
            PressKeys::Codes,
            ARROWS,
            PAGING,
            EDITING,
            DELETE,
            PressKeys::Function,
        ],
    },
    // without Delete: a cut outside editable text
    PressRow {
        modifiers: &["Shift"],
        keys: &[
            PressKeys::Chars,
            PressKeys::Codes,
            ARROWS,
            PAGING,
            EDITING,
            PressKeys::Function,
        ],
    },
    PressRow {
        modifiers: &["Alt"],
        keys: &[
            ARROWS,
            PAGING,
            PressKeys::Named(&["Backspace", "Enter", "Delete"]),
        ],
    },
    PressRow {
        modifiers: &["Shift", "Alt"],
        keys: &[ARROWS, PAGING, PressKeys::Named(&["Backspace", "Enter"])],
    },
    PressRow {
        modifiers: &["Meta"],
        keys: META,
    },
    PressRow {
        modifiers: &["ControlOrMeta"],
        keys: META,
    },
    PressRow {
        modifiers: &["Shift", "Meta"],
        keys: SHIFT_META,
    },
    PressRow {
        modifiers: &["Shift", "ControlOrMeta"],
        keys: SHIFT_META,
    },
    PressRow {
        modifiers: &["Control"],
        keys: &[PressKeys::Named(&[
            "Enter",
            "ArrowLeft",
            "ArrowRight",
            "Home",
            "End",
        ])],
    },
    PressRow {
        modifiers: &["Shift", "Control"],
        keys: &[PressKeys::Named(&["ArrowLeft", "ArrowRight"])],
    },
];

impl PressKeys {
    /// The key spellings of the group.
    pub fn spellings(self) -> Vec<String> {
        match self {
            PressKeys::Chars => (' '..='~').map(String::from).collect(),
            PressKeys::Codes => ('A'..='Z')
                .map(|c| format!("Key{c}"))
                .chain(('0'..='9').map(|c| format!("Digit{c}")))
                .chain(PRESS_CODES.iter().map(|code| (*code).to_string()))
                .collect(),
            PressKeys::Function => (1..=12).map(|n| format!("F{n}")).collect(),
            PressKeys::Named(keys) => keys.iter().map(|key| (*key).to_string()).collect(),
        }
    }
}

/// [`PRESS_TABLE`] spelled out (`Shift+Meta+z`).
pub fn press_spellings() -> &'static HashSet<String> {
    static SPELLINGS: OnceLock<HashSet<String>> = OnceLock::new();
    SPELLINGS.get_or_init(|| {
        PRESS_TABLE
            .iter()
            .flat_map(|row| {
                row.keys
                    .iter()
                    .flat_map(|keys| keys.spellings())
                    .map(|key| {
                        row.modifiers
                            .iter()
                            .map(|modifier| format!("{modifier}+"))
                            .chain([key])
                            .collect::<String>()
                    })
            })
            .collect()
    })
}

/// Whether the table has exactly this spelling. The caller has split the key
/// as Playwright does and put its modifiers in the order of
/// [`PRESS_MODIFIERS`], and presses this spelling.
pub fn press_allowed(spelling: &str) -> bool {
    press_spellings().contains(spelling)
}

// ---------------------------------------------------------------------------
// Errors

/// The socket error codes of the link.
pub mod code {
    /// No op was sent.
    pub const HOST_UNAVAILABLE: &str = "host_unavailable";
    /// The link dropped while the op ran.
    pub const HOST_LINK_LOST: &str = "host_link_lost";
    /// A's gate said no.
    pub const HOST_REFUSED: &str = "host_refused";
    /// [`super::OPS_IN_FLIGHT`] or [`super::QUEUE_BYTES`] reached.
    pub const HOST_BUSY: &str = "host_busy";
    /// `browser.host_register` while linked.
    pub const HOST_IN_USE: &str = "host_in_use";
    /// `browser.link_open` while a local companion or another Mac serves.
    pub const LINK_IN_USE: &str = "link_in_use";
    /// `browser.link_open` with `[browser] link = "off"`, or on Windows.
    pub const LINK_DISABLED: &str = "link_disabled";
    /// `browser.link_open` with another `link_protocol`.
    pub const LINK_PROTOCOL_UNSUPPORTED: &str = "link_protocol_unsupported";
    pub const BROWSER_DISABLED: &str = "browser_disabled";
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// The helper, to the companion: a relayed call while the link is down.
    pub const LINK_DOWN: &str = "link_down";
}

/// The exact texts of the link's errors and notes. `label` is A's name
/// (`mbp`), `machine` B's (`buildbox`).
pub mod text {
    /// `host_unavailable`: no link; `last` is the Mac that had it and the
    /// seconds since.
    pub fn not_connected(last: Option<(&str, u64)>) -> String {
        match last {
            Some((label, seconds)) => format!(
                "no browser: the user's herdr+ client is not connected (last: {label}, {}). Nothing was sent. Continue without the browser, or ask the user to open herdr+ there.",
                ago(seconds)
            ),
            None => "no browser: the user's herdr+ client is not connected. Nothing was sent. Continue without the browser, or ask the user to open herdr+ on their machine.".into(),
        }
    }

    /// `host_unavailable`: the link dropped, its lease still lives.
    pub fn reconnecting(label: &str) -> String {
        format!("the connection to the browser on {label} dropped and is reconnecting; nothing was sent; call again in a few seconds")
    }

    /// `host_unavailable`: `ready` `not_installed`.
    pub fn not_installed(label: &str) -> String {
        format!("the browser is not installed on {label}; ask the user to run \"herdr browser setup --companion\" there")
    }

    /// `host_unavailable`: `ready` `failed`, or an `ensure` that failed.
    pub fn start_failed(label: &str, detail: &str) -> String {
        let detail = detail.trim();
        if detail.is_empty() {
            format!("the browser on {label} did not start; nothing was sent")
        } else {
            format!("the browser on {label} did not start ({detail}); nothing was sent")
        }
    }

    /// `host_link_lost`: whether the op ([`super::OPS`]) may have happened.
    pub fn link_lost(label: &str, op: &str) -> String {
        // the link's op and the agent's name for it
        let (rule, op) = match op {
            "show" | "show_remote" => ("show_remote", "show"),
            op => (op, op),
        };
        let driving = match super::op_rule(rule) {
            Some(rule) => rule.driving,
            None => true,
        };
        if driving {
            format!("the connection to {label} dropped while {op} ran: it may or may not have happened. Check with browser_read or browser_snapshot before repeating it.")
        } else {
            format!("the connection to {label} dropped while {op} ran; it changed nothing, call it again")
        }
    }

    /// `host_refused`.
    pub fn refused(label: &str, what: &str, rule: super::RefuseRule) -> String {
        format!(
            "{label} refuses {what} for remote machines ({})",
            rule.text()
        )
    }

    /// `host_busy`.
    pub fn busy() -> String {
        format!("{} browser ops are already running", super::OPS_IN_FLIGHT)
    }

    /// `herdr browser companion start|stop|restart` on B while linked.
    pub fn control_there(label: &str) -> String {
        format!("the browser runs on {label}; control it there")
    }

    /// `link_in_use`, another Mac; `since` is B's local time (`14:02`).
    pub fn link_in_use(machine: &str, label: &str, since: &str) -> String {
        format!("the browser of {machine} is on {label} (since {since}); quit herdr+ there")
    }

    /// `show` over the link: it does not reload.
    pub const LOADS_ONCE: &str = "loads once; show it again after changes";
    /// `show` with `page_api` over the link.
    pub const PAGE_API_OFF: &str = "page_api is off for pages on a remote browser; ignored";
    /// `show` with `allow_network` over the link.
    pub const ALLOW_NETWORK_OFF: &str =
        "allow_network is off for pages on a remote browser; ignored";

    /// `12 min ago`.
    pub fn ago(seconds: u64) -> String {
        match seconds {
            0..=59 => "just now".into(),
            60..=3_599 => format!("{} min ago", seconds / 60),
            3_600..=86_399 => format!("{} h ago", seconds / 3_600),
            _ => format!("{} d ago", seconds / 86_400),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    const FRAMES: &str = include_str!("../../tests/fixtures/browser-link-v1/frames.json");
    const OPS_FIXTURE: &str = include_str!("../../tests/fixtures/browser-link-v1/ops.json");
    const PRESS: &str = include_str!("../../tests/fixtures/browser-link-v1/press.json");
    const HOST_MJS: &str = include_str!("../integration/assets/browser/host.mjs");

    fn fixture(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn list<'a>(value: &'a Value, key: &str) -> &'a Vec<Value> {
        value[key].as_array().unwrap_or_else(|| panic!("{key}"))
    }

    fn texts(value: &Value, key: &str) -> Vec<String> {
        list(value, key)
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn line(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    /// A frame with fields a newer side may add.
    fn with_extra(value: &Value) -> Value {
        let mut value = value.clone();
        value["x_new"] = json!({"t": "ping", "n": [1]});
        value
    }

    fn round_trip<T>(values: &[Value])
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        for value in values {
            let parsed: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&parsed).unwrap(), value);
            let tolerant: T = serde_json::from_value(with_extra(value)).unwrap();
            assert_eq!(tolerant, parsed);
        }
    }

    #[test]
    fn link_open_types_round_trip_and_tolerate_newer_values() {
        let frames = fixture(FRAMES);
        assert_eq!(frames["link_protocol"], LINK_PROTOCOL);
        round_trip::<BrowserLinkOpenParams>(list(&frames, "open_params"));
        round_trip::<BrowserLinkInfo>(list(&frames, "opened"));
        round_trip::<BrowserLinkStatus>(list(&frames, "link_status"));
        assert_eq!(frames["opened"][0]["ping_ms"], PING_MS);
        assert_eq!(frames["opened"][0]["ttl_ms"], TTL_MS);
        let newer: BrowserLinkOpenParams = serde_json::from_value(json!({
            "link_protocol": 2, "host_id": "h", "instance": "i", "ready": "asleep",
        }))
        .unwrap();
        assert_eq!(newer.ready, BrowserLinkReady::Unknown);
        assert_eq!(newer.link_protocol, 2);
        for missing in ["link_protocol", "host_id", "instance", "ready"] {
            let mut params = frames["open_params"][0].clone();
            params.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<BrowserLinkOpenParams>(params).is_err(),
                "{missing}"
            );
        }
    }

    #[test]
    fn every_fixture_frame_round_trips() {
        let frames = fixture(FRAMES);
        for value in list(&frames, "helper") {
            let frame = HelperFrame::decode(&line(value)).unwrap();
            assert_ne!(frame, HelperFrame::Unknown, "{value}");
            let sent = frame.encode().unwrap();
            assert!(sent.ends_with('\n') && !sent[..sent.len() - 1].contains('\n'));
            assert_eq!(&fixture(&sent), value);
            assert_eq!(HelperFrame::decode(sent.as_bytes()).unwrap(), frame);
            assert_eq!(
                HelperFrame::decode(&line(&with_extra(value))).unwrap(),
                frame
            );
        }
        for value in list(&frames, "server") {
            let frame = ServerFrame::decode(&line(value)).unwrap();
            assert_ne!(frame, ServerFrame::Unknown, "{value}");
            let sent = frame.encode().unwrap();
            assert_eq!(&fixture(&sent), value);
            assert_eq!(ServerFrame::decode(sent.as_bytes()).unwrap(), frame);
            assert_eq!(
                ServerFrame::decode(&line(&with_extra(value))).unwrap(),
                frame
            );
        }
    }

    #[test]
    fn every_frame_of_the_contract_has_a_fixture() {
        let frames = fixture(FRAMES);
        let tags = |key: &str| -> BTreeSet<String> {
            list(&frames, key)
                .iter()
                .map(|v| v["t"].as_str().unwrap().to_string())
                .collect()
        };
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(
            tags("helper"),
            set(&[
                "ping",
                "ensure_re",
                "host_up",
                "host_down",
                "re",
                "ev",
                "blob",
                "file",
                "api",
                "bye",
            ])
        );
        assert_eq!(
            tags("server"),
            set(&[
                "pong", "ensure", "host_ok", "op", "blob_ack", "file_re", "api_re", "event", "bye",
            ])
        );
    }

    #[test]
    fn unknown_frames_are_ignored_and_broken_lines_are_not() {
        let frames = fixture(FRAMES);
        for value in list(&frames, "helper_ignored") {
            assert_eq!(
                HelperFrame::decode(&line(value)),
                Ok(HelperFrame::Unknown),
                "{value}"
            );
        }
        for value in list(&frames, "server_ignored") {
            assert_eq!(
                ServerFrame::decode(&line(value)),
                Ok(ServerFrame::Unknown),
                "{value}"
            );
        }
        // the other direction's frames are unknown too
        for value in list(&frames, "server") {
            if value["t"] != "bye" {
                assert_eq!(HelperFrame::decode(&line(value)), Ok(HelperFrame::Unknown));
            }
        }
        for text in texts(&frames, "helper_malformed") {
            let got = HelperFrame::decode(text.as_bytes());
            assert!(
                matches!(got, Err(FrameError::Malformed(_))),
                "{text} {got:?}"
            );
        }
        for text in texts(&frames, "server_malformed") {
            let got = ServerFrame::decode(text.as_bytes());
            assert!(
                matches!(got, Err(FrameError::Malformed(_))),
                "{text} {got:?}"
            );
        }
        assert!(matches!(
            HelperFrame::decode(b"{\"t\":\"host_down\",\"reason\":\"\xff\"}"),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn missing_optional_fields_have_their_defaults() {
        assert_eq!(
            ServerFrame::decode(br#"{"t":"ensure","id":1,"echo":2}"#).unwrap(),
            ServerFrame::Ensure(Ensure {
                id: 1,
                echo: 2,
                reason: String::new()
            })
        );
        assert_eq!(
            HelperFrame::decode(br#"{"t":"host_down"}"#).unwrap(),
            HelperFrame::HostDown {
                reason: String::new()
            }
        );
        assert_eq!(
            ServerFrame::decode(br#"{"t":"file_re","id":3,"status":403}"#).unwrap(),
            ServerFrame::FileRe(FileReply {
                id: 3,
                status: 403,
                mime: String::new(),
                size: 0,
                data: Vec::new(),
                eof: false,
            })
        );
        let Ok(HelperFrame::Blob(blob)) = HelperFrame::decode(
            br#"{"t":"blob","op":1,"name":"thumbnail","seq":0,"data":"AA==","size":1}"#,
        ) else {
            panic!("blob");
        };
        assert_eq!(blob.name, BlobName::Unknown);
        assert_eq!(blob.data, [0]);
        assert!(!blob.last);
    }

    #[test]
    fn a_line_over_the_cap_is_refused() {
        let of = |n: usize| {
            let frame = HelperFrame::HostDown {
                reason: "x".repeat(n),
            };
            frame.encode()
        };
        let empty = of(0).unwrap().len() - 1;
        let fits = of(MAX_LINE_BYTES - empty).unwrap();
        assert_eq!(fits.len(), MAX_LINE_BYTES + 1, "the newline does not count");
        assert!(HelperFrame::decode(fits.as_bytes()).is_ok());
        assert_eq!(of(MAX_LINE_BYTES - empty + 1), Err(FrameError::TooLong));
        let long = format!(
            "{{\"t\":\"host_down\",\"reason\":\"{}\"}}",
            "x".repeat(MAX_LINE_BYTES)
        );
        assert_eq!(
            HelperFrame::decode(long.as_bytes()),
            Err(FrameError::TooLong)
        );
        assert_eq!(
            ServerFrame::decode(long.as_bytes()),
            Err(FrameError::TooLong)
        );
    }

    #[test]
    fn a_chunk_over_the_cap_is_malformed() {
        let of = |n: usize| {
            HelperFrame::Blob(BlobChunk {
                op: 1,
                name: BlobName::Full,
                seq: 0,
                data: vec![7; n],
                last: true,
                size: n as u64,
            })
            .encode()
            .unwrap()
        };
        assert!(HelperFrame::decode(of(CHUNK_BYTES).as_bytes()).is_ok());
        assert!(matches!(
            HelperFrame::decode(of(CHUNK_BYTES + 1).as_bytes()),
            Err(FrameError::Malformed(_))
        ));
        let reply = |n: usize| {
            ServerFrame::FileRe(FileReply {
                id: 1,
                status: 200,
                mime: "text/html".into(),
                size: n as u64,
                data: vec![7; n],
                eof: true,
            })
            .encode()
            .unwrap()
        };
        assert!(ServerFrame::decode(reply(CHUNK_BYTES).as_bytes()).is_ok());
        assert!(ServerFrame::decode(reply(CHUNK_BYTES + 1).as_bytes()).is_err());
    }

    #[test]
    fn the_line_buffer_cuts_lines_and_keeps_the_rest() {
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.next_line(), Ok(None));
        buffer.push(b"{\"a\":");
        assert_eq!(buffer.next_line(), Ok(None));
        buffer.push(b"1}\r\n\n  \n{\"b\":2}\n{\"c\"");
        assert_eq!(buffer.next_line(), Ok(Some(b"{\"a\":1}".to_vec())));
        assert_eq!(buffer.next_line(), Ok(Some(b"{\"b\":2}".to_vec())));
        assert_eq!(buffer.next_line(), Ok(None));
        buffer.push(b":3}\n");
        assert_eq!(buffer.next_line(), Ok(Some(b"{\"c\":3}".to_vec())));
        assert_eq!(buffer.next_line(), Ok(None));
    }

    #[test]
    fn the_line_buffer_refuses_a_line_over_the_cap() {
        let mut buffer = LineBuffer::default();
        buffer.push(b"{}\n");
        buffer.push(&vec![b'x'; MAX_LINE_BYTES]);
        assert_eq!(buffer.next_line(), Ok(Some(b"{}".to_vec())));
        assert_eq!(buffer.next_line(), Ok(None));
        buffer.push(b"\n");
        assert_eq!(
            buffer.next_line().unwrap().map(|line| line.len()),
            Some(MAX_LINE_BYTES)
        );
        buffer.push(&vec![b'x'; MAX_LINE_BYTES + 1]);
        assert_eq!(buffer.next_line(), Err(FrameError::TooLong));
        let mut buffer = LineBuffer::default();
        buffer.push(&vec![b'x'; MAX_LINE_BYTES + 1]);
        buffer.push(b"\n");
        assert_eq!(buffer.next_line(), Err(FrameError::TooLong));
    }

    #[test]
    fn writer_lanes_follow_the_contract() {
        let frames = fixture(FRAMES);
        for value in list(&frames, "helper") {
            let lane = match value["t"].as_str().unwrap() {
                "ping" => WriteLane::Alive,
                "blob" | "file" => WriteLane::Bulk,
                _ => WriteLane::Work,
            };
            assert_eq!(HelperFrame::decode(&line(value)).unwrap().lane(), lane);
        }
        for value in list(&frames, "server") {
            let lane = match value["t"].as_str().unwrap() {
                "pong" => WriteLane::Alive,
                "file_re" => WriteLane::Bulk,
                _ => WriteLane::Work,
            };
            assert_eq!(ServerFrame::decode(&line(value)).unwrap().lane(), lane);
        }
        assert!(WriteLane::Alive < WriteLane::Work && WriteLane::Work < WriteLane::Bulk);
    }

    #[test]
    fn a_sidecar_reply_crosses_the_link_unchanged() {
        let sidecar = r#"{"id":3,"ok":false,"result":{"x":[1,null]},"page":{"url":"https://a/","title":"A","dialog_open":true,"status":404},"error":{"code":"navigation_failed","message":"m","target":"T"}}"#;
        let Some(super::super::host::HostMessage::Reply(reply)) =
            super::super::host::parse_line(sidecar)
        else {
            panic!("reply");
        };
        let mut sent = LinkReply::from(reply.clone());
        sent.id = 900;
        let Ok(HelperFrame::Re(got)) =
            HelperFrame::decode(HelperFrame::Re(sent).encode().unwrap().as_bytes())
        else {
            panic!("re");
        };
        assert_eq!(HostReply::from(got), HostReply { id: 900, ..reply });
    }

    #[test]
    fn a_sidecar_event_crosses_the_link_as_a_host_event() {
        let frames = fixture(FRAMES);
        let mut seen = 0;
        for value in list(&frames, "helper") {
            if let Ok(HelperFrame::Ev { event }) = HelperFrame::decode(&line(value)) {
                let event: super::super::host::HostEvent = serde_json::from_value(event).unwrap();
                assert_ne!(event, super::super::host::HostEvent::Unknown);
                seen += 1;
            }
        }
        assert_eq!(seen, 2);
    }

    #[test]
    fn the_op_table_is_the_fixture() {
        let fixture = fixture(OPS_FIXTURE);
        assert_eq!(fixture["link_ops"], 1);
        for (rule, want) in OPS.iter().zip(list(&fixture, "ops")) {
            assert_eq!(&serde_json::to_value(rule).unwrap(), want);
        }
        assert_eq!(OPS.len(), list(&fixture, "ops").len());
        let names: BTreeSet<_> = OPS.iter().map(|rule| rule.op).collect();
        assert_eq!(names.len(), OPS.len());
        let driving: Vec<_> = OPS.iter().filter(|r| r.driving).map(|r| r.op).collect();
        assert_eq!(
            driving,
            [
                "show_remote",
                "open",
                "navigate",
                "history",
                "close",
                "dialog",
                "act",
                "eval"
            ]
        );
        for rule in OPS {
            assert!(rule.gate == OpGate::Allowed || rule.args.is_empty());
        }
    }

    /// Link ops v1 says something about every op of the sidecar: a new one
    /// is refused until it has a row here (and a feature string).
    #[test]
    fn every_sidecar_op_has_a_row() {
        let start = HOST_MJS.find("\nconst ops = {\n").unwrap();
        let body = &HOST_MJS[start..];
        let body = &body[..body.find("\n};\n").unwrap()];
        let ops: Vec<&str> = body
            .lines()
            .filter_map(|line| line.strip_prefix("  async "))
            .filter_map(|line| line.split('(').next())
            .collect();
        assert!(ops.len() >= 29, "{ops:?}");
        for op in ops {
            assert!(op_rule(op).is_some(), "{op}");
        }
    }

    #[test]
    fn the_op_cases_agree_with_the_table() {
        let fixture = fixture(OPS_FIXTURE);
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for case in list(&fixture, "cases") {
            let name = case["name"].as_str().unwrap();
            assert!(names.insert(name), "{name}");
            let Ok(ServerFrame::Op(op)) = ServerFrame::decode(&line(&case["frame"])) else {
                panic!("{name}");
            };
            let expect = case["expect"].as_object().unwrap();
            assert_eq!(expect.len(), 1, "{name}");
            let rule = op_rule(&op.op);
            if let Some(forward) = expect.get("forward") {
                let rule = rule.unwrap();
                seen.insert(rule.op);
                assert_eq!(rule.gate, OpGate::Allowed, "{name}");
                assert_eq!(forward["op"], op.op, "{name}");
                assert_eq!(forward["target"].as_str(), op.target.as_deref(), "{name}");
                assert_eq!(rule.target, op.target.is_some(), "{name}");
                let args = forward["args"].as_object().cloned().unwrap_or_default();
                for (key, value) in &args {
                    let arg = rule.args.iter().find(|arg| arg.name == key).unwrap();
                    assert!(fits(arg.kind, value), "{name}: {key}");
                    if arg.kind != ArgKind::Own && arg.kind != ArgKind::Key {
                        assert_eq!(value, &op.args[key], "{name}: {key}");
                    }
                }
                for arg in rule.args {
                    let sent = op.args.get(arg.name).is_some_and(|value| !value.is_null());
                    let want = match arg.kind {
                        ArgKind::Own => !arg.name.ends_with("path"),
                        _ => sent,
                    };
                    assert_eq!(args.contains_key(arg.name), want, "{name}: {}", arg.name);
                    assert!(!arg.required || sent, "{name}: {}", arg.name);
                }
                match (forward.get("activity"), &op.activity) {
                    (Some(want), Some(_)) => {
                        let activity: LinkActivity = serde_json::from_value(want.clone()).unwrap();
                        assert_eq!(&serde_json::to_value(&activity).unwrap(), want, "{name}");
                        assert!(is_activity_color(&activity.color), "{name}");
                        assert!(activity.linger_ms <= ACTIVITY_MAX_MS, "{name}");
                        let group = activity.group.unwrap();
                        assert!(group.title.chars().count() <= ACTIVITY_MAX_CHARS, "{name}");
                        assert!(group.collapse_ms <= ACTIVITY_MAX_MS, "{name}");
                    }
                    (Some(_), None) => panic!("{name}"),
                    _ => {}
                }
            } else if expect.get("helper").is_some() {
                assert_eq!(rule.unwrap().gate, OpGate::Helper, "{name}");
                seen.insert(rule.unwrap().op);
            } else {
                let refusal: Refusal = serde_json::from_value(expect["refuse"].clone()).unwrap();
                match refusal.rule {
                    RefuseRule::UnknownOp => assert!(rule.is_none(), "{name}"),
                    RefuseRule::LocalOp => {
                        assert_eq!(rule.unwrap().gate, OpGate::Refused, "{name}");
                        seen.insert(rule.unwrap().op);
                    }
                    RefuseRule::Key => {
                        let key = op.args["key"].as_str().unwrap();
                        assert_eq!(refusal.what, format!("the key {key}"));
                        assert!(canonical(key).is_none(), "{name}");
                    }
                    RefuseRule::EvalOff => {
                        assert_eq!(case["config"]["allow_eval"], false, "{name}");
                    }
                    RefuseRule::Argument | RefuseRule::TooLarge | RefuseRule::Url => {
                        let rule = rule.unwrap();
                        let (op_name, arg) = refusal.what.split_once(' ').unwrap();
                        assert_eq!(op_name, rule.op, "{name}");
                        match arg {
                            "target" => assert!(rule.target && op.target.is_none(), "{name}"),
                            "args" => assert!(!op.args.is_object(), "{name}"),
                            arg => {
                                let arg = rule.args.iter().find(|a| a.name == arg).unwrap();
                                assert!(!fits(arg.kind, &op.args[arg.name]), "{name}");
                            }
                        }
                    }
                }
            }
        }
        let all: BTreeSet<&str> = OPS.iter().map(|rule| rule.op).collect();
        assert_eq!(seen, all, "every op has a case");
    }

    /// The argument kinds, for checking the fixture (the gate has its own).
    fn fits(kind: ArgKind, value: &Value) -> bool {
        match kind {
            ArgKind::Text => value.is_string(),
            ArgKind::Bool => value.is_boolean(),
            ArgKind::Count => value.is_u64(),
            ArgKind::Integer => value.is_i64() || value.is_u64(),
            ArgKind::WebUrl => value.as_str().is_some_and(|url| {
                let lower = url.to_ascii_lowercase();
                url == "about:blank"
                    || ((lower.starts_with("http://") || lower.starts_with("https://"))
                        && !url.contains(char::is_control))
            }),
            ArgKind::EvalText => value.as_str().is_some_and(|t| t.len() <= EVAL_MAX_BYTES),
            ArgKind::Key => value.as_str().is_some_and(|key| canonical(key).is_some()),
            ArgKind::TextList => value.as_array().is_some_and(|list| {
                list.len() <= RELEASE_MAX_KEYS && list.iter().all(Value::is_string)
            }),
            ArgKind::FileToken => value.as_str().is_some_and(is_file_token),
            ArgKind::RelPath => value.as_str().is_some_and(|path| {
                !path.is_empty()
                    && !path.contains(['\\', '?', '#', '\0'])
                    && path.split('/').all(|segment| {
                        let plain = segment.to_ascii_lowercase().replace("%2e", ".");
                        !plain.is_empty()
                            && plain != "."
                            && plain != ".."
                            && !["%2f", "%5c", "%00"].iter().any(|bad| plain.contains(bad))
                    })
            }),
            ArgKind::OneOf(values) => value.as_str().is_some_and(|text| values.contains(&text)),
            ArgKind::Own => value.is_boolean(),
        }
    }

    /// The spelling the gate presses for a key, for checking the fixture
    /// (the gate has its own): split as Playwright's `Keyboard.press` does,
    /// modifiers in order and at most once.
    fn canonical(key: &str) -> Option<String> {
        let mut parts = vec![String::new()];
        for c in key.chars() {
            match parts.last_mut() {
                Some(part) if c != '+' || part.is_empty() => part.push(c),
                _ => parts.push(String::new()),
            }
        }
        let key = parts.pop()?;
        let mut spelling = String::new();
        for modifier in PRESS_MODIFIERS {
            match parts.iter().filter(|part| *part == modifier).count() {
                0 => {}
                1 => spelling.push_str(&format!("{modifier}+")),
                _ => return None,
            }
        }
        if parts.iter().any(|p| !PRESS_MODIFIERS.contains(&p.as_str())) {
            return None;
        }
        spelling.push_str(&key);
        press_allowed(&spelling).then_some(spelling)
    }

    #[test]
    fn the_press_table_is_exactly_the_measured_allowlist() {
        let fixture = fixture(PRESS);
        let allowed = texts(&fixture, "allowed");
        assert_eq!(allowed.len(), 401);
        let measured: BTreeSet<&String> = allowed.iter().collect();
        assert_eq!(measured.len(), 401);
        assert_eq!(measured, press_spellings().iter().collect());
        let rows: usize = PRESS_TABLE
            .iter()
            .flat_map(|row| row.keys)
            .map(|keys| keys.spellings().len())
            .sum();
        assert_eq!(rows, 401, "no spelling twice");
        for spelling in &allowed {
            assert_eq!(canonical(spelling).as_ref(), Some(spelling));
        }
        for spelling in ["+", "Shift++", " ", "Shift+ ", "a", "A", "é", "~"] {
            assert_eq!(press_allowed(spelling), spelling != "é", "{spelling:?}");
        }
        for row in PRESS_TABLE {
            let order: Vec<usize> = row
                .modifiers
                .iter()
                .map(|m| PRESS_MODIFIERS.iter().position(|p| p == m).unwrap())
                .collect();
            assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{row:?}");
        }
    }

    #[test]
    fn the_spikes_other_spellings_are_reordered_ones() {
        let fixture = fixture(PRESS);
        let reordered = list(&fixture, "reordered");
        assert_eq!(reordered.len(), 6);
        let mut measured: BTreeSet<String> = texts(&fixture, "allowed").into_iter().collect();
        for entry in reordered {
            let key = entry["key"].as_str().unwrap();
            let presses = entry["presses"].as_str().unwrap();
            assert!(!press_allowed(key), "{key}");
            assert!(press_allowed(presses), "{presses}");
            assert_eq!(canonical(key).as_deref(), Some(presses));
            measured.insert(key.to_string());
        }
        assert_eq!(measured.len(), 407, "S3 pressed 407");
    }

    #[test]
    fn the_clipboard_keys_are_refused() {
        let fixture = fixture(PRESS);
        assert_eq!(
            texts(&fixture, "refused"),
            [
                "Shift+Insert",
                "Shift+Delete",
                "Shift+Numpad0",
                "Shift+NumpadDecimal",
                "Control+Insert",
                "Control+Numpad0",
                "Meta+c",
                "ControlOrMeta+v",
                "MetaLeft+v",
                "a+Meta+v",
                "Meta+a+v",
                "Shift+Shift+Insert",
                "Meta+Nope",
                "Control+k",
                "Control+y",
                "Paste",
                "Copy",
                "Cut",
            ]
        );
        for key in texts(&fixture, "refused")
            .into_iter()
            .chain(texts(&fixture, "refused_more"))
        {
            assert_eq!(canonical(&key), None, "{key:?}");
        }
        for spelling in press_spellings() {
            assert!(
                !spelling.contains("Insert") && !spelling.contains("Numpad"),
                "{spelling}"
            );
        }
    }

    #[test]
    fn file_tokens_and_activity_colours() {
        assert!(is_file_token("k3v9x0q2m7w1z8r4t6y5u0ab"));
        assert!(is_file_token(&"a".repeat(24)));
        for bad in [
            "",
            "a",
            "1aaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaA",
            "aaaaaaaaaaaaaaaaaaaaaaaé",
            "aaaaaaaaaaaaaaaaaaaaaaa.",
        ] {
            assert!(!is_file_token(bad), "{bad}");
        }
        assert!(!is_file_token(&"a".repeat(25)));
        assert!(is_activity_color("#00c8ff"));
        for bad in ["", "#00C8FF", "00c8ff", "#00c8f", "#00c8ff0", "#00c8fg"] {
            assert!(!is_activity_color(bad), "{bad}");
        }
        // what herdr itself sends fits
        let actor = crate::api::schema::BrowserActor::Pane {
            pane_id: "w2:p7".into(),
            tab_id: "w2:t3".into(),
            workspace_id: "w2".into(),
            tab_label: "build".into(),
            workspace_label: None,
            agent: Some("claude".into()),
            session: "default".into(),
            gone: false,
            shell_pid: None,
        };
        let sent =
            super::super::activity::directive(&actor, &crate::config::BrowserConfig::default())
                .unwrap();
        let activity: LinkActivity = serde_json::from_value(sent.clone()).unwrap();
        assert!(is_activity_color(&activity.color));
        assert_eq!(serde_json::to_value(&activity).unwrap(), sent);
    }

    /// The companion's list and the contract's are one.
    #[test]
    fn the_api_allowlist_is_the_contracts() {
        assert_eq!(
            API_METHODS,
            [
                "browser.host_state",
                "browser.host_sync",
                "browser.host_report",
                "browser.send_page",
                "tab.focus",
                "events.subscribe",
            ]
        );
        let host_js = include_str!("../../apps/companion/src/host.js");
        let start = host_js.find("const EVENT_KINDS = [").unwrap();
        let body = &host_js[start..];
        let kinds: Vec<&str> = body[..body.find("];").unwrap()]
            .lines()
            .skip(1)
            .map(|line| line.trim().trim_matches([',', '\'']))
            .filter(|kind| !kind.is_empty())
            .collect();
        assert_eq!(kinds, EVENT_KINDS);
    }

    #[test]
    fn the_texts_are_the_contracts() {
        assert_eq!(
            text::not_connected(Some(("mbp", 12 * 60))),
            "no browser: the user's herdr+ client is not connected (last: mbp, 12 min ago). Nothing was sent. Continue without the browser, or ask the user to open herdr+ there."
        );
        assert_eq!(
            text::not_connected(None),
            "no browser: the user's herdr+ client is not connected. Nothing was sent. Continue without the browser, or ask the user to open herdr+ on their machine."
        );
        assert_eq!(
            text::reconnecting("mbp"),
            "the connection to the browser on mbp dropped and is reconnecting; nothing was sent; call again in a few seconds"
        );
        assert_eq!(
            text::not_installed("mbp"),
            "the browser is not installed on mbp; ask the user to run \"herdr browser setup --companion\" there"
        );
        assert_eq!(
            text::start_failed("mbp", " exited 5 times in 5 minutes\n"),
            "the browser on mbp did not start (exited 5 times in 5 minutes); nothing was sent"
        );
        assert_eq!(
            text::start_failed("mbp", ""),
            "the browser on mbp did not start; nothing was sent"
        );
        let happened = "the connection to mbp dropped while OP ran: it may or may not have happened. Check with browser_read or browser_snapshot before repeating it.";
        for op in [
            "open", "show", "navigate", "history", "act", "eval", "dialog", "close",
        ] {
            assert_eq!(text::link_lost("mbp", op), happened.replace("OP", op));
        }
        assert_eq!(
            text::link_lost("mbp", "show_remote"),
            happened.replace("OP", "show")
        );
        assert_eq!(
            text::link_lost("mbp", "read"),
            "the connection to mbp dropped while read ran; it changed nothing, call it again"
        );
        // an op this build does not know may have done anything
        assert_eq!(
            text::link_lost("mbp", "future"),
            happened.replace("OP", "future")
        );
        assert_eq!(
            Refusal {
                rule: RefuseRule::Url,
                what: "open url".into()
            }
            .error("mbp"),
            LinkError::new(
                "host_refused",
                "mbp refuses open url for remote machines (only http, https and about:blank pages)"
            )
        );
        assert_eq!(text::busy(), "32 browser ops are already running");
        assert_eq!(
            text::control_there("mbp"),
            "the browser runs on mbp; control it there"
        );
        assert_eq!(
            text::link_in_use("buildbox", "mbp", "14:02"),
            "the browser of buildbox is on mbp (since 14:02); quit herdr+ there"
        );
        assert_eq!(text::LOADS_ONCE, "loads once; show it again after changes");
        assert_eq!(
            text::PAGE_API_OFF,
            "page_api is off for pages on a remote browser; ignored"
        );
        assert_eq!(
            text::ALLOW_NETWORK_OFF,
            "allow_network is off for pages on a remote browser; ignored"
        );
    }

    #[test]
    fn ago_is_short() {
        for (seconds, want) in [
            (0, "just now"),
            (59, "just now"),
            (60, "1 min ago"),
            (3_599, "59 min ago"),
            (3_600, "1 h ago"),
            (86_399, "23 h ago"),
            (86_400, "1 d ago"),
        ] {
            assert_eq!(text::ago(seconds), want);
        }
    }

    #[test]
    fn every_goodbye_has_a_fixture() {
        let frames = fixture(FRAMES);
        let said: BTreeSet<String> = ["helper", "server"]
            .iter()
            .flat_map(|side| list(&frames, side))
            .filter(|value| value["t"] == "bye")
            .map(|value| value["code"].as_str().unwrap_or_default().to_string())
            .collect();
        let codes = [
            bye::PROTOCOL,
            bye::REPLACED,
            bye::STOPPING,
            bye::QUIT,
            bye::DISABLED,
        ];
        assert_eq!(BTreeSet::from(codes.map(str::to_string)), said);
    }

    #[test]
    fn the_numbers_are_the_contracts() {
        assert_eq!(
            (PING_MS, TTL_MS, HELPER_SILENCE_MS, STALE_ECHO_MS, ENSURE_MS),
            (3_000, 15_000, 12_000, 15_000, 45_000)
        );
        assert_eq!(
            (
                REPLY_EXTRA_MS,
                READ_POLL_MS,
                RECONNECT_FIRST_MS,
                RECONNECT_MAX_MS,
                IN_USE_RETRY_MS
            ),
            (2_000, 250, 500, 5_000, 30_000)
        );
        assert_eq!((MAX_LINE_BYTES, CHUNK_BYTES), (8 << 20, 64 << 10));
        assert_eq!((BLOB_MAX_BYTES, FILE_MAX_BYTES), (32 << 20, 32 << 20));
        assert_eq!((BLOB_WINDOW, FILE_WINDOW), (4, 4));
        assert_eq!((FILE_PULLS, OPS_IN_FLIGHT), (16, 32));
        let limits = &fixture(FRAMES)["limits"];
        assert_eq!(limits["chunk_bytes"], CHUNK_BYTES);
        assert_eq!(limits["blob_window"], BLOB_WINDOW);
        assert_eq!(limits["file_window"], FILE_WINDOW);
        assert_eq!(limits["max_line_bytes"], MAX_LINE_BYTES);
        assert_eq!((QUEUE_BYTES, EVAL_MAX_BYTES), (4 << 20, 256 << 10));
        assert_eq!(
            (RELEASE_MAX_KEYS, ACTIVITY_MAX_MS, ACTIVITY_MAX_CHARS),
            (64, 600_000, 80)
        );
        // a full chunk fits a line
        const { assert!(CHUNK_BYTES.div_ceil(3) * 4 < MAX_LINE_BYTES) };
    }
}
