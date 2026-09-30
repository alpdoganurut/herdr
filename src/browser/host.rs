//! The Playwright sidecar (`herdr-browser-host.mjs`): how it is spawned and
//! the JSON-lines protocol on its stdio.
//!
//! Request: `{"id":17,"op":"read","profile":"main","target":"<targetId>","args":{…},"deadline_ms":30000}`
//! Reply: `{"id":17,"ok":true,"result":{…},"page":{"url":…,"title":…,"dialog_open":false}}`
//! or `{"id":17,"ok":false,"error":{"code":"stale_ref","message":"…"}}`.
//! Events (unsolicited): `{"event":"tab",…}`, `{"event":"dialog",…}`,
//! `{"event":"browser",…}`, `{"event":"log",…}`.
//!
//! The sidecar exits when its stdin closes, so it dies with the server
//! without a shutdown hook; it is spawned as a process-group leader so a
//! SIGKILL takes its helpers too.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

use super::state::HostTabEvent;

/// The protocol the embedded `host.mjs` speaks; `hello` must return it.
pub const HOST_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct HostRequest<'a> {
    pub id: u64,
    pub op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'a str>,
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub args: serde_json::Value,
    pub deadline_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct HostError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct PageInfo {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub dialog_open: bool,
    #[serde(default)]
    pub status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostReply {
    pub id: u64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default)]
    pub error: Option<HostError>,
    #[serde(default)]
    pub page: Option<PageInfo>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    Tab(HostTabEvent),
    Dialog {
        profile: String,
        target: String,
        #[serde(rename = "type", default)]
        type_: String,
        #[serde(default)]
        message: String,
        /// `open` or `closed`.
        #[serde(default)]
        state: String,
    },
    Browser {
        profile: String,
        /// `attached` or `disconnected`.
        kind: String,
        #[serde(default)]
        detail: String,
    },
    Log {
        #[serde(default)]
        level: String,
        #[serde(default)]
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    Reply(HostReply),
    Event(HostEvent),
}

/// Parse one stdout line. Non-JSON lines (a stray `console.log`) are ignored.
pub fn parse_line(line: &str) -> Option<HostMessage> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("event").is_some() {
        return serde_json::from_value::<HostEvent>(value)
            .ok()
            .map(HostMessage::Event);
    }
    if value.get("id").is_some() {
        return serde_json::from_value::<HostReply>(value)
            .ok()
            .map(HostMessage::Reply);
    }
    None
}

/// The writing half of a sidecar link.
pub trait HostWriter: Send {
    fn write_line(&mut self, line: &str) -> io::Result<()>;
}

impl<W: Write + Send> HostWriter for W {
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.write_all(line.as_bytes())?;
        self.write_all(b"\n")?;
        self.flush()
    }
}

/// A spawned sidecar: the child, its stdin and stdout.
pub struct SpawnedHost {
    pub child: Child,
    pub pid: u32,
    pub writer: Box<dyn HostWriter>,
    pub reader: Box<dyn BufRead + Send>,
}

/// Spawn `node <host_dir>/host.mjs` with stdin/stdout piped and stderr
/// appended to `<host_dir>/host.log`.
pub fn spawn(node: &Path, host_dir: &Path, entry: &str) -> io::Result<SpawnedHost> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(host_dir.join("host.log"))?;
    let mut command = Command::new(node);
    command
        .arg(host_dir.join(entry))
        .current_dir(host_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .env("NODE_NO_WARNINGS", "1");
    super::launch::scrub_herdr_env(&mut command);
    command.env("HERDR_BROWSER_HOST", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("sidecar stdin missing"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("sidecar stdout missing"))?;
    let pid = child.id();
    Ok(SpawnedHost {
        child,
        pid,
        writer: Box::new(stdin),
        reader: Box::new(io::BufReader::new(stdout)),
    })
}

/// SIGKILL the sidecar's process group (it is the leader).
pub fn kill_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// The last `lines` of a log file, for `host_failed` messages.
pub fn log_tail(path: &Path, lines: usize) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_events_and_noise_parse() {
        let reply = parse_line(
            r#"{"id":3,"ok":true,"result":{"x":1},"page":{"url":"https://a/","title":"A"}}"#,
        )
        .unwrap();
        match reply {
            HostMessage::Reply(reply) => {
                assert_eq!(reply.id, 3);
                assert!(reply.ok);
                assert_eq!(reply.result["x"], 1);
                assert_eq!(reply.page.unwrap().title, "A");
            }
            other => panic!("{other:?}"),
        }
        let err =
            parse_line(r#"{"id":4,"ok":false,"error":{"code":"stale_ref","message":"gone"}}"#)
                .unwrap();
        assert!(
            matches!(err, HostMessage::Reply(HostReply { ok: false, error: Some(HostError { ref code, .. }), .. }) if code == "stale_ref")
        );
        let event = parse_line(r#"{"event":"tab","profile":"main","target":"T","kind":"navigated","url":"https://b/","initiator":"other"}"#).unwrap();
        assert!(
            matches!(event, HostMessage::Event(HostEvent::Tab(ref tab)) if tab.kind == "navigated" && tab.initiator == "other")
        );
        let dialog = parse_line(r#"{"event":"dialog","profile":"main","target":"T","type":"alert","message":"hi","state":"open"}"#).unwrap();
        assert!(
            matches!(dialog, HostMessage::Event(HostEvent::Dialog { ref state, .. }) if state == "open")
        );
        assert!(matches!(
            parse_line(r#"{"event":"future","x":1}"#),
            Some(HostMessage::Event(HostEvent::Unknown))
        ));
        assert!(parse_line("Debugger attached.").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line("{not json").is_none());
    }

    #[test]
    fn requests_serialize_compactly() {
        let request = HostRequest {
            id: 1,
            op: "read",
            profile: Some("main"),
            target: Some("T"),
            args: serde_json::json!({"format": "markdown"}),
            deadline_ms: 30000,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json,
            r#"{"id":1,"op":"read","profile":"main","target":"T","args":{"format":"markdown"},"deadline_ms":30000}"#
        );
        let hello = HostRequest {
            id: 2,
            op: "hello",
            profile: None,
            target: None,
            args: serde_json::Value::Null,
            deadline_ms: 5000,
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"id":2,"op":"hello","deadline_ms":5000}"#
        );
    }
}
