//! Checkpoint context from agents' native transcripts (fork): where a
//! checkpoint sits in the session (the anchor, captured when it is added) and
//! the prompt and reply around it (read later, on the notes worker).
//!
//! Everything here is plain, bounded file reading with no `App` state:
//! [`capture_anchor`] reads at most [`TAIL_BYTES`] from the end of the
//! transcript, [`context_at`] one window of [`BEFORE_BYTES`] +
//! [`AFTER_BYTES`] around the anchor (or the last [`RECOVERY_BYTES`] when the
//! file shrank), and [`find_codex_rollout`] lists at most
//! [`CODEX_MAX_DAY_DIRS`] day directories and [`CODEX_MAX_ENTRIES`] entries.
//! Nothing is ever written.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How much of a transcript's tail [`capture_anchor`] reads.
pub(crate) const TAIL_BYTES: u64 = 64 * 1024;
/// The window [`context_at`] reads before the anchor.
pub(crate) const BEFORE_BYTES: u64 = 192 * 1024;
/// The window [`context_at`] reads after the anchor.
pub(crate) const AFTER_BYTES: u64 = 64 * 1024;
/// The tail [`context_at`] reads when the file is shorter than the anchor.
pub(crate) const RECOVERY_BYTES: u64 = 256 * 1024;
/// Characters per side when the caller passes `0`.
pub(crate) const DEFAULT_CHARS: u32 = 600;
/// The most characters per side.
pub(crate) const MAX_CHARS: u32 = 4000;
/// Day directories [`find_codex_rollout`] looks into, newest first.
pub(crate) const CODEX_MAX_DAY_DIRS: usize = 45;
/// Directory entries [`find_codex_rollout`] reads in total.
pub(crate) const CODEX_MAX_ENTRIES: usize = 4000;

/// Which transcript format a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Flavor {
    /// Claude Code: one JSON object per line with `type`, `uuid` and `message`.
    Claude,
    /// Codex: rollout lines with `type` and `payload`.
    Codex,
}

/// Where a checkpoint sits in its transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Anchor {
    pub path: Option<PathBuf>,
    /// The transcript's length when the checkpoint was added.
    pub offset: Option<u64>,
    /// Claude only: the `uuid` of the last line before `offset`, to find the
    /// spot again after the file was rewritten or replaced by a backup.
    pub uuid: Option<String>,
}

/// The prompt and reply around an anchor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Extract {
    pub prompt: Option<String>,
    pub reply: Option<String>,
    pub prompt_ts: Option<u64>,
    pub reply_ts: Option<u64>,
    /// No prompt in the window: the turn started earlier (or the session
    /// continued from another one).
    pub continued: bool,
    /// The prompt or the reply was cut to `chars`.
    pub truncated: bool,
}

/// The anchor for a checkpoint added now: the file's length and, for Claude,
/// the `uuid` of the last complete line that has one within the last
/// [`TAIL_BYTES`]. `None` when the file cannot be read.
pub(crate) fn capture_anchor(path: &Path, flavor: Flavor) -> Option<Anchor> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let uuid = match flavor {
        Flavor::Claude => last_uuid(&mut file, len).ok().flatten(),
        Flavor::Codex => None,
    };
    Some(Anchor {
        path: Some(path.to_path_buf()),
        offset: Some(len),
        uuid,
    })
}

/// The prompt and reply around `anchor` in `path`, each cut to `chars`
/// characters (`0` = [`DEFAULT_CHARS`], at most [`MAX_CHARS`]).
pub(crate) fn context_at(
    path: &Path,
    anchor: &Anchor,
    flavor: Flavor,
    chars: u32,
) -> io::Result<Extract> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    extract_from(&mut file, len, anchor, flavor, chars)
}

