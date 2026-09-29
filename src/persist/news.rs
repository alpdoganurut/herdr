//! The AI news desk's server-side record (fork).
//!
//! `news.json` next to `session.json` keeps what the server must remember
//! across restarts: the next scheduled run, the News tab and pane, a run in
//! flight (so a restarted server keeps watching it), the failure counter and
//! the notification ledger (today's count, the pending notifications, the
//! failure alert flag).
//! Run history is not copied here: it is read back from the runner's
//! `<home>/runs/index.jsonl`, the newest [`MAX_HISTORY`] lines.
//!
//! Plain path operations, no application state, so everything is testable
//! with temporary directories. Writes go through a temporary file and a
//! rename; a missing file is the default record; an unreadable or corrupt
//! one is the default record with a warning, and a corrupt file is moved
//! aside so the next save does not destroy it. A run entry this build cannot
//! decode is dropped on its own.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::schema::{NewsEditionInfo, NewsNotifyInfo, NewsRunRecord};

/// File name inside the session data directory.
pub const FILE_NAME: &str = "news.json";
/// Where a corrupt file is moved before it is replaced.
const CORRUPT_FILE_NAME: &str = "news.corrupt.json";
/// The news home's name inside the session data directory.
pub const HOME_DIR_NAME: &str = "news";
/// The runner's run log, relative to the home.
pub const INDEX_FILE: &str = "runs/index.jsonl";
/// The runner's editions index, relative to the home.
pub const EDITIONS_INDEX_FILE: &str = "editions/index.json";
/// The published page (a copy of the latest edition), relative to the home.
pub const PAGE_FILE: &str = "page.json";
/// The most run records read back.
pub const MAX_HISTORY: usize = 50;
const FILE_VERSION: u32 = 1;

/// The record for the active session: next to its `session.json`.
pub fn store_path() -> PathBuf {
    crate::session::data_dir().join(FILE_NAME)
}

/// The news home for the active session.
pub fn home_dir() -> PathBuf {
    crate::session::data_dir().join(HOME_DIR_NAME)
}

/// The runner's run log inside `home`.
pub fn index_path(home: &Path) -> PathBuf {
    home.join(INDEX_FILE)
}

/// The editions index inside `home`.
pub fn editions_index_path(home: &Path) -> PathBuf {
    home.join(EDITIONS_INDEX_FILE)
}

/// The published page inside `home`.
pub fn page_path(home: &Path) -> PathBuf {
    home.join(PAGE_FILE)
}

/// `YYYY-MM-DDTHH:MM:SS` (any suffix; assumed UTC, which is what the
/// runner writes) to seconds since the epoch.
pub fn iso_to_unix(iso: &str) -> Option<u64> {
    let bytes = iso.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

/// The editions the runner indexed, oldest first. A missing or unreadable
/// index is an empty list; entries this build cannot decode are skipped.
pub fn read_editions(home: &Path) -> Vec<NewsEditionInfo> {
    let content = match fs::read_to_string(editions_index_path(home)) {
        Ok(content) => content,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(
                    event = "persist.news.editions",
                    outcome = "read_error",
                    err = %err,
                    "failed to read the news editions index"
                );
            }
            return Vec::new();
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                event = "persist.news.editions",
                outcome = "parse_error",
                err = %err,
                "the news editions index is not valid JSON"
            );
            return Vec::new();
        }
    };
    value
        .get("editions")
        .and_then(|editions| editions.as_array())
        .map(|editions| {
            editions
                .iter()
                .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// A run in flight, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedNewsRun {
    /// Seconds since the Unix epoch.
    pub started_at: u64,
    /// The same instant as the runner writes it (ISO 8601 UTC), for the
    /// index comparison.
    pub started: String,
    /// `manual` or `scheduled`.
    pub trigger: String,
    /// Byte length of `runs/index.jsonl` when the run started.
    #[serde(default)]
    pub index_len: u64,
}

/// A notification waiting for a client shell or the end of quiet hours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingNewsNotify {
    /// `run` (a run changed the page) or `failures` (three runs failed).
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `urgency: high`.
    #[serde(default)]
    pub high: bool,
    /// Not before this time (seconds since the epoch): the end of quiet
    /// hours; zero for now.
    #[serde(default)]
    pub deliver_after: u64,
    #[serde(default)]
    pub queued_at: u64,
}

