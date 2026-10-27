//! Fork: Claude Code session chains.
//!
//! When the user backgrounds a Claude Code session, Claude's daemon carries
//! it on as a fork-resume (`--fork-session --resume <old>.jsonl`) outside the
//! pane, under a new session id, and appends one record to the old
//! session's transcript:
//!
//! ```text
//! {"type":"continued-in","timestamp":"…","sessionId":"<old>","continuedInSessionId":"<new>"}
//! ```
//!
//! The pane still names the old id (read from its command line or its
//! hooks), so a resume would reopen the conversation as it stood before the
//! fork. [`newest_session_in`] follows those records to the newest link.
//!
//! A link counts only while it is the old transcript's last word: when a
//! conversation entry (any line with a `"uuid"`) follows the record, the old
//! session was resumed in place afterwards and the link is stale. Only small
//! bookkeeping lines (`last-prompt`, `cost-state`) follow a live link, so the
//! record is looked for in the transcript's last [`TAIL_BYTES`] only;
//! transcripts run to hundreds of megabytes.
//!
//! Each transcript's link is cached by (path, modification time, length).
//! This reads files: it runs on identity and resume events (a restore, an
//! activation or restart, a reopen, a caller resolved by its session id),
//! never on render, PTY or detection paths.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind, PersistedAgentSession};

/// The most links followed from one session.
pub const MAX_DEPTH: usize = 16;
/// How much of a transcript's end is searched for its `continued-in` record.
pub const TAIL_BYTES: u64 = 256 * 1024;
/// Cached transcripts before the cache starts over.
const CACHE_CAP: usize = 512;

const RECORD_TYPE: &str = "continued-in";

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

type Cache = HashMap<PathBuf, (Stamp, Option<String>)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A session id that can name a transcript file in its project directory.
fn valid_id(id: &str) -> bool {
    AgentSessionRef::id(id).is_some() && crate::agent_resume::is_safe_path_component(id)
}

fn transcript_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jsonl"))
}

/// The session `id`'s transcript (in `dir`) says it was continued in, if
/// its last record says so and nothing was written to it since. Missing or
/// unreadable files have no link.
fn continued_in(dir: &Path, id: &str) -> Option<String> {
    let path = transcript_file(dir, id);
    let meta = std::fs::metadata(&path)
        .ok()
        .filter(|meta| meta.is_file())?;
    let stamp = Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    };
    if let Ok(cache) = cache().lock() {
        if let Some((cached, link)) = cache.get(&path) {
            if *cached == stamp {
                return link.clone();
            }
        }
    }
    let link = read_tail(&path, stamp.len)
        .ok()
        .and_then(|(tail, partial)| link_in_tail(&tail, partial, id));
    if let Ok(mut cache) = cache().lock() {
        if cache.len() >= CACHE_CAP {
            cache.clear();
        }
        cache.insert(path, (stamp, link.clone()));
    }
    link
}

/// The file's last [`TAIL_BYTES`]; `true` when that starts mid-file (its
/// first line is partial).
fn read_tail(path: &Path, len: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = std::fs::File::open(path)?;
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut tail)?;
    Ok((tail, start > 0))
}

