//! Fork: how full an agent's context window is, read from the tail of the
//! agent's own session file (no PTY, no screen text).
//!
//! Evidence (Claude Code transcripts on this machine, 2026-10): every
//! `type: "assistant"` line carries `message.model` (`claude-opus-5-5`,
//! `claude-fable-5-1`, `claude-sonnet-5`, `claude-haiku-4-5-20251001`, …,
//! and `<synthetic>` for locally made messages with zero usage) and
//! `message.usage` with `input_tokens`, `cache_creation_input_tokens`,
//! `cache_read_input_tokens` and `output_tokens`. No model id carries a
//! `[1m]` marker, yet the same ids exceed 200k tokens in use, so the window
//! is inferred: an explicit `[agents] context_window` value first, then a
//! `[1m]` / `-1m` model id (1M), then 200k unless the usage already exceeds
//! it (1M). Usage over 1M with no configured window has no known window and
//! yields nothing.
//!
//! Codex rollouts (`event_msg` lines with `payload.type == "token_count"`)
//! carry `payload.info.last_token_usage` (`input_tokens` already includes
//! `cached_input_tokens`) and `payload.info.model_context_window`; `info` is
//! null on rate-limit-only events. The live context is the last request's
//! `input_tokens + output_tokens`.
//!
//! Only the file's tail is read (256 KiB, doubled up to 4 MiB while the tail
//! holds no usable line); the first line of a tail is usually cut and is
//! skipped. A file whose size and mtime did not change is never read again
//! ([`ContextCache`]).

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The default Claude window.
pub const CLAUDE_DEFAULT_WINDOW: u64 = 200_000;
/// The long-context Claude window.
pub const CLAUDE_LONG_WINDOW: u64 = 1_000_000;
const FIRST_TAIL_BYTES: u64 = 256 * 1024;
const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The agents whose session files carry token usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextAgent {
    Claude,
    Codex,
}

impl ContextAgent {
    pub fn from_agent(agent: crate::detect::Agent) -> Option<Self> {
        match agent {
            crate::detect::Agent::Claude => Some(Self::Claude),
            crate::detect::Agent::Codex => Some(Self::Codex),
            _ => None,
        }
    }

    /// The `[agents] context_window` key.
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// What a session file's last usable line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptUsage {
    /// Tokens in the context after the last request.
    pub used: u64,
    /// The model id, when the line names one.
    pub model: Option<String>,
    /// The window the file states (Codex), when it does.
    pub window: Option<u64>,
}

/// A pane agent's context use: tokens in use and the window size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextUsage {
    pub used: u64,
    pub window: u64,
}

impl ContextUsage {
    /// Whole percent of the window in use (0 for an empty window), capped at 100.
    pub fn percent(self) -> u8 {
        if self.window == 0 {
            return 0;
        }
        (self.used.saturating_mul(100) / self.window).min(100) as u8
    }
}

/// The usage with its window resolved (see the module docs); `None` when
/// the window is unknown.
pub fn resolve_usage(
    agent: ContextAgent,
    usage: &TranscriptUsage,
    overrides: &BTreeMap<String, u64>,
) -> Option<ContextUsage> {
    let configured = overrides
        .get(agent.key())
        .copied()
        .filter(|window| *window > 0);
    let window = match (configured, agent) {
        (Some(window), _) => window,
        (None, ContextAgent::Codex) => usage.window.filter(|window| *window > 0)?,
        (None, ContextAgent::Claude) => {
            let long_model = usage.model.as_deref().is_some_and(is_long_context_model);
            if long_model || usage.used > CLAUDE_DEFAULT_WINDOW {
                if usage.used > CLAUDE_LONG_WINDOW {
                    return None;
                }
                CLAUDE_LONG_WINDOW
            } else {
                CLAUDE_DEFAULT_WINDOW
            }
        }
    };
    Some(ContextUsage {
        used: usage.used,
        window,
    })
}

/// A model id that names the 1M-token window (`claude-sonnet-4-5[1m]`,
/// `…-1m`).
fn is_long_context_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("[1m]") || model.ends_with("-1m") || model.contains("-1m-")
}

/// The last usable line of a session file's tail; the first line is skipped
/// unless the tail is the whole file.
pub fn parse_tail(agent: ContextAgent, tail: &str, whole_file: bool) -> Option<TranscriptUsage> {
    let mut lines = tail.lines();
    if !whole_file {
        lines.next();
    }
    lines.rev().find_map(|line| match agent {
        ContextAgent::Claude => parse_claude_line(line),
        ContextAgent::Codex => parse_codex_line(line),
    })
}

