//! Fork: learn which native Codex thread a pane's Codex TUI is running from
//! Codex's own rollout files, for panes the Codex integration hook does not
//! report.
//!
//! Evidence (codex-cli 0.160.0): the TUI talks to a shared app-server
//! daemon, and the daemon (not the pane's process) owns the rollout files, so
//! a pane's process tree never holds the file open. Every thread gets
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<local time>-<thread id>.jsonl`
//! whose first line is a `session_meta` record carrying the thread `id`, the
//! `cwd` and the thread creation `timestamp`. The thread is created when the
//! TUI starts (or on `/new`), but the file only appears with the first
//! prompt. A thread therefore belongs to the Codex process that runs in the
//! same directory and started last before the thread was created.
//!
//! Subagent and guardian threads (`thread_source` other than `user`) and
//! `codex exec` threads (unless the pane runs `codex exec`) are ignored.
//! Limitations: a thread picked with `/resume` inside a running TUI keeps an
//! old creation time and is not followed, and a Codex started outside Herdr
//! in the same directory after the pane's Codex can be mistaken for it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

/// Threads created this long before a process start still count as its own:
/// process start times and thread timestamps come from different clocks.
const START_SLACK_MS: u64 = 1_000;
/// A TUI creates its first thread about 3 s after it starts (later while a
/// startup dialog is open). When several Codex processes share a directory,
/// a thread created later than this after the newest one started may be a
/// `/new` in any of them, so it is attributed to none.
const FIRST_THREAD_WINDOW_MS: u64 = 60_000;
/// How much of a rollout's first line is read; the `session_meta` record
/// carries the base instructions (about 22 KB in 0.160).
const MAX_META_LINE_BYTES: u64 = 1024 * 1024;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// At most this many day directories are listed per pass.
const MAX_DAY_DIRS: u64 = 31;

/// A Codex process running in a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexProcess<K> {
    pub key: K,
    pub start_ms: u64,
    /// Canonical working directory of the process.
    pub cwd: PathBuf,
    /// The pane runs `codex exec` rather than the TUI.
    pub exec: bool,
}

/// The identity fields of one rollout's `session_meta` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloutMeta {
    pub id: String,
    /// Canonical working directory the thread runs in.
    pub cwd: PathBuf,
    pub created_ms: u64,
    pub exec: bool,
}

/// Parsed first lines by rollout path; a rollout's complete first line never
/// changes. A file whose first line is still being written is not cached.
#[derive(Debug, Default)]
pub struct RolloutCache {
    entries: HashMap<PathBuf, Option<RolloutMeta>>,
}

impl RolloutCache {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The thread each process is running, keyed like `processes`.
pub fn resolve_threads<K: Clone + Eq + std::hash::Hash>(
    sessions_root: &Path,
    processes: &[CodexProcess<K>],
    now_ms: u64,
    cache: &mut RolloutCache,
) -> HashMap<K, String> {
    let Some(since_ms) = processes.iter().map(|process| process.start_ms).min() else {
        return HashMap::new();
    };
    let threads = scan_rollouts(sessions_root, since_ms, now_ms, cache);
    attribute_threads(processes, &threads)
}

/// Every user thread created at or after `since_ms` (less the slack), read
/// from the day directories that can hold it.
pub fn scan_rollouts(
    sessions_root: &Path,
    since_ms: u64,
    now_ms: u64,
    cache: &mut RolloutCache,
) -> Vec<RolloutMeta> {
    let floor_ms = since_ms.saturating_sub(START_SLACK_MS);
    let mut seen = HashMap::new();
    let mut threads = Vec::new();
    for day_dir in day_dirs(sessions_root, floor_ms, now_ms) {
        let Ok(entries) = std::fs::read_dir(&day_dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !is_rollout_file_name(&path) {
                continue;
            }
            // A thread's file is written after the thread exists.
            let modified_ms = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_ms);
            if modified_ms.is_some_and(|modified| modified < floor_ms) {
                continue;
            }
            let meta = match cache.entries.get(&path) {
                Some(meta) => Some(meta.clone()),
                None => read_rollout_meta(&path),
            };
            // `None`: the first line is not complete yet; read it again on
            // the next pass.
            let Some(meta) = meta else {
                continue;
            };
            if let Some(meta) = meta.as_ref().filter(|meta| meta.created_ms >= floor_ms) {
                threads.push(meta.clone());
            }
            seen.insert(path, meta);
        }
    }
    // Forget files that are gone or out of range.
    cache.entries = seen;
    threads
}

/// Assign each thread to the process in the same directory that started
/// last before it, then keep each process's newest thread (`/new` and
/// `/clear` start a new one). With several candidate processes, only a
/// thread created within [`FIRST_THREAD_WINDOW_MS`] of the newest one's
/// start is attributed (its first thread); a later one could be a `/new` in
/// any of them and is skipped rather than given to the wrong pane.
pub fn attribute_threads<K: Clone + Eq + std::hash::Hash>(
    processes: &[CodexProcess<K>],
    threads: &[RolloutMeta],
) -> HashMap<K, String> {
    let mut newest: HashMap<K, (u64, &str)> = HashMap::new();
    for thread in threads {
        let mut candidates = processes
            .iter()
            .filter(|process| process.cwd == thread.cwd)
            .filter(|process| process.start_ms <= thread.created_ms.saturating_add(START_SLACK_MS))
            .peekable();
        let Some(first) = candidates.next() else {
            continue;
        };
        let shared = candidates.peek().is_some();
        let owner = std::iter::once(first)
            .chain(candidates)
            .max_by_key(|process| process.start_ms)
            .unwrap_or(first);
        if shared && thread.created_ms > owner.start_ms.saturating_add(FIRST_THREAD_WINDOW_MS) {
            continue;
        }
        if thread.exec && !owner.exec {
            continue;
        }
        let entry = newest
            .entry(owner.key.clone())
            .or_insert((thread.created_ms, thread.id.as_str()));
        if (thread.created_ms, thread.id.as_str()) > *entry {
            *entry = (thread.created_ms, thread.id.as_str());
        }
    }
    newest
        .into_iter()
        .map(|(key, (_, id))| (key, id.to_string()))
        .collect()
}

/// Read the `session_meta` record on a rollout's first line: `None` while
/// that line is not complete yet (Codex is still writing it, or the file
/// cannot be read now), `Some(None)` when it is not a user thread's record.
pub fn read_rollout_meta(path: &Path) -> Option<Option<RolloutMeta>> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    let read = BufReader::new(file.take(MAX_META_LINE_BYTES))
        .read_line(&mut line)
        .ok()?;
    if !line.ends_with('\n') && (read as u64) < MAX_META_LINE_BYTES {
        return None;
    }
    Some(parse_rollout_meta(&line))
}

pub fn parse_rollout_meta(line: &str) -> Option<RolloutMeta> {
    let record: serde_json::Value = serde_json::from_str(line.trim_end()).ok()?;
    if record.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = record.get("payload")?;
    let id = payload.get("id").and_then(serde_json::Value::as_str)?;
    crate::agent_resume::AgentSessionRef::id(id)?;
    match payload
        .get("thread_source")
        .and_then(serde_json::Value::as_str)
    {
        Some("user") => {}
        Some(_) => return None,
        // Older rollouts: subagent threads carry an object source.
        None if payload
            .get("source")
            .is_some_and(|source| !source.is_string()) =>
        {
            return None;
        }
        None => {}
    }
    let cwd = payload.get("cwd").and_then(serde_json::Value::as_str)?;
    let created_ms = payload
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_rfc3339_ms)
        .or_else(|| uuid_v7_ms(id))?;
    let exec = payload
        .get("originator")
        .and_then(serde_json::Value::as_str)
        == Some("codex_exec")
        || payload.get("source").and_then(serde_json::Value::as_str) == Some("exec");
    Some(RolloutMeta {
        id: id.to_string(),
        cwd: canonical_dir(Path::new(cwd)),
        created_ms,
        exec,
    })
}

/// The directory as the filesystem names it (`/var` is `/private/var` on
/// macOS), or unchanged when it no longer exists.
pub fn canonical_dir(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `argv` (without the program) starts `codex exec`.
pub fn is_exec_invocation(args: &[String]) -> bool {
    args.iter()
        .find(|arg| !arg.starts_with('-'))
        .is_some_and(|arg| arg == "exec" || arg == "e")
}

fn is_rollout_file_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

/// `sessions/YYYY/MM/DD` for every UTC day from a day before `floor_ms` to a
/// day after `now_ms`: the directories are named in local time, which is at
/// most a day away from UTC.
fn day_dirs(sessions_root: &Path, floor_ms: u64, now_ms: u64) -> Vec<PathBuf> {
    let first = (floor_ms / DAY_MS).saturating_sub(1);
    let last = now_ms.max(floor_ms) / DAY_MS + 1;
    let first = first.max(last.saturating_sub(MAX_DAY_DIRS - 1));
    (first..=last)
        .map(|day| {
            let (year, month, day) = civil_from_days(day as i64);
            sessions_root
                .join(format!("{year:04}"))
                .join(format!("{month:02}"))
                .join(format!("{day:02}"))
        })
        .collect()
}

fn system_time_ms(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

pub fn now_unix_ms() -> u64 {
    system_time_ms(std::time::SystemTime::now()).unwrap_or(0)
}

/// The millisecond timestamp in a version 7 UUID.
fn uuid_v7_ms(id: &str) -> Option<u64> {
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || hex.as_bytes().get(12) != Some(&b'7') {
        return None;
    }
    u64::from_str_radix(hex.get(..12)?, 16).ok()
}

/// `YYYY-MM-DDTHH:MM:SS[.fff…](Z|±HH:MM)` to Unix milliseconds.
pub fn parse_rfc3339_ms(value: &str) -> Option<u64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[13] != b':' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[16] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = value.get(range)?;
        text.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse().ok())
            .flatten()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let mut rest = value.get(19..)?;
    let mut millis = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let mut scaled = 0i64;
        for (index, byte) in fraction.bytes().take(3).enumerate() {
            if index < digits {
                scaled = scaled * 10 + i64::from(byte - b'0');
            }
        }
        for _ in digits.min(3)..3 {
            scaled *= 10;
        }
        millis = scaled;
        rest = fraction.get(digits..)?;
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        offset if offset.len() == 6 && offset.as_bytes()[3] == b':' => {
            let sign = match offset.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = offset.get(1..3)?.parse().ok()?;
            let minutes: i64 = offset.get(4..6)?.parse().ok()?;
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    u64::try_from(seconds * 1_000 + millis).ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-codex-sessions-{name}-{}-{}",
                std::process::id(),
                now_unix_ms()
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const T0: u64 = 1_791_038_040_000; // 2026-10-03T14:34:00Z

    fn meta_line(id: &str, cwd: &Path, timestamp: &str, extra: &str) -> String {
        format!(
            r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","timestamp":"{timestamp}","cwd":"{}","originator":"codex-tui","cli_version":"0.160.0","source":"cli"{extra},"base_instructions":{{"text":"long"}}}}}}"#,
            cwd.display()
        )
    }

    fn write_rollout(root: &Path, day: &str, id: &str, line: &str) -> PathBuf {
        let dir = root.join(day);
        std::fs::create_dir_all(&dir).expect("day dir");
        let path = dir.join(format!("rollout-2026-10-03T17-34-02-{id}.jsonl"));
        std::fs::write(&path, format!("{line}\n{{\"type\":\"event_msg\"}}\n")).expect("rollout");
        path
    }

    fn process(key: &str, start_ms: u64, cwd: &Path) -> CodexProcess<String> {
        CodexProcess {
            key: key.into(),
            start_ms,
            cwd: canonical_dir(cwd),
            exec: false,
        }
    }

    fn thread(id: &str, created_ms: u64, cwd: &Path) -> RolloutMeta {
        RolloutMeta {
            id: id.into(),
            cwd: canonical_dir(cwd),
            created_ms,
            exec: false,
        }
    }

    #[test]
    fn rfc3339_timestamps_parse_to_unix_millis() {
        assert_eq!(parse_rfc3339_ms("2026-10-03T14:34:00Z"), Some(T0));
        assert_eq!(
            parse_rfc3339_ms("2026-10-03T14:34:02.976Z"),
            Some(T0 + 2_976)
        );
        assert_eq!(parse_rfc3339_ms("2026-10-03T14:34:02.9Z"), Some(T0 + 2_900));
        assert_eq!(
            parse_rfc3339_ms("2026-10-03T17:34:02.123456+03:00"),
            Some(T0 + 2_123)
        );
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-10-03"), None);
        assert_eq!(parse_rfc3339_ms("2026-13-03T14:34:00Z"), None);
        assert_eq!(parse_rfc3339_ms("2026-10-03T14:34:00"), None);
    }

    #[test]
    fn day_counts_round_trip_through_civil_dates() {
        for days in [0, 59, 60, 365, 11_016, 20_729, 20_730] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days((T0 / DAY_MS) as i64), (2026, 10, 3));
    }

    #[test]
    fn uuid_v7_ids_carry_their_creation_time() {
        assert_eq!(
            uuid_v7_ms("01a1022f-b35b-7c12-81ea-bafbc3b8c3c1"),
            Some(0x01a1_022f_b35b)
        );
        assert_eq!(uuid_v7_ms("0d6a6c1e-1111-4abc-8def-0123456789ab"), None);
    }

    #[test]
    fn session_meta_lines_yield_user_threads_only() {
        let cwd = Path::new("/nonexistent/herdr-codex-demo");
        let id = "01a1022f-b35b-7c12-81ea-bafbc3b8c3c1";
        let meta = parse_rollout_meta(&meta_line(
            id,
            cwd,
            "2026-10-03T14:34:02.976Z",
            r#","thread_source":"user""#,
        ))
        .expect("user thread");
        assert_eq!(meta.id, id);
        assert_eq!(meta.cwd, cwd);
        assert_eq!(meta.created_ms, T0 + 2_976);
        assert!(!meta.exec);

        for extra in [
            r#","thread_source":"subagent""#,
            r#","thread_source":"guardian_review""#,
        ] {
            assert_eq!(
                parse_rollout_meta(&meta_line(id, cwd, "2026-10-03T14:34:02Z", extra)),
                None
            );
        }
        let old_subagent = meta_line(id, cwd, "2026-10-03T14:34:02Z", "")
            .replace(r#""source":"cli""#, r#""source":{"subagent":"review"}"#);
        assert_eq!(parse_rollout_meta(&old_subagent), None);
        let exec =
            meta_line(id, cwd, "2026-10-03T14:34:02Z", "").replace("codex-tui", "codex_exec");
        assert!(parse_rollout_meta(&exec).expect("exec thread").exec);
        assert_eq!(
            parse_rollout_meta(r#"{"type":"event_msg","payload":{}}"#),
            None
        );
        assert_eq!(parse_rollout_meta("not json"), None);
    }

    #[test]
    fn a_thread_belongs_to_the_last_process_started_before_it_in_its_directory() {
        let a = Path::new("/nonexistent/herdr-codex-a");
        let b = Path::new("/nonexistent/herdr-codex-b");
        let processes = [
            process("first", T0, a),
            process("second", T0 + 60_000, a),
            process("other-dir", T0, b),
        ];
        let threads = [
            thread("01a1022f-0000-7000-8000-000000000001", T0 + 3_000, a),
            thread("01a1022f-0000-7000-8000-000000000002", T0 + 62_000, a),
            // Created before any process: someone else's thread.
            thread("01a1022f-0000-7000-8000-000000000003", T0 - 60_000, b),
        ];
        let resolved = attribute_threads(&processes, &threads);
        assert_eq!(
            resolved.get("first").map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000001")
        );
        assert_eq!(
            resolved.get("second").map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000002")
        );
        assert_eq!(resolved.get("other-dir"), None);
    }

    #[test]
    fn a_later_thread_shared_by_two_processes_in_one_directory_goes_to_neither() {
        let a = Path::new("/nonexistent/herdr-codex-a");
        let processes = [process("first", T0, a), process("second", T0 + 60_000, a)];
        let first_thread = thread("01a1022f-0000-7000-8000-000000000001", T0 + 3_000, a);
        let second_thread = thread("01a1022f-0000-7000-8000-000000000002", T0 + 62_000, a);
        // `/new` in the first TUI after the second one started: either
        // could have made it, so it is nobody's.
        let new_in_first = thread("01a1022f-0000-7000-8000-000000000003", T0 + 160_000, a);
        let resolved = attribute_threads(&processes, &[first_thread, second_thread, new_in_first]);
        assert_eq!(
            resolved.get("first").map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000001")
        );
        assert_eq!(
            resolved.get("second").map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000002")
        );
    }

    #[test]
    fn a_first_line_still_being_written_is_read_again_later() {
        let home = TempDir::new("partial");
        let cwd = home.0.join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let root = home.0.join("sessions");
        let id = "01a1022f-b35b-7c12-81ea-bafbc3b8c3c1";
        let line = meta_line(
            id,
            &cwd,
            "2026-10-03T14:34:02.976Z",
            r#","thread_source":"user""#,
        );
        let dir = root.join("2026/10/03");
        std::fs::create_dir_all(&dir).expect("day dir");
        let path = dir.join(format!("rollout-2026-10-03T17-34-02-{id}.jsonl"));
        let processes = [process("pane", T0, &cwd)];
        let mut cache = RolloutCache::default();
        for partial in [String::new(), line[..line.len() / 2].to_string()] {
            std::fs::write(&path, partial).expect("partial rollout");
            assert!(resolve_threads(&root, &processes, T0 + 60_000, &mut cache).is_empty());
            assert_eq!(cache.len(), 0);
        }
        std::fs::write(&path, format!("{line}\n")).expect("rollout");
        let resolved = resolve_threads(&root, &processes, T0 + 60_000, &mut cache);
        assert_eq!(resolved.get("pane").map(String::as_str), Some(id));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn a_new_thread_in_the_same_tui_replaces_the_first() {
        let a = Path::new("/nonexistent/herdr-codex-a");
        let processes = [process("pane", T0, a)];
        let threads = [
            thread("01a1022f-0000-7000-8000-000000000001", T0 + 3_000, a),
            thread("01a1022f-0000-7000-8000-000000000009", T0 + 600_000, a),
        ];
        assert_eq!(
            attribute_threads(&processes, &threads)
                .get("pane")
                .map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000009")
        );
    }

    #[test]
    fn exec_threads_only_count_for_exec_processes() {
        let a = Path::new("/nonexistent/herdr-codex-a");
        let mut exec_thread = thread("01a1022f-0000-7000-8000-000000000005", T0 + 9_000, a);
        exec_thread.exec = true;
        let tui_thread = thread("01a1022f-0000-7000-8000-000000000004", T0 + 3_000, a);
        let threads = [tui_thread, exec_thread];
        let tui = [process("pane", T0, a)];
        assert_eq!(
            attribute_threads(&tui, &threads)
                .get("pane")
                .map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000004")
        );
        let mut exec = process("pane", T0, a);
        exec.exec = true;
        assert_eq!(
            attribute_threads(&[exec], &threads)
                .get("pane")
                .map(String::as_str),
            Some("01a1022f-0000-7000-8000-000000000005")
        );
    }

    #[test]
    fn exec_invocations_are_recognised_past_global_flags() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert!(is_exec_invocation(&args(&["exec", "hi"])));
        assert!(is_exec_invocation(&args(&["--yolo", "e", "hi"])));
        assert!(!is_exec_invocation(&args(&[])));
        assert!(!is_exec_invocation(&args(&["resume", "abc"])));
    }

    #[test]
    fn scanning_reads_rollouts_from_the_day_directories_and_caches_them() {
        let home = TempDir::new("scan");
        let cwd = home.0.join("project");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let root = home.0.join("sessions");
        let id = "01a1022f-b35b-7c12-81ea-bafbc3b8c3c1";
        write_rollout(
            &root,
            "2026/10/03",
            id,
            &meta_line(
                id,
                &cwd,
                "2026-10-03T14:34:02.976Z",
                r#","thread_source":"user""#,
            ),
        );
        // Stale thread from before the process started.
        let old = "01a10000-0000-7000-8000-000000000001";
        write_rollout(
            &root,
            "2026/10/03",
            old,
            &meta_line(
                old,
                &cwd,
                "2026-10-03T10:00:00Z",
                r#","thread_source":"user""#,
            ),
        );
        // Not a rollout.
        std::fs::write(root.join("2026/10/03/notes.txt"), "x").expect("noise");

        let mut cache = RolloutCache::default();
        let processes = [process("pane", T0, &cwd)];
        let resolved = resolve_threads(&root, &processes, T0 + 60_000, &mut cache);
        assert_eq!(resolved.get("pane").map(String::as_str), Some(id));
        assert_eq!(cache.len(), 2);

        // A day directory far from the process start is never listed.
        let threads = scan_rollouts(&root, T0 + 10 * DAY_MS, T0 + 10 * DAY_MS, &mut cache);
        assert!(threads.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn day_directories_cover_a_day_either_side_and_stay_bounded() {
        let root = Path::new("/sessions");
        let dirs = day_dirs(root, T0, T0);
        assert_eq!(
            dirs,
            vec![
                root.join("2026/10/02"),
                root.join("2026/10/03"),
                root.join("2026/10/04"),
            ]
        );
        assert_eq!(
            day_dirs(root, T0 - 400 * DAY_MS, T0).len() as u64,
            MAX_DAY_DIRS
        );
    }
}