/// The link the transcript tail of session `id` ends on (see the module
/// docs). A record for another session, a garbled record or an invalid id
/// end it with none.
fn link_in_tail(tail: &[u8], partial: bool, id: &str) -> Option<String> {
    let mut lines = tail.split(|byte| *byte == b'\n');
    if partial {
        lines.next();
    }
    let mut link = None;
    for line in lines {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let names_record = contains(line, RECORD_TYPE.as_bytes());
        if !names_record {
            if contains(line, b"\"uuid\"") {
                // The conversation went on in this session.
                link = None;
            }
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            link = None;
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some(RECORD_TYPE) {
            if value.get("uuid").is_some() {
                link = None;
            }
            continue;
        }
        let from = value.get("sessionId").and_then(serde_json::Value::as_str);
        let to = value
            .get("continuedInSessionId")
            .and_then(serde_json::Value::as_str);
        link = match (from, to) {
            (Some(from), Some(to)) if from == id && to != id && valid_id(to) => {
                Some(to.to_string())
            }
            _ => None,
        };
    }
    link
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Every session `id` (in project directory `dir`) was continued in, oldest
/// first, `id` itself not included. A link is followed only when its
/// transcript exists in `dir`; the walk stops at [`MAX_DEPTH`] links, at a
/// cycle, or at a missing or garbled transcript (the last good id ends it).
pub fn chain_in(dir: &Path, id: &str) -> Vec<String> {
    let mut chain = Vec::new();
    if !valid_id(id) {
        return chain;
    }
    let mut seen = HashSet::from([id.to_string()]);
    let mut current = id.to_string();
    while chain.len() < MAX_DEPTH {
        let Some(next) = continued_in(dir, &current) else {
            break;
        };
        if !seen.insert(next.clone()) || !transcript_file(dir, &next).is_file() {
            break;
        }
        chain.push(next.clone());
        current = next;
    }
    chain
}

/// The newest session of `id`'s chain in `dir` (`id` itself without one).
#[cfg(test)]
fn newest_session_in(dir: &Path, id: &str) -> String {
    chain_in(dir, id).pop().unwrap_or_else(|| id.to_string())
}

/// The Claude project directory a session's transcript lives in.
fn project_dir(session: &PersistedAgentSession) -> Option<PathBuf> {
    if !is_claude_id(session) {
        return None;
    }
    let file = crate::agent_resume::native_transcript_locations(session)?.file;
    file.parent().map(Path::to_path_buf)
}

fn is_claude_id(session: &PersistedAgentSession) -> bool {
    session.source == "herdr:claude"
        && session.agent == "claude"
        && session.session_ref.kind == AgentSessionRefKind::Id
}

/// Every later session of a Claude session's chain, oldest first; empty for
/// other agents, an unknown transcript, or no chain.
pub fn session_chain(session: &PersistedAgentSession) -> Vec<String> {
    match project_dir(session) {
        Some(dir) => chain_in(&dir, &session.session_ref.value),
        None => Vec::new(),
    }
}

/// The same Claude session advanced to the newest link of its chain (its id
/// and transcript path), or `None` when there is nothing to advance.
pub fn advance(session: &PersistedAgentSession) -> Option<PersistedAgentSession> {
    let dir = project_dir(session)?;
    let newest = chain_in(&dir, &session.session_ref.value).pop()?;
    let session_ref = AgentSessionRef::id(newest.clone())?;
    tracing::info!(
        from = %session.session_ref.value,
        to = %newest,
        "the Claude session was continued in another session; following it"
    );
    Some(PersistedAgentSession {
        source: session.source.clone(),
        agent: session.agent.clone(),
        session_ref,
        transcript_path: Some(transcript_file(&dir, &newest)),
    })
}

/// [`advance`], or the session unchanged.
pub fn advanced(session: PersistedAgentSession) -> PersistedAgentSession {
    advance(&session).unwrap_or(session)
}

/// Claude-shaped project directories for tests (here and in the app).
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use crate::agent_resume::{AgentSessionRef, PersistedAgentSession};

    /// `<temp>/<name>/projects/-tmp-project`, removed with its root on drop.
    pub(crate) struct Project {
        pub(crate) dir: PathBuf,
        root: PathBuf,
    }

    impl Project {
        pub(crate) fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("herdr-session-chain-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let dir = root.join("projects").join("-tmp-project");
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir, root }
        }

        pub(crate) fn file(&self, id: &str) -> PathBuf {
            super::transcript_file(&self.dir, id)
        }

        pub(crate) fn write(&self, id: &str, lines: &[String]) {
            let mut text = lines.join("\n");
            text.push('\n');
            std::fs::write(self.file(id), text).unwrap();
        }

        /// A transcript for `id` with one conversation entry, ending on a
        /// `continued-in` record for `next` when given.
        pub(crate) fn session_file(&self, id: &str, next: Option<&str>) {
            let mut lines = vec![entry(id)];
            lines.extend(next.map(|next| record(id, next)));
            self.write(id, &lines);
        }

        /// The Claude session `id`, its transcript path in this project.
        pub(crate) fn session(&self, id: &str) -> PersistedAgentSession {
            PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: AgentSessionRef::id(id).unwrap(),
                transcript_path: Some(self.file(id)),
            }
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(crate) fn entry(id: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"u-{id}","sessionId":"{id}","message":{{"content":"hi"}}}}"#
        )
    }

    /// The record Claude Code appends when it continues `from` in `to`.
    pub(crate) fn record(from: &str, to: &str) -> String {
        format!(
            r#"{{"type":"continued-in","timestamp":"2026-10-26T22:05:06.662Z","sessionId":"{from}","continuedInSessionId":"{to}"}}"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{entry, record, Project};
    use super::*;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

    fn bookkeeping(id: &str) -> String {
        format!(r#"{{"type":"last-prompt","lastPrompt":"go","sessionId":"{id}"}}"#)
    }

    #[test]
    fn a_single_link_is_followed() {
        let dir = Project::new("single");
        dir.write(A, &[entry(A), record(A, B), bookkeeping(A)]);
        dir.write(B, &[entry(B)]);
        assert_eq!(chain_in(&dir.dir, A), vec![B.to_string()]);
        assert_eq!(newest_session_in(&dir.dir, A), B);
        assert_eq!(newest_session_in(&dir.dir, B), B, "the newest has no link");
    }

    #[test]
    fn a_chain_is_followed_to_its_newest_session() {
        let dir = Project::new("chain");
        dir.write(A, &[entry(A), record(A, B)]);
        dir.write(B, &[entry(B), record(B, C)]);
        dir.write(C, &[entry(C)]);
        assert_eq!(chain_in(&dir.dir, A), vec![B.to_string(), C.to_string()]);
        assert_eq!(newest_session_in(&dir.dir, A), C);
    }

    #[test]
    fn a_cycle_stops_at_the_last_new_session() {
        let dir = Project::new("cycle");
        dir.write(A, &[entry(A), record(A, B)]);
        dir.write(B, &[entry(B), record(B, A)]);
        assert_eq!(chain_in(&dir.dir, A), vec![B.to_string()]);
        assert_eq!(newest_session_in(&dir.dir, A), B);
    }

    #[test]
    fn a_missing_transcript_stops_at_the_last_good_session() {
        let dir = Project::new("missing");
        dir.write(A, &[entry(A), record(A, B)]);
        dir.write(B, &[entry(B), record(B, C)]);
        // C was never written.
        assert_eq!(newest_session_in(&dir.dir, A), B);
        assert_eq!(newest_session_in(&dir.dir, C), C, "no transcript at all");
    }

    #[test]
    fn a_malformed_record_is_no_link() {
        let dir = Project::new("malformed");
        dir.write(
            A,
            &[entry(A), r#"{"type":"continued-in","sessionId":"#.into()],
        );
        dir.write(B, &[entry(B), record(A, C)]);
        dir.write(C, &[entry(C), record(C, "../../etc/passwd")]);
        assert_eq!(newest_session_in(&dir.dir, A), A, "garbled JSON");
        assert_eq!(
            newest_session_in(&dir.dir, B),
            B,
            "another session's record"
        );
        assert_eq!(newest_session_in(&dir.dir, C), C, "an unsafe id");
    }

    #[test]
    fn a_session_resumed_in_place_after_the_record_has_no_link() {
        // Seen in real transcripts: the old session was continued in another
        // one, then resumed directly and written to again.
        let dir = Project::new("stale");
        dir.write(A, &[entry(A), record(A, B), bookkeeping(A), entry(A)]);
        dir.write(B, &[entry(B)]);
        assert_eq!(newest_session_in(&dir.dir, A), A);
    }

    #[test]
    fn the_record_is_found_in_the_tail_of_a_long_transcript() {
        let dir = Project::new("long");
        let filler = format!(
            r#"{{"type":"user","uuid":"x","pad":"{}"}}"#,
            "p".repeat(4096)
        );
        let mut lines: Vec<String> = std::iter::repeat_n(filler, 200).collect();
        lines.push(record(A, B));
        dir.write(A, &lines);
        dir.write(B, &[entry(B)]);
        assert!(std::fs::metadata(dir.file(A)).unwrap().len() > TAIL_BYTES);
        assert_eq!(newest_session_in(&dir.dir, A), B);
    }

    #[test]
    fn the_cache_sees_a_rewritten_transcript() {
        let dir = Project::new("cache");
        dir.write(B, &[entry(B)]);
        dir.write(A, &[entry(A)]);
        assert_eq!(newest_session_in(&dir.dir, A), A);
        dir.write(A, &[entry(A), record(A, B)]);
        assert_eq!(newest_session_in(&dir.dir, A), B, "the length changed");
    }

    #[test]
    fn a_claude_session_advances_with_its_transcript_path() {
        let dir = Project::new("advance");
        dir.write(A, &[entry(A), record(A, B)]);
        dir.write(B, &[entry(B)]);
        let session = PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(A).unwrap(),
            transcript_path: Some(dir.file(A)),
        };
        let next = advance(&session).unwrap();
        assert_eq!(next.session_ref.value, B);
        assert_eq!(next.transcript_path, Some(dir.file(B)));
        assert_eq!(session_chain(&session), vec![B.to_string()]);
        assert!(advance(&next).is_none(), "the newest stays");
        let codex = PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            ..session
        };
        assert!(advance(&codex).is_none(), "only Claude sessions chain");
    }
}