/// One Claude transcript line: an assistant message with real usage.
pub fn parse_claude_line(line: &str) -> Option<TranscriptUsage> {
    // Cheap prefilter before the JSON parse.
    if !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = value.get("message")?;
    let model = message.get("model").and_then(|model| model.as_str());
    if model == Some("<synthetic>") {
        return None;
    }
    let usage = message.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(serde_json::Value::as_u64);
    let input = field("input_tokens")?;
    let used = input
        .saturating_add(field("cache_creation_input_tokens").unwrap_or(0))
        .saturating_add(field("cache_read_input_tokens").unwrap_or(0))
        .saturating_add(field("output_tokens").unwrap_or(0));
    if used == 0 {
        return None;
    }
    Some(TranscriptUsage {
        used,
        model: model.map(str::to_owned),
        window: None,
    })
}

/// One Codex rollout line: a `token_count` event with usage info.
pub fn parse_codex_line(line: &str) -> Option<TranscriptUsage> {
    if !line.contains("\"token_count\"") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let payload = value.get("payload")?;
    if payload.get("type")?.as_str()? != "token_count" {
        return None;
    }
    let info = payload.get("info")?;
    let last = info.get("last_token_usage")?;
    let field = |name: &str| last.get(name).and_then(serde_json::Value::as_u64);
    let used = field("input_tokens")?.saturating_add(field("output_tokens").unwrap_or(0));
    Some(TranscriptUsage {
        used,
        model: None,
        window: info
            .get("model_context_window")
            .and_then(serde_json::Value::as_u64),
    })
}

/// Reads the tail of `path` (growing it while no usable line is found) and
/// parses it. `None` when the file cannot be read or holds no usage.
pub fn read_usage(agent: ContextAgent, path: &Path) -> Option<TranscriptUsage> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut tail_bytes = FIRST_TAIL_BYTES;
    loop {
        let start = len.saturating_sub(tail_bytes);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.by_ref()
            .take(len - start)
            .read_to_end(&mut bytes)
            .ok()?;
        let tail = String::from_utf8_lossy(&bytes);
        if let Some(usage) = parse_tail(agent, &tail, start == 0) {
            return Some(usage);
        }
        if start == 0 || tail_bytes >= MAX_TAIL_BYTES {
            return None;
        }
        tail_bytes = (tail_bytes * 2).min(MAX_TAIL_BYTES);
    }
}

/// The session a pane agent runs, as the probe hands it to the reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSession {
    pub agent: ContextAgent,
    /// The native session id (Claude session, Codex thread).
    pub id: String,
    /// The persisted session record (Claude: the transcript lookup).
    pub session: crate::agent_resume::PersistedAgentSession,
}

#[derive(Debug, Clone)]
struct CachedFile {
    path: PathBuf,
    stamp: Option<(u64, SystemTime)>,
    usage: Option<TranscriptUsage>,
}

/// Session files by session id: the resolved path, the size and mtime last
/// read and what that read found.
#[derive(Debug, Default)]
pub struct ContextCache {
    entries: HashMap<(ContextAgent, String), CachedFile>,
    #[cfg(test)]
    pub(crate) reads: u32,
}

impl ContextCache {
    /// Drop the sessions not in `keep`.
    pub fn retain_sessions(&mut self, keep: &[(ContextAgent, String)]) {
        self.entries.retain(|key, _| keep.contains(key));
    }

    /// The session's usage: re-read only when its file's size or mtime
    /// changed since the last read.
    pub fn usage(
        &mut self,
        session: &ContextSession,
        locate: impl Fn(&ContextSession) -> Option<PathBuf>,
    ) -> Option<TranscriptUsage> {
        let key = (session.agent, session.id.clone());
        let cached_path = self
            .entries
            .get(&key)
            .map(|entry| entry.path.clone())
            .filter(|path| path.is_file());
        let path = match cached_path {
            Some(path) => path,
            None => locate(session)?,
        };
        let metadata = std::fs::metadata(&path).ok()?;
        let stamp = metadata
            .modified()
            .ok()
            .map(|modified| (metadata.len(), modified));
        if let Some(entry) = self.entries.get(&key) {
            if entry.path == path && stamp.is_some() && entry.stamp == stamp {
                return entry.usage.clone();
            }
        }
        #[cfg(test)]
        {
            self.reads += 1;
        }
        let usage = read_usage(session.agent, &path);
        self.entries.insert(
            key,
            CachedFile {
                path,
                stamp,
                usage: usage.clone(),
            },
        );
        usage
    }
}

