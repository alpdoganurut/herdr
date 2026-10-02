//! The agent message log (`messages.jsonl`): one JSON object per line,
//! appended by whichever MCP server sent the message. Agents message each
//! other directly; the log is how the coordinator agent and the dashboard
//! stay aware of that traffic without relaying it.

use std::collections::hash_map::RandomState;
use std::collections::HashSet;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::messages_path;

/// Lock taken by appenders and by rotation, so no line lands in a file that
/// is being renamed away.
const MESSAGES_LOCK: &str = "messages";

/// `kind` of a log line for a message that was not typed into its target.
pub const KIND_REFUSAL: &str = "refusal";

/// `outcome` of a message typed into its target.
pub const OUTCOME_SENT: &str = "sent";

/// `outcome` of a reply that was not typed in because the asker was busy
/// (usually waiting in agents_wait_for_message). Not a refusal: the asker
/// receives it from the log (agents_wait_for_message, agents_messages).
pub const OUTCOME_LOGGED: &str = "logged";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub unix: u64,
    /// Sender pane id; `None` for an outside caller (a tool not in a pane).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    pub to_pane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_name: Option<String>,
    pub text: String,
    /// [`OUTCOME_SENT`], [`OUTCOME_LOGGED`] for a reply delivered through
    /// the log, or the error code when delivery was refused.
    pub outcome: String,
    /// Message id (`m...`, see [`new_id`]); absent on lines written before ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The id of the message this one answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_role: Option<String>,
    /// `None` for a message, [`KIND_REFUSAL`] when herdr+ refused to deliver it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The rotated log (`messages.1.jsonl`), replaced on every rotation.
pub fn rotated_path(dir: &Path) -> PathBuf {
    dir.join("messages.1.jsonl")
}

/// A new message id: `m`, the unix time in milliseconds in base 36, and four
/// hex digits. The hex digits are a per-process random start (pid and clock
/// hashed with `RandomState`) plus a counter, so ids from one process never
/// collide (up to 65536 per millisecond) and ids from two processes collide
/// only if both the millisecond and the 16-bit suffix match.
pub fn new_id() -> String {
    static START: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let start = *START.get_or_init(|| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(now.as_nanos());
        hasher.write_u32(std::process::id());
        hasher.finish()
    });
    let suffix = start.wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed)) & 0xffff;
    format!("m{}{suffix:04x}", base36(now.as_millis() as u64))
}

/// Whether `value` has the shape of a message id from [`new_id`].
pub fn is_id(value: &str) -> bool {
    value.len() > 1
        && value.len() <= 32
        && value.starts_with('m')
        && value[1..]
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch.is_ascii_lowercase())
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

pub fn append(dir: &Path, message: &AgentMessage) -> io::Result<()> {
    let _lock = super::lock::exclusive(dir, MESSAGES_LOCK)?;
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    // One write per entry in append mode: concurrent writers do not interleave lines.
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(messages_path(dir))?;
    file.write_all(line.as_bytes())
}

/// The newest `limit` messages (oldest first), optionally only those a pane
/// sent or received. Unparseable lines are skipped.
pub fn recent(dir: &Path, limit: usize, involving: Option<&str>) -> Vec<AgentMessage> {
    let Ok(text) = std::fs::read_to_string(messages_path(dir)) else {
        return Vec::new();
    };
    let mut out: Vec<AgentMessage> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<AgentMessage>(line).ok())
        .filter(|message| {
            involving.is_none_or(|pane| {
                message.from_pane.as_deref() == Some(pane) || message.to_pane == pane
            })
        })
        .collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

/// The complete lines appended since byte `offset`, and the offset to pass
/// next time. A trailing line without its newline is left for the next call
/// (a writer may be mid-append). A file shorter than `offset` was rotated:
/// reading restarts at 0. Unparseable lines are skipped.
pub fn since_offset(dir: &Path, offset: u64) -> (Vec<AgentMessage>, u64) {
    let mut file = match std::fs::File::open(messages_path(dir)) {
        Ok(file) => file,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!("coordinator: cannot open the message log: {err}");
            }
            return (Vec::new(), 0);
        }
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = if len < offset { 0 } else { offset };
    let mut bytes = Vec::new();
    if let Err(err) = file
        .seek(SeekFrom::Start(start))
        .and_then(|_| file.read_to_end(&mut bytes))
    {
        tracing::warn!("coordinator: cannot read the message log: {err}");
        return (Vec::new(), start);
    }
    let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
        return (Vec::new(), start);
    };
    let messages = bytes[..end]
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<AgentMessage>(line).ok())
        .collect();
    (messages, start + end as u64 + 1)
}

