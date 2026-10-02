//! The coordinator's server-side record (fork).
//!
//! `coordinator.json` next to `session.json` keeps what the server must
//! remember about the coordinator across restarts: its tab and pane, the
//! coordinator session, the relaunch window, why it is down, the
//! notification ledger, the suggestions already notified, the unread count,
//! and whether the POC migration ran. Shared coordinator data (registry,
//! messages, board) lives in the coordinator directory, not here.
//!
//! Plain path operations, no application state. Writes go through a
//! temporary file and a rename; a missing file is the default record; an
//! unreadable or corrupt one is the default record with a warning, and a
//! corrupt file (or one written by a newer build) is moved aside so the next
//! save does not destroy it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the session data directory.
pub const FILE_NAME: &str = "coordinator.json";
/// Where a corrupt file is moved before it is replaced.
const CORRUPT_FILE_NAME: &str = "coordinator.corrupt.json";
/// Where a file from a newer build is moved before it is replaced.
const NEWER_FILE_NAME: &str = "coordinator.newer.json";
const FILE_VERSION: u32 = 1;
/// Suggestion hashes kept (the newest).
pub const MAX_SEEN_SUGGESTIONS: usize = 500;

/// The record for the active session: next to its `session.json`.
pub fn store_path() -> PathBuf {
    crate::session::data_dir().join(FILE_NAME)
}

/// Why the coordinator is down (persisted so a restart does not hide it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedDown {
    /// Seconds since the Unix epoch.
    pub since: u64,
    /// `relaunch_cap`, `start_failed`, `name_taken`, `launch_timeout`, ...
    pub reason: String,
}

/// A notification waiting for a client shell or the end of quiet hours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCoordinatorNotify {
    /// `suggestions`, `down`, `blocked` or `locked`.
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Bypasses quiet hours (`down`).
    #[serde(default)]
    pub urgent: bool,
    /// Not before this time (seconds since the epoch); zero for now.
    #[serde(default)]
    pub deliver_after: u64,
    #[serde(default)]
    pub queued_at: u64,
}

/// The notification ledger.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CoordinatorNotifyRecord {
    /// The local day (`YYYY-MM-DD`) the counter is for.
    pub day: String,
    /// Suggestion notifications delivered that day.
    pub delivered: u32,
    /// Failure kinds already alerted (`down`, `blocked`, `locked`): one
    /// notification each until the condition clears.
    pub alerted: Vec<String>,
    /// Waiting for a client shell or the end of quiet hours; at most one per
    /// kind, the newest replacing the older.
    pub pending: Vec<PendingCoordinatorNotify>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CoordinatorRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The coordinator's Claude session (for `--resume`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Relaunch times within the last hour (seconds since the epoch).
    pub relaunches: Vec<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub down: Option<PersistedDown>,
    pub notify: CoordinatorNotifyRecord,
    /// Hashes of the board suggestions already notified, oldest first.
    pub seen_suggestions: Vec<u64>,
    /// Suggestions not looked at yet (the tab or the dashboard).
    pub unread: u32,
    /// The one-time migration from the POC ran.
    pub migrated: bool,
    /// A one-time notice for the read model (e.g. after the migration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl CoordinatorRecord {
    /// Remember suggestion hashes, keeping the newest [`MAX_SEEN_SUGGESTIONS`].
    pub fn remember_suggestions(&mut self, hashes: impl IntoIterator<Item = u64>) {
        for hash in hashes {
            if !self.seen_suggestions.contains(&hash) {
                self.seen_suggestions.push(hash);
            }
        }
        if self.seen_suggestions.len() > MAX_SEEN_SUGGESTIONS {
            let extra = self.seen_suggestions.len() - MAX_SEEN_SUGGESTIONS;
            self.seen_suggestions.drain(..extra);
        }
    }
}

#[derive(Serialize)]
struct StoreFileOut<'a> {
    version: u32,
    #[serde(flatten)]
    record: &'a CoordinatorRecord,
}

#[derive(Deserialize)]
struct StoreFileHead {
    #[serde(default)]
    version: u32,
}

fn move_aside(path: &Path, name: &str, outcome: &'static str) {
    let aside = path.with_file_name(name);
    if let Err(err) = fs::rename(path, &aside) {
        tracing::warn!(
            event = "persist.coordinator.load",
            outcome,
            path = %path.display(),
            err = %err,
            "failed to move the coordinator record aside"
        );
    }
}