/// Where a session's file lives: Claude through the native transcript
/// lookup, Codex by the thread id's day directories under
/// `codex_sessions_root`.
pub fn locate_session_file(
    session: &ContextSession,
    home: Option<&Path>,
    codex_sessions_root: Option<&Path>,
) -> Option<PathBuf> {
    match session.agent {
        ContextAgent::Claude => {
            let transcript =
                crate::agent_resume::native_transcript_locations_in(&session.session, home?)?;
            transcript.file.is_file().then_some(transcript.file)
        }
        ContextAgent::Codex => find_codex_rollout(codex_sessions_root?, &session.id),
    }
}

/// `sessions/YYYY/MM/DD/rollout-*-<id>.jsonl` for a version 7 thread id
/// (its timestamp names the day, a day either side for local time).
pub fn find_codex_rollout(sessions_root: &Path, id: &str) -> Option<PathBuf> {
    if !crate::agent_resume::is_safe_path_component(id) {
        return None;
    }
    let created_ms = crate::codex_sessions::uuid_v7_ms(id)?;
    let suffix = format!("-{id}.jsonl");
    crate::codex_sessions::day_dirs(sessions_root, created_ms, created_ms)
        .into_iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_line(model: &str, input: u64, created: u64, read: u64, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":{input},"cache_creation_input_tokens":{created},"cache_read_input_tokens":{read},"output_tokens":{output}}}}}}}"#
        )
    }

    fn codex_line(input: u64, cached: u64, output: u64, window: u64) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":999999,"output_tokens":5}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}},"model_context_window":{window}}}}}}}"#
        )
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "herdr-agent-context-{name}-{}-{}",
                std::process::id(),
                crate::codex_sessions::now_unix_ms()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn claude_usage_sums_input_cache_and_output_of_the_last_assistant_message() {
        let tail = [
            // The cut first line of a tail is ignored even when it parses.
            claude_line("claude-opus-5-5", 9, 9, 9, 9),
            claude_line("claude-opus-5-5", 2, 1_000, 150_000, 500),
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.into(),
            // Locally made messages carry no real usage.
            r#"{"type":"assistant","message":{"model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#.into(),
            r#"{"type":"system","subtype":"turn_duration"}"#.into(),
        ]
        .join("\n");
        let usage = parse_tail(ContextAgent::Claude, &tail, false).unwrap();
        assert_eq!(usage.used, 151_502);
        assert_eq!(usage.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(usage.window, None);
    }

    #[test]
    fn a_truncated_or_malformed_line_is_skipped() {
        let full = claude_line("claude-sonnet-5", 1, 2, 3, 4);
        let cut = &full[20..];
        let tail = format!("{full}\n{cut}\n{{not json");
        // The whole file: the first line counts; the broken ones are skipped.
        assert_eq!(
            parse_tail(ContextAgent::Claude, &tail, true).unwrap().used,
            10
        );
        // A tail: the (possibly cut) first line is dropped, nothing is left.
        assert_eq!(parse_tail(ContextAgent::Claude, &tail, false), None);
        assert_eq!(parse_tail(ContextAgent::Claude, "", true), None);
    }

    #[test]
    fn the_claude_window_follows_config_then_model_then_usage() {
        let none = BTreeMap::new();
        let usage = |model: &str, used: u64| TranscriptUsage {
            used,
            model: Some(model.into()),
            window: None,
        };
        let resolve = |usage: &TranscriptUsage, overrides: &BTreeMap<String, u64>| {
            resolve_usage(ContextAgent::Claude, usage, overrides).map(|usage| usage.window)
        };
        assert_eq!(
            resolve(&usage("claude-opus-5-5", 150_000), &none),
            Some(200_000)
        );
        assert_eq!(
            resolve(&usage("claude-sonnet-4-5[1m]", 150_000), &none),
            Some(1_000_000)
        );
        assert_eq!(resolve(&usage("claude-x-1m", 10), &none), Some(1_000_000));
        // Usage past 200k proves the long window.
        assert_eq!(
            resolve(&usage("claude-opus-5-5", 820_000), &none),
            Some(1_000_000)
        );
        // Past 1M with nothing configured: unknown.
        assert_eq!(resolve(&usage("claude-fable-5", 1_600_000), &none), None);
        let configured = BTreeMap::from([("claude".to_string(), 2_000_000)]);
        assert_eq!(
            resolve(&usage("claude-fable-5", 1_600_000), &configured),
            Some(2_000_000)
        );
        assert_eq!(
            resolve(&usage("claude-opus-5-5", 150_000), &configured),
            Some(2_000_000)
        );
        // A zero override is ignored.
        let zero = BTreeMap::from([("claude".to_string(), 0)]);
        assert_eq!(resolve(&usage("claude-opus-5-5", 1), &zero), Some(200_000));
    }

    #[test]
    fn codex_usage_is_the_last_requests_input_and_output_in_the_stated_window() {
        let tail = [
            String::new(),
            codex_line(100_000, 90_000, 10, 258_400),
            // Rate-limit-only events carry no info.
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{}}}"#
                .into(),
            codex_line(184_975, 184_576, 20, 258_400),
            r#"{"type":"response_item","payload":{"type":"message"}}"#.into(),
        ]
        .join("\n");
        let usage = parse_tail(ContextAgent::Codex, &tail, false).unwrap();
        // Cached input is part of input_tokens, never added again.
        assert_eq!(usage.used, 184_995);
        assert_eq!(usage.window, Some(258_400));
        let resolved = resolve_usage(ContextAgent::Codex, &usage, &BTreeMap::new()).unwrap();
        assert_eq!(resolved.percent(), 71);
        // No stated window and none configured: unknown.
        let bare = TranscriptUsage {
            used: 5,
            model: None,
            window: None,
        };
        assert_eq!(
            resolve_usage(ContextAgent::Codex, &bare, &BTreeMap::new()),
            None
        );
    }

    #[test]
    fn percent_is_whole_and_capped() {
        let usage = |used, window| ContextUsage { used, window };
        assert_eq!(usage(164_000, 200_000).percent(), 82);
        assert_eq!(usage(300_000, 200_000).percent(), 100);
        assert_eq!(usage(5, 0).percent(), 0);
    }

    #[test]
    fn the_tail_grows_until_a_usage_line_shows_and_the_cache_skips_unchanged_files() {
        let dir = TempDir::new("tail");
        let path = dir.0.join("session.jsonl");
        // The usage line sits before 300 KiB of other lines: the first
        // 256 KiB tail misses it, the doubled one finds it.
        let mut text = claude_line("claude-opus-5-5", 1, 2, 3, 4);
        text.push('\n');
        let filler = format!(
            "{{\"type\":\"attachment\",\"data\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        for _ in 0..300 {
            text.push_str(&filler);
        }
        std::fs::write(&path, &text).unwrap();
        assert_eq!(read_usage(ContextAgent::Claude, &path).unwrap().used, 10);

        let session = ContextSession {
            agent: ContextAgent::Claude,
            id: "id-1".into(),
            session: crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("id-1").unwrap(),
                transcript_path: None,
            },
        };
        let located = path.clone();
        let locate = move |_: &ContextSession| Some(located.clone());
        let mut cache = ContextCache::default();
        assert_eq!(cache.usage(&session, &locate).unwrap().used, 10);
        assert_eq!(cache.usage(&session, &locate).unwrap().used, 10);
        assert_eq!(cache.reads, 1, "an unchanged file is not read again");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(
            &mut file,
            format!("{}\n", claude_line("claude-opus-5-5", 1, 1, 1, 1)).as_bytes(),
        )
        .unwrap();
        drop(file);
        assert_eq!(cache.usage(&session, &locate).unwrap().used, 4);
        assert_eq!(cache.reads, 2, "a grown file is read again");
        cache.retain_sessions(&[]);
        assert_eq!(cache.usage(&session, &locate).unwrap().used, 4);
        assert_eq!(cache.reads, 3, "a forgotten session is read again");
    }

    #[test]
    fn codex_rollouts_are_found_by_the_thread_ids_day() {
        let dir = TempDir::new("codex");
        // 2026-10-04T08:00:00Z as a version 7 id.
        let ms: u64 = 1_791_100_800_000;
        let id = format!(
            "{:08x}-{:04x}-7000-8000-000000000001",
            ms >> 16,
            ms & 0xffff
        );
        let day = dir.0.join("2026").join("10").join("04");
        std::fs::create_dir_all(&day).unwrap();
        let file = day.join(format!("rollout-2026-10-04T11-00-00-{id}.jsonl"));
        std::fs::write(&file, codex_line(1, 0, 1, 10)).unwrap();
        std::fs::write(day.join("rollout-2026-10-04T11-00-00-other.jsonl"), "").unwrap();
        assert_eq!(find_codex_rollout(&dir.0, &id), Some(file));
        assert_eq!(find_codex_rollout(&dir.0, "not-a-v7-id"), None);
        assert_eq!(find_codex_rollout(&dir.0, "../escape"), None);
    }
}