/// The oldest message addressed to `to_pane` that answers `reply_to` and/or
/// comes from `from_pane` (each filter applies only when given), skipping the
/// ids in `skip` (already returned to this waiter). Only messages logged after
/// the line with id `after_id` count when that line is in the log (log order,
/// not the clock: a message from the same second before it does not count);
/// otherwise those at or after `after_unix`. The outcome is deliberately not
/// checked: an agent waiting for a reply is `working`, so the reply is usually
/// logged (`logged`) instead of being typed in, and this log line is how the
/// waiter receives it.
pub fn find_reply(
    dir: &Path,
    to_pane: &str,
    reply_to: Option<&str>,
    from_pane: Option<&str>,
    after_unix: u64,
    after_id: Option<&str>,
    skip: &HashSet<String>,
) -> Option<AgentMessage> {
    let text = std::fs::read_to_string(messages_path(dir)).ok()?;
    let log: Vec<AgentMessage> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<AgentMessage>(line).ok())
        .collect();
    let anchor = after_id.and_then(|id| log.iter().position(|m| m.id.as_deref() == Some(id)));
    let (tail, after_unix) = match anchor {
        Some(index) => (&log[index + 1..], 0),
        None => (&log[..], after_unix),
    };
    tail.iter()
        .find(|message| {
            message.to_pane == to_pane
                && message.unix >= after_unix
                && reply_to.is_none_or(|id| message.reply_to.as_deref() == Some(id))
                && from_pane.is_none_or(|pane| message.from_pane.as_deref() == Some(pane))
                && message.id.as_ref().is_none_or(|id| !skip.contains(id))
        })
        .cloned()
}