/// Load the record. Never fails: a missing, unreadable, corrupt or newer
/// file is the default record (a corrupt or newer one is moved aside first).
pub fn load(path: &Path) -> CoordinatorRecord {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return CoordinatorRecord::default(),
        Err(err) => {
            tracing::warn!(
                event = "persist.coordinator.load",
                outcome = "read_error",
                path = %path.display(),
                err = %err,
                "failed to read the coordinator record"
            );
            return CoordinatorRecord::default();
        }
    };
    let parsed = serde_json::from_str::<StoreFileHead>(&content).and_then(|head| {
        serde_json::from_str::<CoordinatorRecord>(&content).map(|record| (head, record))
    });
    match parsed {
        Ok((head, _)) if head.version > FILE_VERSION => {
            tracing::warn!(
                event = "persist.coordinator.load",
                outcome = "newer_version",
                path = %path.display(),
                version = head.version,
                "coordinator record is from a newer herdr; starting from the defaults"
            );
            move_aside(path, NEWER_FILE_NAME, "move_aside_failed");
            CoordinatorRecord::default()
        }
        Ok((_, record)) => record,
        Err(err) => {
            tracing::warn!(
                event = "persist.coordinator.load",
                outcome = "parse_error",
                path = %path.display(),
                err = %err,
                "coordinator record is corrupt; starting from the defaults"
            );
            move_aside(path, CORRUPT_FILE_NAME, "move_aside_failed");
            CoordinatorRecord::default()
        }
    }
}

/// Write the record atomically (a temporary file in the same directory,
/// then a rename).
pub fn save(path: &Path, record: &CoordinatorRecord) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-persist-coordinator-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_a_missing_file_is_the_default() {
        let dir = temp("roundtrip");
        let path = dir.join(FILE_NAME);
        assert_eq!(load(&path), CoordinatorRecord::default());
        let record = CoordinatorRecord {
            tab_id: Some("w1:t3".into()),
            pane_id: Some("w1:p3".into()),
            session: Some("s".into()),
            relaunches: vec![10, 20],
            down: Some(PersistedDown {
                since: 30,
                reason: "relaunch_cap".into(),
            }),
            notify: CoordinatorNotifyRecord {
                day: "2026-10-02".into(),
                delivered: 2,
                alerted: vec!["down".into()],
                pending: vec![PendingCoordinatorNotify {
                    kind: "suggestions".into(),
                    title: "coordinator: 1 new suggestion".into(),
                    body: None,
                    urgent: false,
                    deliver_after: 0,
                    queued_at: 5,
                }],
            },
            seen_suggestions: vec![1, 2],
            unread: 2,
            migrated: true,
            notice: Some("close the old herdr+ group".into()),
        };
        save(&path, &record).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        assert_eq!(load(&path), record);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_moved_aside() {
        let dir = temp("corrupt");
        let path = dir.join(FILE_NAME);
        fs::write(&path, "{oops").unwrap();
        assert_eq!(load(&path), CoordinatorRecord::default());
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.join(CORRUPT_FILE_NAME)).unwrap(),
            "{oops"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_version_is_moved_aside_and_an_older_one_loads() {
        let dir = temp("version");
        let path = dir.join(FILE_NAME);
        fs::write(&path, r#"{"version":2,"migrated":true}"#).unwrap();
        assert_eq!(load(&path), CoordinatorRecord::default());
        assert!(dir.join(NEWER_FILE_NAME).exists());
        fs::write(&path, r#"{"migrated":true,"future_field":1}"#).unwrap();
        assert!(load(&path).migrated, "no version reads as the first one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seen_suggestions_keep_the_newest() {
        let mut record = CoordinatorRecord::default();
        record.remember_suggestions(0..(MAX_SEEN_SUGGESTIONS as u64 + 10));
        record.remember_suggestions([3]);
        assert_eq!(record.seen_suggestions.len(), MAX_SEEN_SUGGESTIONS);
        assert_eq!(record.seen_suggestions[0], 11);
        assert_eq!(record.seen_suggestions.last(), Some(&3));
        record.remember_suggestions([3]);
        assert_eq!(
            record.seen_suggestions.len(),
            MAX_SEEN_SUGGESTIONS,
            "no duplicates"
        );
    }
}