/// The Codex rollout file of `session_id`
/// (`<codex_home>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`), newest day
/// first. Read-only and bounded: at most [`CODEX_MAX_DAY_DIRS`] day
/// directories and [`CODEX_MAX_ENTRIES`] directory entries in total.
pub(crate) fn find_codex_rollout(codex_home: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || session_id.len() > 512
        || session_id.contains(['/', '\\'])
        || session_id.contains("..")
    {
        return None;
    }
    let suffix = format!("-{session_id}.jsonl");
    let mut budget = CODEX_MAX_ENTRIES;
    let mut days_left = CODEX_MAX_DAY_DIRS;
    let sessions = codex_home.join("sessions");
    for year in numbered_dirs(&sessions, &mut budget)? {
        for month in numbered_dirs(&year, &mut budget)? {
            for day in numbered_dirs(&month, &mut budget)? {
                if days_left == 0 {
                    return None;
                }
                days_left -= 1;
                let Ok(entries) = fs::read_dir(&day) else {
                    continue;
                };
                for entry in entries.flatten() {
                    if budget == 0 {
                        return None;
                    }
                    budget -= 1;
                    let name = entry.file_name();
                    let matches = name.to_str().is_some_and(|name| name.ends_with(&suffix));
                    if matches && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                        return Some(entry.path());
                    }
                }
            }
        }
    }
    None
}

/// The all-digit subdirectories of `dir`, newest (largest) first; `None`
/// once the entry budget is spent. A missing directory is empty.
fn numbered_dirs(dir: &Path, budget: &mut usize) -> Option<Vec<PathBuf>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Some(Vec::new());
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let number = name.parse::<u32>().unwrap_or(0);
            dirs.push((number, entry.path()));
        }
    }
    dirs.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    Some(dirs.into_iter().map(|(_, path)| path).collect())
}

// ----- reading ------------------------------------------------------------

/// Bytes `[start, end)` of `reader`.
fn read_range<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(end.saturating_sub(start) as usize);
    reader.seek(SeekFrom::Start(start))?;
    reader
        .take(end.saturating_sub(start))
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// One complete line of a window: its byte range in the window buffer (`end`
/// is past the newline) and its parsed JSON.
struct Line {
    start: usize,
    end: usize,
    value: Value,
}

/// The complete JSON lines of a window that starts at file offset `start`
/// and ends at `end` of a file `len` bytes long. A first line cut by the
/// window start and a last line cut by the window end are dropped; a final
/// line without a newline at the end of the file is kept when it parses.
fn lines(buf: &[u8], start: u64, end: u64, len: u64) -> Vec<Line> {
    let mut out = Vec::new();
    let mut pos = 0;
    if start > 0 {
        match buf.iter().position(|&b| b == b'\n') {
            Some(newline) => pos = newline + 1,
            None => return out,
        }
    }
    while pos < buf.len() {
        let (line_end, next) = match buf[pos..].iter().position(|&b| b == b'\n') {
            Some(newline) => (pos + newline, pos + newline + 1),
            None if end >= len => (buf.len(), buf.len()),
            None => break,
        };
        let raw = &buf[pos..line_end];
        if !raw.iter().all(u8::is_ascii_whitespace) {
            if let Ok(value) = serde_json::from_slice::<Value>(raw) {
                out.push(Line {
                    start: pos,
                    end: next,
                    value,
                });
            }
        }
        pos = next;
    }
    out
}

/// The `uuid` of the last complete line that has one in the last
/// [`TAIL_BYTES`] of a Claude transcript.
fn last_uuid<R: Read + Seek>(reader: &mut R, len: u64) -> io::Result<Option<String>> {
    let start = len.saturating_sub(TAIL_BYTES);
    let buf = read_range(reader, start, len)?;
    Ok(lines(&buf, start, len, len)
        .iter()
        .rev()
        .find_map(|line| line.value["uuid"].as_str().map(str::to_string)))
}

/// The position just past the last line in `buf` whose `uuid` is `uuid`.
fn find_uuid(buf: &[u8], lines: &[Line], uuid: &str) -> Option<usize> {
    lines.iter().rev().find_map(|line| {
        let raw = &buf[line.start..line.end];
        let mentions = raw.windows(uuid.len()).any(|w| w == uuid.as_bytes());
        (mentions && line.value["uuid"].as_str() == Some(uuid)).then_some(line.end)
    })
}