/// Move the log to `messages.1.jsonl` (replacing the previous one) once it is
/// larger than `max_bytes`; `true` when it rotated. `read_to` is the offset
/// the caller has read up to ([`since_offset`]): a log that grew past it (a
/// line appended since) is left for a later call, so no line moves away unread.
pub fn rotate_if_large(dir: &Path, max_bytes: u64, read_to: u64) -> io::Result<bool> {
    let _lock = super::lock::exclusive(dir, MESSAGES_LOCK)?;
    let len = match std::fs::metadata(messages_path(dir)) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    if len <= max_bytes || len != read_to {
        return Ok(false);
    }
    std::fs::rename(messages_path(dir), rotated_path(dir))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(from: &str, to: &str, text: &str) -> AgentMessage {
        AgentMessage {
            unix: 1,
            from_pane: Some(from.into()),
            to_pane: to.into(),
            text: text.into(),
            outcome: "sent".into(),
            ..AgentMessage::default()
        }
    }

    #[test]
    fn appends_and_reads_back_the_newest_first_filtered() {
        let dir = super::super::test_dir("messages");
        assert!(recent(&dir, 10, None).is_empty());
        append(&dir, &message("a", "b", "one")).unwrap();
        append(&dir, &message("b", "c", "two")).unwrap();
        append(&dir, &message("c", "a", "three")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(messages_path(&dir))
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let raw = std::fs::read_to_string(messages_path(&dir)).unwrap();
        assert_eq!(raw.lines().count(), 4);
        let all = recent(&dir, 10, None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].text, "one");
        let last = recent(&dir, 2, None);
        assert_eq!(
            last.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["two", "three"]
        );
        let involving_a = recent(&dir, 10, Some("a"));
        assert_eq!(
            involving_a
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["one", "three"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_lines_without_the_new_fields_still_parse() {
        let line =
            r#"{"unix":5,"from_pane":"w1:p1","to_pane":"w1:p2","text":"hi","outcome":"sent"}"#;
        let message: AgentMessage = serde_json::from_str(line).unwrap();
        assert_eq!(message.text, "hi");
        assert_eq!(message.id, None);
        assert_eq!(message.kind, None);
        // And the new fields stay out of lines that do not use them.
        let written = serde_json::to_string(&message).unwrap();
        assert!(!written.contains("reply_to"), "{written}");
        let full = AgentMessage {
            id: Some("m1".into()),
            reply_to: Some("m0".into()),
            from_role: Some("lead".into()),
            kind: Some(KIND_REFUSAL.into()),
            ..message
        };
        let back: AgentMessage =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn new_ids_are_well_formed_and_distinct() {
        let ids: Vec<String> = (0..1000).map(|_| new_id()).collect();
        for id in &ids {
            assert!(id.starts_with('m'), "{id}");
            assert!(
                id[1..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase()),
                "{id}"
            );
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        assert_eq!(base36(0), "0");
        assert_eq!(base36(36 * 36 + 35), "10z");
    }

    #[test]
    fn since_offset_reads_only_new_complete_lines_and_restarts_after_rotation() {
        let dir = super::super::test_dir("messages-offset");
        assert_eq!(since_offset(&dir, 0), (Vec::new(), 0));
        assert_eq!(
            since_offset(&dir, 40),
            (Vec::new(), 0),
            "missing file resets"
        );
        append(&dir, &message("a", "b", "one")).unwrap();
        append(&dir, &message("b", "a", "two")).unwrap();
        let (first, offset) = since_offset(&dir, 0);
        assert_eq!(first.len(), 2);
        assert_eq!(
            offset,
            std::fs::metadata(messages_path(&dir)).unwrap().len()
        );
        assert_eq!(since_offset(&dir, offset), (Vec::new(), offset));

        // A line still being written is left for the next call.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(messages_path(&dir))
            .unwrap();
        let three = serde_json::to_string(&message("a", "c", "three")).unwrap();
        file.write_all(&three.as_bytes()[..10]).unwrap();
        assert_eq!(since_offset(&dir, offset), (Vec::new(), offset));
        file.write_all(&three.as_bytes()[10..]).unwrap();
        file.write_all(b"\nnot json\n").unwrap();
        let (new, offset) = since_offset(&dir, offset);
        assert_eq!(
            new.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["three"]
        );

        // Rotation: the log moves away and a fresh, shorter one starts.
        assert!(
            !rotate_if_large(&dir, 1 << 20, offset).unwrap(),
            "small enough"
        );
        assert!(
            !rotate_if_large(&dir, 10, offset - 1).unwrap(),
            "a line appended after the read stays until it is read"
        );
        assert!(rotate_if_large(&dir, 10, offset).unwrap());
        assert!(rotated_path(&dir).exists());
        assert!(
            !rotate_if_large(&dir, 10, 0).unwrap(),
            "nothing left to rotate"
        );
        assert_eq!(since_offset(&dir, offset), (Vec::new(), 0));
        append(&dir, &message("c", "a", "four")).unwrap();
        let (after, _) = since_offset(&dir, offset);
        assert_eq!(
            after.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["four"]
        );
        assert_eq!(recent(&dir, 10, None).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_reply_matches_by_reply_id_or_sender_whatever_the_outcome() {
        let dir = super::super::test_dir("messages-reply");
        let with =
            |unix: u64, from: &str, to: &str, text: &str, reply: Option<&str>| AgentMessage {
                unix,
                reply_to: reply.map(str::to_string),
                ..message(from, to, text)
            };
        append(&dir, &with(10, "rev", "lead", "stale", Some("m1"))).unwrap();
        append(&dir, &with(20, "lead", "rev", "question", None)).unwrap();
        append(&dir, &with(21, "other", "lead", "noise", None)).unwrap();
        // The waiter was working, so the reply was logged as a busy refusal.
        let busy = AgentMessage {
            outcome: "busy".into(),
            kind: Some(KIND_REFUSAL.into()),
            ..with(22, "rev", "lead", "hi", Some("m1"))
        };
        append(&dir, &busy).unwrap();
        append(&dir, &with(23, "rev", "lead", "later", None)).unwrap();
        let none = HashSet::new();

        assert_eq!(
            find_reply(&dir, "lead", Some("m1"), None, 20, None, &none),
            Some(busy.clone())
        );
        assert_eq!(
            find_reply(&dir, "lead", Some("m1"), None, 0, None, &none).map(|m| m.text),
            Some("stale".into()),
            "the oldest match after the start"
        );
        assert_eq!(
            find_reply(&dir, "lead", None, Some("rev"), 20, None, &none),
            Some(busy)
        );
        assert_eq!(
            find_reply(&dir, "lead", None, Some("rev"), 23, None, &none).map(|m| m.text),
            Some("later".into()),
            "same-second messages count"
        );
        assert_eq!(
            find_reply(&dir, "lead", None, None, 21, None, &none).map(|m| m.text),
            Some("noise".into())
        );
        assert_eq!(
            find_reply(&dir, "lead", Some("m9"), None, 0, None, &none),
            None
        );
        assert_eq!(
            find_reply(&dir, "rev", None, Some("rev"), 0, None, &none),
            None
        );
        assert_eq!(find_reply(&dir, "lead", None, None, 24, None, &none), None);

        // Anchored on the waiter's own question (log order): a same-second
        // message logged before it does not count, and a returned reply is
        // not returned again.
        let ask = AgentMessage {
            id: Some("mq".into()),
            ..with(30, "lead", "rev", "again?", None)
        };
        let early = AgentMessage {
            id: Some("ma".into()),
            ..with(30, "rev", "lead", "before", None)
        };
        append(&dir, &early).unwrap();
        append(&dir, &ask).unwrap();
        for (id, text) in [("mb", "first"), ("mc", "second")] {
            let reply = AgentMessage {
                id: Some(id.into()),
                ..with(30, "rev", "lead", text, None)
            };
            append(&dir, &reply).unwrap();
        }
        let mut returned = HashSet::new();
        let next = |returned: &HashSet<String>| {
            find_reply(&dir, "lead", None, Some("rev"), 30, Some("mq"), returned)
        };
        let first = next(&returned).unwrap();
        assert_eq!(first.text, "first");
        returned.insert(first.id.unwrap());
        assert_eq!(next(&returned).map(|m| m.text), Some("second".into()));
        returned.insert("mc".into());
        assert_eq!(next(&returned), None);
        // An anchor rotated away falls back to the clock.
        assert_eq!(
            find_reply(&dir, "lead", None, Some("rev"), 30, Some("mgone"), &none).map(|m| m.text),
            Some("before".into())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