/// The notification ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewsNotifyRecord {
    /// The local day (`YYYY-MM-DD`) the counters are for.
    pub day: String,
    /// Run notifications allowed that day.
    pub delivered: u8,
    /// A high-urgency notification already went past the day's cap.
    pub high_extra_used: bool,
    /// The alert for the current failure streak was queued.
    pub failure_alerted: bool,
    /// Waiting for a client shell or the end of quiet hours; at most one per
    /// kind, the newest replacing the older.
    pub pending: Vec<PendingNewsNotify>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewsRecord {
    /// The next scheduled run, seconds since the Unix epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<PersistedNewsRun>,
    pub consecutive_failures: u32,
    pub notify: NewsNotifyRecord,
}

#[derive(Serialize)]
struct StoreFileOut<'a> {
    version: u32,
    #[serde(flatten)]
    record: &'a NewsRecord,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StoreFileIn {
    next_run_at: Option<u64>,
    tab_id: Option<String>,
    pane_id: Option<String>,
    run: Option<serde_json::Value>,
    consecutive_failures: u32,
    notify: Option<serde_json::Value>,
}

/// Load the record. Never fails: a missing, unreadable or corrupt file is
/// the default record (a corrupt one is moved aside first).
pub fn load(path: &Path) -> NewsRecord {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return NewsRecord::default(),
        Err(err) => {
            tracing::warn!(
                event = "persist.news.load",
                outcome = "read_error",
                path = %path.display(),
                err = %err,
                "failed to read the news record"
            );
            return NewsRecord::default();
        }
    };
    let file: StoreFileIn = match serde_json::from_str(&content) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(
                event = "persist.news.load",
                outcome = "parse_error",
                path = %path.display(),
                err = %err,
                "news record is corrupt; starting from the defaults"
            );
            let aside = path.with_file_name(CORRUPT_FILE_NAME);
            if let Err(err) = fs::rename(path, &aside) {
                tracing::warn!(
                    event = "persist.news.load",
                    outcome = "move_aside_failed",
                    path = %path.display(),
                    err = %err,
                    "failed to move the corrupt news record aside"
                );
            }
            return NewsRecord::default();
        }
    };
    let run = file.run.and_then(|value| {
        serde_json::from_value::<PersistedNewsRun>(value)
            .map_err(|err| {
                tracing::warn!(
                    event = "persist.news.load",
                    outcome = "run_skipped",
                    path = %path.display(),
                    err = %err,
                    "dropping a news run entry this build cannot read"
                )
            })
            .ok()
    });
    let notify = file
        .notify
        .and_then(|value| {
            serde_json::from_value::<NewsNotifyRecord>(value)
                .map_err(|err| {
                    tracing::warn!(
                        event = "persist.news.load",
                        outcome = "notify_skipped",
                        path = %path.display(),
                        err = %err,
                        "dropping a news notification ledger this build cannot read"
                    )
                })
                .ok()
        })
        .unwrap_or_default();
    NewsRecord {
        next_run_at: file.next_run_at,
        tab_id: file.tab_id,
        pane_id: file.pane_id,
        run,
        consecutive_failures: file.consecutive_failures,
        notify,
    }
}

/// Write the record atomically (a temporary file in the same directory,
/// then a rename).
pub fn save(path: &Path, record: &NewsRecord) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&StoreFileOut {
        version: FILE_VERSION,
        record,
    })?;
    let tmp = path.with_file_name(format!(".{FILE_NAME}.tmp-{}", std::process::id()));
    let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// One `runs/index.jsonl` line as the runner writes it (`decision` is the