/// [`context_at`] over any reader of known length.
fn extract_from<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    anchor: &Anchor,
    flavor: Flavor,
    chars: u32,
) -> io::Result<Extract> {
    let chars = match chars {
        0 => DEFAULT_CHARS,
        n => n.min(MAX_CHARS),
    } as usize;
    let uuid = anchor.uuid.as_deref().filter(|uuid| !uuid.is_empty());
    let (start, end, anchor_pos) = match anchor.offset {
        Some(offset) if offset <= len => {
            let start = offset.saturating_sub(BEFORE_BYTES);
            let end = offset.saturating_add(AFTER_BYTES).min(len);
            (start, end, Some((offset - start) as usize))
        }
        // Shrunk (rewritten, or a backup) or never measured: the tail, where
        // the uuid finds the spot again.
        _ => (len.saturating_sub(RECOVERY_BYTES), len, None),
    };
    let buf = read_range(reader, start, end)?;
    let lines = lines(&buf, start, end, len);
    let anchor_pos = match (anchor_pos, uuid) {
        (Some(pos), Some(uuid)) => {
            // The offset is only trusted when the line before it is still the
            // one the anchor named.
            let before = lines
                .iter()
                .rev()
                .filter(|line| line.end <= pos)
                .find_map(|line| line.value["uuid"].as_str());
            if before == Some(uuid) {
                pos
            } else {
                find_uuid(&buf, &lines, uuid).unwrap_or(pos)
            }
        }
        (Some(pos), None) => pos,
        (None, Some(uuid)) => find_uuid(&buf, &lines, uuid).unwrap_or(buf.len()),
        (None, None) => buf.len(),
    };
    let turns = match flavor {
        Flavor::Claude => claude_turns(&lines),
        Flavor::Codex => codex_turns(&lines),
    };
    Ok(pick(&turns, anchor_pos, chars))
}

// ----- extraction -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

/// One user prompt or one assistant text message in window order. `group`
/// joins lines of the same assistant message (Claude writes one content
/// block per line).
struct Turn {
    role: Role,
    end: usize,
    group: String,
    text: String,
    ts: Option<u64>,
}

/// Prompt: the last user turn before the anchor. Reply: the last assistant
/// message between it and the anchor, else the first one after the anchor
/// (before the next prompt).
fn pick(turns: &[Turn], anchor: usize, chars: usize) -> Extract {
    let before = turns.iter().take_while(|turn| turn.end <= anchor).count();
    let prompt = turns[..before]
        .iter()
        .rposition(|turn| turn.role == Role::User);
    let from = prompt.map_or(0, |index| index + 1);
    let reply = match turns[from..before]
        .iter()
        .rposition(|turn| turn.role == Role::Assistant)
    {
        Some(last) => Some(group_ending(&turns[from..before], last)),
        None => turns[before..]
            .iter()
            .take_while(|turn| turn.role == Role::Assistant)
            .next()
            .map(|_| group_starting(&turns[before..])),
    };
    let mut extract = Extract {
        continued: prompt.is_none(),
        ..Extract::default()
    };
    if let Some(index) = prompt {
        let (text, cut) = cut(&turns[index].text, chars);
        extract.prompt = Some(text);
        extract.prompt_ts = turns[index].ts;
        extract.truncated |= cut;
    }
    if let Some((text, ts)) = reply {
        let (text, cut) = cut(&text, chars);
        extract.reply = Some(text);
        extract.reply_ts = ts;
        extract.truncated |= cut;
    }
    extract
}

/// The text of the assistant message whose last line is `turns[last]`.
fn group_ending(turns: &[Turn], last: usize) -> (String, Option<u64>) {
    let group = &turns[last].group;
    let first = turns[..=last]
        .iter()
        .rposition(|turn| turn.role != Role::Assistant || &turn.group != group)
        .map_or(0, |index| index + 1);
    join(&turns[first..=last])
}

/// The text of the assistant message starting at `turns[0]`.
fn group_starting(turns: &[Turn]) -> (String, Option<u64>) {
    let group = &turns[0].group;
    let count = turns
        .iter()
        .take_while(|turn| turn.role == Role::Assistant && &turn.group == group)
        .count();
    join(&turns[..count])
}

fn join(turns: &[Turn]) -> (String, Option<u64>) {
    let text = turns
        .iter()
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    (text, turns.first().and_then(|turn| turn.ts))
}

/// `text` trimmed and cut to `chars` characters; whether it was cut.
fn cut(text: &str, chars: usize) -> (String, bool) {
    let text = text.trim();
    match text.char_indices().nth(chars) {
        Some((index, _)) => (text[..index].trim_end().to_string(), true),
        None => (text.to_string(), false),
    }
}

/// Claude user text that the user did not type: slash-command and shell
/// wrappers, injected reminders, interruption markers.
fn is_claude_wrapper(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty()
        || [
            "<command-",
            "<local-command-",
            "<system-reminder>",
            "<bash-",
            "<user-prompt-submit-hook>",
            "[Request interrupted by user",
            "Caveat: The messages below were generated by the user while running local commands",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// The text blocks of a message `content` (a string or an array of blocks of
/// `block_type`), wrappers dropped.
fn texts(content: &Value, block_type: &str, keep: impl Fn(&str) -> bool) -> Vec<String> {
    match content {
        Value::String(text) => [text.clone()]
            .into_iter()
            .filter(|text| keep(text))
            .collect(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"].as_str() == Some(block_type))
            .filter_map(|block| block["text"].as_str())
            .filter(|text| keep(text))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn claude_turns(lines: &[Line]) -> Vec<Turn> {
    let mut turns = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let value = &line.value;
        if value["isSidechain"].as_bool() == Some(true) || value["isMeta"].as_bool() == Some(true) {
            continue;
        }
        let role = match value["type"].as_str() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        let content = &value["message"]["content"];
        let blocks = match role {
            Role::User => texts(content, "text", |text| !is_claude_wrapper(text)),
            Role::Assistant => texts(content, "text", |text| !text.trim().is_empty()),
        };
        if blocks.is_empty() {
            // Tool results, tool calls, thinking and wrappers only.
            continue;
        }
        let group = value["message"]["id"]
            .as_str()
            .or_else(|| value["uuid"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{index}"));
        turns.push(Turn {
            role,
            end: line.end,
            group,
            text: blocks.join("\n\n"),
            ts: value["timestamp"].as_str().and_then(parse_timestamp),
        });
    }
    turns
}

/// Codex user text that the user did not type: the session's environment
/// and instruction preambles.
fn is_codex_preamble(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty()
        || text.starts_with("<environment_context>")
        || text.starts_with("<user_instructions>")
}

fn codex_turns(lines: &[Line]) -> Vec<Turn> {
    let messages: Vec<Turn> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let value = &line.value;
            if value["type"].as_str() != Some("response_item") {
                return None;
            }
            let payload = &value["payload"];
            if payload["type"].as_str() != Some("message") {
                return None;
            }
            let (role, blocks) = match payload["role"].as_str() {
                Some("user") => (
                    Role::User,
                    texts(&payload["content"], "input_text", |text| {
                        !is_codex_preamble(text)
                    }),
                ),
                Some("assistant") => (
                    Role::Assistant,
                    texts(&payload["content"], "output_text", |text| {
                        !text.trim().is_empty()
                    }),
                ),
                _ => return None,
            };
            codex_turn(line, index, role, blocks)
        })
        .collect();
    if !messages.is_empty() {
        return messages;
    }
    // Older or partial rollouts: the UI events.
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let value = &line.value;
            if value["type"].as_str() != Some("event_msg") {
                return None;
            }
            let payload = &value["payload"];
            let role = match payload["type"].as_str() {
                Some("user_message") => Role::User,
                Some("agent_message") => Role::Assistant,
                _ => return None,
            };
            let text = payload["message"].as_str().unwrap_or("");
            let keep = match role {
                Role::User => !is_codex_preamble(text),
                Role::Assistant => !text.trim().is_empty(),
            };
            codex_turn(
                line,
                index,
                role,
                keep.then(|| text.to_string()).into_iter().collect(),
            )
        })
        .collect()
}

fn codex_turn(line: &Line, index: usize, role: Role, blocks: Vec<String>) -> Option<Turn> {
    if blocks.is_empty() {
        return None;
    }
    Some(Turn {
        role,
        end: line.end,
        // Codex writes each message whole.
        group: format!("line-{index}"),
        text: blocks.join("\n\n"),
        ts: line.value["timestamp"].as_str().and_then(parse_timestamp),
    })
}

/// Unix seconds of an RFC 3339 timestamp (`2025-06-01T12:00:00.123Z`,
/// `…+02:00`; no zone means UTC).
fn parse_timestamp(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = b.get(range)?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    };
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut pos = 19;
    if b.get(pos) == Some(&b'.') {
        pos += 1;
        while b.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
    }
    let offset = match b.get(pos) {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = num(pos + 1..pos + 3)?;
            let minutes = num(pos + 4..pos + 6)?;
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                seconds
            } else {
                -seconds
            }
        }
        Some(_) => return None,
    };
    let unix =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(unix).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Write};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-transcript-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn ts(second: u32) -> String {
        format!("2025-06-01T12:00:{second:02}.000Z")
    }

    fn user(uuid: &str, content: Value, second: u32) -> String {
        json!({ "type": "user", "uuid": uuid, "isSidechain": false, "timestamp": ts(second),
            "message": { "role": "user", "content": content } })
        .to_string()
    }

    fn assistant(uuid: &str, id: &str, content: Value, second: u32) -> String {
        json!({ "type": "assistant", "uuid": uuid, "isSidechain": false, "timestamp": ts(second),
            "message": { "id": id, "role": "assistant", "content": content } })
        .to_string()
    }

    fn said(text: &str) -> Value {
        json!([{ "type": "text", "text": text }])
    }

    fn write(path: &Path, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        fs::write(path, text).expect("write transcript");
    }

    fn append(path: &Path, lines: &[String]) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open transcript");
        for line in lines {
            writeln!(file, "{line}").expect("append");
        }
    }

    const T0: u64 = 1_748_779_200; // 2025-06-01T12:00:00Z

    #[test]
    fn claude_prompt_and_reply_around_the_anchor() {
        let dir = temp_dir("claude");
        let path = dir.join("s.jsonl");
        write(
            &path,
            &[
                json!({ "type": "summary", "summary": "old" }).to_string(),
                user("u1", json!("first prompt"), 1),
                assistant("a1", "m1", said("first reply"), 2),
                user("u2", json!("second prompt"), 3),
                assistant("a2", "m2", said("part one"), 4),
                assistant(
                    "a3",
                    "m2",
                    json!([{ "type": "tool_use", "id": "t", "name": "Bash", "input": {} }]),
                    5,
                ),
                assistant("a4", "m2", said("part two"), 6),
                json!({ "type": "file-history-snapshot", "snapshot": {} }).to_string(),
            ],
        );
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        assert_eq!(
            anchor.uuid.as_deref(),
            Some("a4"),
            "lines without a uuid are skipped"
        );
        assert_eq!(anchor.offset, Some(fs::metadata(&path).unwrap().len()));
        assert_eq!(anchor.path.as_deref(), Some(path.as_path()));
        append(
            &path,
            &[
                user("u3", json!("third prompt"), 7),
                assistant("a5", "m3", said("third reply"), 8),
            ],
        );
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("second prompt"));
        assert_eq!(extract.reply.as_deref(), Some("part one\n\npart two"));
        assert_eq!(extract.prompt_ts, Some(T0 + 3));
        assert_eq!(extract.reply_ts, Some(T0 + 4));
        assert!(!extract.continued && !extract.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checkpoint_before_any_reply_takes_the_next_reply_of_the_same_turn() {
        let dir = temp_dir("claude-after");
        let path = dir.join("s.jsonl");
        write(&path, &[user("u1", json!("do it"), 1)]);
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        append(&path, &[assistant("a1", "m1", said("done"), 2)]);
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("do it"));
        assert_eq!(extract.reply.as_deref(), Some("done"));
        assert_eq!(extract.reply_ts, Some(T0 + 2));

        // A reply to a later prompt is not this checkpoint's reply.
        write(&path, &[user("u1", json!("do it"), 1)]);
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        append(
            &path,
            &[
                user("u2", json!("something else"), 2),
                assistant("a2", "m2", said("other"), 3),
            ],
        );
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("do it"));
        assert_eq!(extract.reply, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_results_sidechains_meta_and_wrappers_are_not_prompts() {
        let dir = temp_dir("claude-skip");
        let path = dir.join("s.jsonl");
        let sidechain = |kind: &str, content: Value| {
            json!({ "type": kind, "uuid": "x", "isSidechain": true, "timestamp": ts(9),
                "message": { "id": "side", "role": kind, "content": content } })
            .to_string()
        };
        write(
            &path,
            &[
                user(
                    "u1",
                    json!([
                        { "type": "text", "text": "<system-reminder>be nice</system-reminder>" },
                        { "type": "text", "text": "real prompt" },
                    ]),
                    1,
                ),
                assistant("a1", "m1", said("real reply"), 2),
                user(
                    "u2",
                    json!([{ "type": "tool_result", "tool_use_id": "t", "content": "ok" }]),
                    3,
                ),
                sidechain("user", json!("subagent prompt")),
                sidechain("assistant", said("subagent reply")),
                json!({ "type": "user", "uuid": "u3", "isMeta": true, "timestamp": ts(4),
                    "message": { "role": "user", "content": "Caveat: generated by local commands" } })
                .to_string(),
                user(
                    "u4",
                    json!("<command-name>/compact</command-name>\n<command-message>compact</command-message>"),
                    5,
                ),
                user("u5", json!("<local-command-stdout>ok</local-command-stdout>"), 6),
                user("u6", said("[Request interrupted by user]"), 7),
            ],
        );
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("real prompt"));
        assert_eq!(extract.reply.as_deref(), Some("real reply"));
        assert!(!extract.continued);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_prompt_outside_the_window_is_continued() {
        let dir = temp_dir("claude-continued");
        let path = dir.join("s.jsonl");
        let filler = "x".repeat(1000);
        let mut lines = vec![user("u1", json!("long ago"), 1)];
        // More than BEFORE_BYTES of assistant output after the prompt.
        for index in 0..220 {
            lines.push(assistant(
                &format!("a{index}"),
                &format!("m{index}"),
                said(&filler),
                2,
            ));
        }
        lines.push(assistant("last", "m-last", said("latest words"), 3));
        write(&path, &lines);
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert!(extract.continued);
        assert_eq!(extract.prompt, None);
        assert_eq!(extract.reply.as_deref(), Some("latest words"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_window_ends_after_the_anchor_and_sides_are_cut_to_chars() {
        let dir = temp_dir("claude-caps");
        let path = dir.join("s.jsonl");
        let long: String = "é".repeat(5000);
        write(&path, &[user("u1", json!(long), 1)]);
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        // The only reply starts past AFTER_BYTES after the anchor.
        append(
            &path,
            &[
                user(
                    "u2",
                    json!([{ "type": "tool_result", "tool_use_id": "t", "content": "y".repeat(70 * 1024) }]),
                    2,
                ),
                assistant("a1", "m1", said("too far"), 3),
            ],
        );
        let extract = context_at(&path, &anchor, Flavor::Claude, 100).expect("context");
        assert_eq!(extract.reply, None, "past the window");
        let prompt = extract.prompt.expect("prompt");
        assert_eq!(prompt.chars().count(), 100);
        assert!(extract.truncated);
        let default = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(default.prompt.map(|p| p.chars().count()), Some(600));
        let max = context_at(&path, &anchor, Flavor::Claude, 1_000_000).expect("context");
        assert_eq!(max.prompt.map(|p| p.chars().count()), Some(4000));
        let whole = context_at(&path, &anchor, Flavor::Claude, 4000);
        assert!(whole.is_ok_and(|e| e.truncated));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_shrunk_or_replaced_file_finds_the_anchor_by_uuid() {
        let dir = temp_dir("claude-shrunk");
        let path = dir.join("s.jsonl");
        let padding = "p".repeat(8 * 1024);
        write(
            &path,
            &[
                user("u1", json!("old prompt"), 1),
                assistant("a1", "m1", said(&padding), 2),
                user("u2", json!("anchored prompt"), 3),
                assistant("a2", "m2", said("anchored reply"), 4),
            ],
        );
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        assert_eq!(anchor.uuid.as_deref(), Some("a2"));
        // Rewritten without the old turn, then grown a little: shorter than
        // the anchor's offset.
        write(
            &path,
            &[
                user("u2", json!("anchored prompt"), 3),
                assistant("a2", "m2", said("anchored reply"), 4),
                user("u3", json!("later prompt"), 5),
                assistant("a3", "m3", said("later reply"), 6),
            ],
        );
        assert!(fs::metadata(&path).unwrap().len() < anchor.offset.unwrap());
        let extract = context_at(&path, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("anchored prompt"));
        assert_eq!(extract.reply.as_deref(), Some("anchored reply"));

        // Without a uuid the end of the file stands in.
        let blind = Anchor {
            uuid: None,
            ..anchor.clone()
        };
        let extract = context_at(&path, &blind, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("later prompt"));

        // A backup at least as long, with the line at another offset.
        let backup = dir.join("backup.jsonl");
        write(
            &backup,
            &[
                user("u0", json!("x".repeat(9 * 1024)), 0),
                user("u1", json!("old prompt"), 1),
                assistant("a1", "m1", said(&padding), 2),
                user("u2", json!("anchored prompt"), 3),
                assistant("a2", "m2", said("anchored reply"), 4),
                user("u3", json!("later prompt"), 5),
            ],
        );
        assert!(fs::metadata(&backup).unwrap().len() >= anchor.offset.unwrap());
        let extract = context_at(&backup, &anchor, Flavor::Claude, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("anchored prompt"));
        assert_eq!(extract.reply.as_deref(), Some("anchored reply"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_reads_only_complete_lines_and_a_missing_file_has_no_anchor() {
        let dir = temp_dir("claude-partial");
        let path = dir.join("s.jsonl");
        let mut text = user("u1", json!("hi"), 1);
        text.push('\n');
        text.push_str(r#"{"type":"assistant","uuid":"half"#);
        fs::write(&path, &text).unwrap();
        let anchor = capture_anchor(&path, Flavor::Claude).expect("anchor");
        assert_eq!(anchor.uuid.as_deref(), Some("u1"));
        assert_eq!(anchor.offset, Some(text.len() as u64));
        assert_eq!(
            capture_anchor(&dir.join("missing.jsonl"), Flavor::Claude),
            None
        );
        assert!(context_at(&dir.join("missing.jsonl"), &anchor, Flavor::Claude, 0).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reader_is_pure_over_any_seekable_reader() {
        let text = format!(
            "{}\n{}\n",
            user("u1", json!("from memory"), 1),
            assistant("a1", "m1", said("answer"), 2)
        );
        let len = text.len() as u64;
        let mut cursor = Cursor::new(text.into_bytes());
        assert_eq!(last_uuid(&mut cursor, len).unwrap().as_deref(), Some("a1"));
        let anchor = Anchor {
            path: None,
            offset: Some(len),
            uuid: Some("a1".into()),
        };
        let extract = extract_from(&mut cursor, len, &anchor, Flavor::Claude, 0).unwrap();
        assert_eq!(extract.prompt.as_deref(), Some("from memory"));
        assert_eq!(extract.reply.as_deref(), Some("answer"));
    }

    fn codex(kind: &str, payload: Value, second: u32) -> String {
        json!({ "timestamp": ts(second), "type": kind, "payload": payload }).to_string()
    }

    fn codex_message(role: &str, block: &str, text: &str, second: u32) -> String {
        codex(
            "response_item",
            json!({ "type": "message", "role": role, "content": [{ "type": block, "text": text }] }),
            second,
        )
    }

    #[test]
    fn codex_rollouts_skip_the_session_preambles() {
        let dir = temp_dir("codex");
        let path = dir.join("rollout-2025-06-01T12-00-00-abc.jsonl");
        write(
            &path,
            &[
                codex("session_meta", json!({ "id": "abc", "cwd": "/w" }), 0),
                codex_message(
                    "user",
                    "input_text",
                    "<environment_context>\n  <cwd>/w</cwd>\n</environment_context>",
                    0,
                ),
                codex_message(
                    "user",
                    "input_text",
                    "<user_instructions>be terse</user_instructions>",
                    0,
                ),
                codex_message("developer", "input_text", "system rules", 0),
                codex_message("user", "input_text", "fix the bug", 1),
                codex(
                    "event_msg",
                    json!({ "type": "user_message", "message": "fix the bug" }),
                    1,
                ),
                codex(
                    "response_item",
                    json!({ "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking" }] }),
                    2,
                ),
                codex(
                    "response_item",
                    json!({ "type": "function_call", "name": "shell", "arguments": "{}" }),
                    2,
                ),
                codex_message("assistant", "output_text", "fixed it", 3),
                codex(
                    "event_msg",
                    json!({ "type": "agent_message", "message": "fixed it" }),
                    3,
                ),
            ],
        );
        let anchor = capture_anchor(&path, Flavor::Codex).expect("anchor");
        assert_eq!(anchor.uuid, None);
        append(
            &path,
            &[codex_message("user", "input_text", "next task", 4)],
        );
        let extract = context_at(&path, &anchor, Flavor::Codex, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("fix the bug"));
        assert_eq!(extract.reply.as_deref(), Some("fixed it"));
        assert_eq!(extract.prompt_ts, Some(T0 + 1));
        assert_eq!(extract.reply_ts, Some(T0 + 3));
        assert!(!extract.continued);

        // Only UI events: the fallback reads them.
        let events = dir.join("events.jsonl");
        write(
            &events,
            &[
                codex(
                    "event_msg",
                    json!({ "type": "user_message", "message": "<environment_context>x</environment_context>" }),
                    0,
                ),
                codex(
                    "event_msg",
                    json!({ "type": "user_message", "message": "explain" }),
                    1,
                ),
                codex(
                    "event_msg",
                    json!({ "type": "agent_message", "message": "because" }),
                    2,
                ),
            ],
        );
        let anchor = capture_anchor(&events, Flavor::Codex).expect("anchor");
        let extract = context_at(&events, &anchor, Flavor::Codex, 0).expect("context");
        assert_eq!(extract.prompt.as_deref(), Some("explain"));
        assert_eq!(extract.reply.as_deref(), Some("because"));
        let _ = fs::remove_dir_all(&dir);
    }

    fn day(home: &Path, month: u32, day: u32) -> PathBuf {
        let dir = home.join(format!("sessions/2025/{month:02}/{day:02}"));
        fs::create_dir_all(&dir).expect("day dir");
        dir
    }

    /// 50 day directories: February 1–19 (newest) and January 1–31. The 45th
    /// newest is January 6.
    fn codex_home(name: &str) -> PathBuf {
        let home = temp_dir(name);
        for d in 1..=19 {
            day(&home, 2, d);
        }
        for d in 1..=31 {
            day(&home, 1, d);
        }
        home
    }

    #[test]
    fn find_codex_rollout_looks_into_at_most_45_days_newest_first() {
        let home = codex_home("codex-days");
        let id = "0198-aaaa";
        let newest = day(&home, 2, 19).join(format!("rollout-2025-02-19T09-00-00-{id}.jsonl"));
        fs::write(&newest, "{}\n").unwrap();
        let older = day(&home, 1, 3).join(format!("rollout-2025-01-03T09-00-00-{id}.jsonl"));
        fs::write(&older, "{}\n").unwrap();
        assert_eq!(find_codex_rollout(&home, id), Some(newest));
        assert_eq!(find_codex_rollout(&home, "0198-bbbb"), None);
        let in_reach = day(&home, 1, 6).join("rollout-2025-01-06T09-00-00-in-reach.jsonl");
        fs::write(&in_reach, "{}\n").unwrap();
        assert_eq!(find_codex_rollout(&home, "in-reach"), Some(in_reach));
        let too_old = day(&home, 1, 5).join("rollout-2025-01-05T09-00-00-too-old.jsonl");
        fs::write(&too_old, "{}\n").unwrap();
        assert_eq!(find_codex_rollout(&home, "too-old"), None, "the 46th day");
        assert_eq!(find_codex_rollout(&home, "../x"), None);
        assert_eq!(find_codex_rollout(&home, ""), None);
        assert_eq!(find_codex_rollout(&home.join("absent"), id), None);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn find_codex_rollout_stops_after_4000_entries() {
        let home = temp_dir("codex-entries");
        let crowded = day(&home, 2, 2);
        for index in 0..CODEX_MAX_ENTRIES {
            fs::write(crowded.join(format!("rollout-x-{index}.jsonl")), "").unwrap();
        }
        let target = day(&home, 2, 1).join("rollout-2025-02-01T09-00-00-needle.jsonl");
        fs::write(&target, "{}\n").unwrap();
        assert_eq!(find_codex_rollout(&home, "needle"), None);
        fs::remove_dir_all(&crowded).unwrap();
        assert_eq!(find_codex_rollout(&home, "needle"), Some(target));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn timestamps_parse_with_zones_and_fractions() {
        assert_eq!(parse_timestamp("2025-06-01T12:00:00Z"), Some(T0));
        assert_eq!(parse_timestamp("2025-06-01T12:00:00.123456Z"), Some(T0));
        assert_eq!(parse_timestamp("2025-06-01T14:00:00+02:00"), Some(T0));
        assert_eq!(parse_timestamp("2025-06-01T10:30:00-01:30"), Some(T0));
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_timestamp("yesterday"), None);
        assert_eq!(parse_timestamp("2025-13-01T00:00:00Z"), None);
        assert_eq!(parse_timestamp("1969-12-31T23:59:59Z"), None);
    }
}