/// editor's `{changed, notify, summary}` object on an `ok` run).
pub fn record_from_index_line(line: &str) -> Option<NewsRunRecord> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let object = value.as_object()?;
    let string = |key: &str| object.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let decision = object.get("decision").and_then(|d| d.as_object());
    Some(NewsRunRecord {
        started: string("started")?,
        ended: string("ended"),
        trigger: string("trigger").unwrap_or_else(|| "manual".into()),
        run: string("run"),
        outcome: string("outcome")?,
        seconds: object.get("seconds").and_then(|v| v.as_u64()),
        cost_usd: object
            .get("cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        turns: object
            .get("turns")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        edition: object
            .get("edition")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok()),
        changed: decision
            .and_then(|d| d.get("changed"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        summary: decision
            .and_then(|d| d.get("summary"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        notify: decision
            .and_then(|d| d.get("notify"))
            .and_then(|v| serde_json::from_value::<NewsNotifyInfo>(v.clone()).ok())
            .filter(|notify| !notify.title.trim().is_empty()),
        errors: object
            .get("errors")
            .and_then(|v| v.as_array())
            .map(|errors| {
                errors
                    .iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The newest `cap` decodable records of `runs/index.jsonl`, newest first.
/// A missing log is an empty list; undecodable lines are skipped.
pub fn read_history(home: &Path, cap: usize) -> Vec<NewsRunRecord> {
    let content = match fs::read_to_string(index_path(home)) {
        Ok(content) => content,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                tracing::warn!(
                    event = "persist.news.history",
                    outcome = "read_error",
                    err = %err,
                    "failed to read the news run log"
                );
            }
            return Vec::new();
        }
    };
    content
        .lines()
        .rev()
        .filter_map(record_from_index_line)
        .take(cap)
        .collect()
}

/// Every record appended to `runs/index.jsonl` after its first `after`
/// bytes, oldest first. Used to spot the run in flight finishing.
pub fn read_index_after(home: &Path, after: u64) -> io::Result<Vec<NewsRunRecord>> {
    let content = match fs::read(index_path(home)) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let after = usize::try_from(after).unwrap_or(usize::MAX);
    let tail = content.get(after..).unwrap_or(&content[..]);
    Ok(String::from_utf8_lossy(tail)
        .lines()
        .filter_map(record_from_index_line)
        .collect())
}

/// The byte length of `runs/index.jsonl` (zero when missing).
pub fn index_len(home: &Path) -> u64 {
    fs::metadata(index_path(home))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// Append a record the server made itself (a watchdog timeout, a run that
/// never started) so `herdr news log` and the history show it. Tagged with
/// `recorded_by: herdr`; the runner never reads the log.
pub fn append_index_record(home: &Path, record: &NewsRunRecord) -> io::Result<()> {
    use std::io::Write;
    let path = index_path(home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut value = serde_json::to_value(record)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("recorded_by".into(), "herdr".into());
        if record.summary.is_some() || record.changed || record.notify.is_some() {
            object.insert(
                "decision".into(),
                serde_json::json!({
                    "changed": record.changed,
                    "summary": record.summary,
                    "notify": record.notify,
                }),
            );
        }
        object.remove("changed");
        object.remove("summary");
        object.remove("notify");
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(value.to_string().as_bytes())?;
    file.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("herdr-news-persist-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const RUNNER_LINE: &str = r#"{"started": "2026-09-29T11:05:07+00:00", "trigger": "manual", "run": "20260929-140507", "outcome": "ok", "cost_usd": 0.9123, "turns": 31, "edition": 3, "errors": [], "decision": {"changed": true, "notify": null, "summary": "Two new model launches."}, "ended": "2026-09-29T11:10:02+00:00", "seconds": 295}"#;

    fn record() -> NewsRecord {
        NewsRecord {
            next_run_at: Some(1_800_000_000),
            tab_id: Some("w_1:t_3".into()),
            pane_id: Some("w_1:p_4".into()),
            run: Some(PersistedNewsRun {
                started_at: 1_799_990_000,
                started: "2026-09-29T11:05:07+00:00".into(),
                trigger: "scheduled".into(),
                index_len: 1234,
            }),
            consecutive_failures: 2,
            notify: NewsNotifyRecord {
                day: "2026-09-29".into(),
                delivered: 1,
                high_extra_used: false,
                failure_alerted: true,
                pending: vec![PendingNewsNotify {
                    kind: "run".into(),
                    title: "News: Sonnet 5.5".into(),
                    body: Some("Out now.".into()),
                    high: true,
                    deliver_after: 1_800_000_600,
                    queued_at: 1_799_999_000,
                }],
            },
        }
    }

    #[test]
    fn missing_file_is_the_default_record() {
        let dir = TempDir::new("missing");
        assert_eq!(load(&dir.file()), NewsRecord::default());
    }

    #[test]
    fn record_round_trips_with_a_version() {
        let dir = TempDir::new("roundtrip");
        save(&dir.file(), &record()).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.file()).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["run"]["trigger"], "scheduled");
        assert_eq!(json["notify"]["pending"][0]["kind"], "run");
        assert_eq!(load(&dir.file()), record());

        save(&dir.file(), &NewsRecord::default()).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.file()).unwrap()).unwrap();
        assert!(json.get("run").is_none(), "absent fields are omitted");
        assert_eq!(load(&dir.file()), NewsRecord::default());
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_an_unreadable_run_is_dropped() {
        let dir = TempDir::new("corrupt");
        fs::write(dir.file(), "{ not json").unwrap();
        assert_eq!(load(&dir.file()), NewsRecord::default());
        assert!(!dir.file().exists());
        assert_eq!(
            fs::read_to_string(dir.0.join(CORRUPT_FILE_NAME)).unwrap(),
            "{ not json"
        );

        fs::write(
            dir.file(),
            r#"{"version": 1, "consecutive_failures": 1, "run": {"started_at": "soon"}, "future": 1}"#,
        )
        .unwrap();
        let loaded = load(&dir.file());
        assert_eq!(loaded.consecutive_failures, 1);
        assert!(loaded.run.is_none());
        assert!(dir.file().exists(), "a tolerable file stays in place");

        fs::write(
            dir.file(),
            r#"{"version": 1, "notify": {"day": "2026-09-29", "delivered": 2, "pending": [{"kind": "run", "title": "t"}], "later": true}}"#,
        )
        .unwrap();
        let loaded = load(&dir.file());
        assert_eq!(loaded.notify.delivered, 2);
        assert_eq!(loaded.notify.pending.len(), 1);
        assert_eq!(loaded.notify.pending[0].deliver_after, 0);
        assert!(!loaded.notify.pending[0].high);
        fs::write(dir.file(), r#"{"version": 1, "notify": 7}"#).unwrap();
        assert_eq!(load(&dir.file()).notify, NewsNotifyRecord::default());
    }

    #[test]
    fn runner_index_lines_decode_into_records() {
        let record = record_from_index_line(RUNNER_LINE).unwrap();
        assert_eq!(record.started, "2026-09-29T11:05:07+00:00");
        assert_eq!(record.ended.as_deref(), Some("2026-09-29T11:10:02+00:00"));
        assert_eq!(record.outcome, "ok");
        assert_eq!(record.trigger, "manual");
        assert_eq!(record.run.as_deref(), Some("20260929-140507"));
        assert_eq!(record.seconds, Some(295));
        assert_eq!(record.cost_usd, 0.9123);
        assert_eq!(record.turns, 31);
        assert_eq!(record.edition, Some(3));
        assert!(record.changed);
        assert_eq!(record.summary.as_deref(), Some("Two new model launches."));
        assert!(record.succeeded());

        let invalid = record_from_index_line(
            r#"{"started":"2026-09-29T12:00:00+00:00","trigger":"scheduled","outcome":"invalid","errors":["lead text ends in an ellipsis"],"edition":null}"#,
        )
        .unwrap();
        assert_eq!(invalid.outcome, "invalid");
        assert_eq!(invalid.edition, None);
        assert!(!invalid.changed);
        assert_eq!(invalid.errors, ["lead text ends in an ellipsis"]);
        assert!(!invalid.succeeded());

        assert!(record_from_index_line("").is_none());
        assert!(record_from_index_line("not json").is_none());
        assert!(record_from_index_line(r#"{"outcome":"ok"}"#).is_none());
    }

    #[test]
    fn iso_timestamps_convert_to_unix_seconds() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            iso_to_unix("2026-09-21T14:13:20+00:00"),
            Some(1_790_000_000)
        );
        assert_eq!(iso_to_unix("2027-01-15T08:00:00Z"), Some(1_800_000_000));
        assert_eq!(iso_to_unix("2024-02-29T23:59:59"), Some(1_709_251_199));
        assert_eq!(iso_to_unix("soon"), None);
        assert_eq!(iso_to_unix("2026-13-01T00:00:00"), None);
    }

    #[test]
    fn the_notify_request_rides_along_in_the_decision() {
        let line = r#"{"started": "2026-09-29T06:09:22+00:00", "trigger": "manual", "outcome": "ok", "edition": 1, "decision": {"changed": true, "notify": {"title": "Sonnet 5.5", "body": "Out now.", "urgency": "high"}, "summary": "One launch."}}"#;
        let record = record_from_index_line(line).unwrap();
        let notify = record.notify.as_ref().expect("notify decoded");
        assert_eq!(notify.title, "Sonnet 5.5");
        assert_eq!(notify.body.as_deref(), Some("Out now."));
        assert_eq!(notify.urgency, "high");

        let blank = record_from_index_line(
            r#"{"started":"2026-09-29T06:09:22+00:00","outcome":"ok","decision":{"changed":true,"notify":{"title":"  "}}}"#,
        )
        .unwrap();
        assert!(blank.notify.is_none(), "a blank title is no request");
        let null = record_from_index_line(
            r#"{"started":"2026-09-29T06:09:22+00:00","outcome":"ok","decision":{"changed":true,"notify":null}}"#,
        )
        .unwrap();
        assert!(null.notify.is_none());

        let dir = TempDir::new("notify-roundtrip");
        let home = dir.0.join("news");
        append_index_record(&home, &record).unwrap();
        let back = read_history(&home, 1);
        assert_eq!(back[0].notify, record.notify);
        assert_eq!(back[0].summary.as_deref(), Some("One launch."));
    }

    #[test]
    fn editions_index_reads_oldest_first_and_tolerates_junk() {
        let dir = TempDir::new("editions");
        let home = dir.0.join("news");
        assert!(read_editions(&home).is_empty());
        fs::create_dir_all(home.join("editions")).unwrap();
        fs::write(editions_index_path(&home), "not json").unwrap();
        assert!(read_editions(&home).is_empty());
        fs::write(
            editions_index_path(&home),
            r#"{"version": 1, "editions": [
                {"edition": 1, "path": "2026-09-29/0915-e0001.json", "at": "2026-09-29T06:15:51+00:00", "day": "2026-09-29", "trigger": "manual", "stories": 30, "changed": true},
                {"edition": "two"},
                {"edition": 3, "day": "2026-09-30"}
            ]}"#,
        )
        .unwrap();
        let editions = read_editions(&home);
        assert_eq!(editions.len(), 2);
        assert_eq!(editions[0].edition, 1);
        assert_eq!(editions[0].stories, 30);
        assert!(editions[0].changed);
        assert_eq!(editions[1].edition, 3);
        assert_eq!(editions[1].day, "2026-09-30");
        assert!(editions[1].trigger.is_empty());
    }

    #[test]
    fn history_is_newest_first_capped_and_tolerant() {
        let dir = TempDir::new("history");
        let home = dir.0.join("news");
        assert!(read_history(&home, MAX_HISTORY).is_empty());
        fs::create_dir_all(home.join("runs")).unwrap();
        let mut lines = String::new();
        for n in 0..60 {
            lines.push_str(&format!(
                r#"{{"started":"2026-09-{:02}T00:00:{:02}+00:00","trigger":"scheduled","outcome":"ok","edition":{n}}}"#,
                1 + n / 30,
                n % 30
            ));
            lines.push('\n');
            if n == 10 {
                lines.push_str("garbage line\n");
            }
        }
        fs::write(index_path(&home), &lines).unwrap();
        let history = read_history(&home, MAX_HISTORY);
        assert_eq!(history.len(), MAX_HISTORY);
        assert_eq!(history[0].edition, Some(59));
        assert_eq!(history[49].edition, Some(10));
        assert_eq!(index_len(&home), lines.len() as u64);
    }

    #[test]
    fn index_tail_and_server_records() {
        let dir = TempDir::new("tail");
        let home = dir.0.join("news");
        assert!(read_index_after(&home, 0).unwrap().is_empty());
        fs::create_dir_all(home.join("runs")).unwrap();
        fs::write(index_path(&home), format!("{RUNNER_LINE}\n")).unwrap();
        let before = index_len(&home);
        assert_eq!(read_index_after(&home, before).unwrap().len(), 0);

        let timeout = NewsRunRecord {
            started: "2026-09-29T17:00:00+00:00".into(),
            ended: Some("2026-09-29T18:05:00+00:00".into()),
            trigger: "scheduled".into(),
            outcome: "timeout".into(),
            seconds: Some(3900),
            errors: vec!["no result after 65 minutes".into()],
            ..NewsRunRecord::default()
        };
        append_index_record(&home, &timeout).unwrap();
        let appended = read_index_after(&home, before).unwrap();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].outcome, "timeout");
        assert_eq!(appended[0].errors, timeout.errors);
        let raw = fs::read_to_string(index_path(&home)).unwrap();
        let last: serde_json::Value = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
        assert_eq!(last["recorded_by"], "herdr");
        assert!(last.get("changed").is_none());
        let history = read_history(&home, MAX_HISTORY);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].outcome, "timeout");
        assert_eq!(history[1].outcome, "ok");
    }
}
