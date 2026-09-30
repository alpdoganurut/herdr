//! The browser ledger on disk: `browser.json` next to `session.json`
//! (tab records, short ids, pane cursors, the last activity entries) and
//! `browser-activity.jsonl` (append-only, rotated). Same rules as the
//! closed-sessions store: versioned, temp+rename, a corrupt file is moved
//! aside, entries this build cannot read are skipped.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::api::schema::BrowserActivity;
use crate::browser::state::LedgerSnapshot;

pub const LEDGER_FILE: &str = "browser.json";
pub const ACTIVITY_FILE: &str = "browser-activity.jsonl";
const CORRUPT_FILE: &str = "browser.corrupt.json";
const FILE_VERSION: u32 = 1;
/// The activity log is rotated (one `.1` copy kept) past this size.
pub const MAX_ACTIVITY_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    #[serde(flatten)]
    ledger: &'a LedgerSnapshot,
}

#[derive(Deserialize)]
struct FileIn {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    tabs: Vec<serde_json::Value>,
    #[serde(default)]
    cursors: serde_json::Value,
    #[serde(default)]
    next_short: serde_json::Value,
    #[serde(default)]
    log: Vec<serde_json::Value>,
    #[serde(default)]
    seq: u64,
}

/// Load the ledger; `None` when missing. A corrupt file is moved aside and
/// treated as missing; unreadable entries are skipped one by one.
pub fn load(path: &Path) -> Option<LedgerSnapshot> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::warn!(event = "persist.browser.load", outcome = "read_error", path = %path.display(), err = %err, "failed to read browser.json");
            return None;
        }
    };
    let file: FileIn = match serde_json::from_str(&content) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(event = "persist.browser.load", outcome = "parse_error", path = %path.display(), err = %err, "browser.json is corrupt; moved aside");
            let _ = fs::rename(path, path.with_file_name(CORRUPT_FILE));
            return None;
        }
    };
    if file.version > FILE_VERSION {
        tracing::warn!(
            event = "persist.browser.load",
            outcome = "newer_version",
            version = file.version,
            "browser.json was written by a newer herdr; reading what this build knows"
        );
    }
    let mut snapshot = LedgerSnapshot {
        seq: file.seq,
        ..Default::default()
    };
    for value in file.tabs {
        match serde_json::from_value(value) {
            Ok(record) => snapshot.tabs.push(record),
            Err(err) => {
                tracing::warn!(event = "persist.browser.load", outcome = "entry_skipped", err = %err, "skipping a browser tab record")
            }
        }
    }
    snapshot.cursors = serde_json::from_value(file.cursors).unwrap_or_default();
    snapshot.next_short = serde_json::from_value(file.next_short).unwrap_or_default();
    for value in file.log {
        if let Ok(entry) = serde_json::from_value::<BrowserActivity>(value) {
            snapshot.log.push(entry);
        }
    }
    Some(snapshot)
}

/// Write the ledger atomically.
pub fn save(path: &Path, ledger: &LedgerSnapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&FileOut {
        version: FILE_VERSION,
        ledger,
    })
    .map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Append activity entries as JSON lines, rotating past [`MAX_ACTIVITY_BYTES`].
pub fn append_activity(path: &Path, entries: &[BrowserActivity]) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path)
        .map(|m| m.len() > MAX_ACTIVITY_BYTES)
        .unwrap_or(false)
    {
        let rotated = path.with_extension("jsonl.1");
        let _ = fs::rename(path, rotated);
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut buffer = String::new();
    for entry in entries {
        buffer.push_str(&serde_json::to_string(entry).map_err(io::Error::other)?);
        buffer.push('\n');
    }
    file.write_all(buffer.as_bytes())
}

/// Chromium's `profile.exit_type` from `<profile>/Default/Preferences`
/// (`Normal`, `SessionEnded` or `Crashed`), used to tell a quit from a crash.
pub fn profile_exit_type(profile_dir: &Path) -> Option<String> {
    let content = fs::read_to_string(profile_dir.join("Default").join("Preferences")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .pointer("/profile/exit_type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::BrowserActor;
    use crate::browser::state::{BrowserState, HostTab, TabKey};
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-persist-browser-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ledger_round_trips_and_a_corrupt_file_is_moved_aside() {
        let dir = temp("roundtrip");
        let path = dir.join(LEDGER_FILE);
        assert!(load(&path).is_none());
        let mut state = BrowserState::new();
        let key = TabKey::new("main", "T");
        state.adopt_tab(
            &key,
            &HostTab {
                target: "T".into(),
                url: "https://a/".into(),
                ..Default::default()
            },
            &BrowserActor::User,
            5,
        );
        state.set_cursor("w2:pD", &key, Some("w2:tD"), 6);
        state.touch(
            "main",
            Some(&key),
            &BrowserActor::User,
            "read",
            "x",
            true,
            1,
            7,
        );
        save(&path, &state.snapshot()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].short, "t1");
        assert_eq!(loaded.cursors["w2:pD"].key, key);
        assert_eq!(loaded.next_short["main"], 2);
        assert_eq!(loaded.log.len(), 1);
        assert_eq!(loaded.seq, state.seq);

        fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_none());
        assert!(dir.join(CORRUPT_FILE).exists());
        assert!(!path.exists());

        // an entry this build cannot read is skipped, the rest survives
        fs::write(&path, r#"{"version":1,"tabs":[{"nonsense":true},{"short":"t2","profile":"main","target_id":"U","opened_by":{"kind":"user"},"last_actor":{"kind":"user"}}],"seq":9}"#).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].target_id, "U");
        assert_eq!(loaded.seq, 9);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn activity_appends_lines_and_rotates() {
        let dir = temp("activity");
        let path = dir.join(ACTIVITY_FILE);
        let entry = BrowserActivity {
            seq: 1,
            at: 2,
            profile: "main".into(),
            tab: Some("main:t1".into()),
            actor: BrowserActor::User,
            op: "navigate".into(),
            detail: "a".into(),
            ok: true,
            ms: 0,
        };
        append_activity(&path, &[entry.clone(), entry.clone()]).unwrap();
        let lines: Vec<String> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"op\":\"navigate\""));
        // force a rotation
        let big = vec![b'x'; (MAX_ACTIVITY_BYTES + 1) as usize];
        fs::write(&path, big).unwrap();
        append_activity(&path, &[entry]).unwrap();
        assert!(dir.join("browser-activity.jsonl.1").exists());
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_type_is_read_from_preferences() {
        let dir = temp("exit");
        assert_eq!(profile_exit_type(&dir), None);
        fs::create_dir_all(dir.join("Default")).unwrap();
        fs::write(
            dir.join("Default/Preferences"),
            r#"{"profile":{"exit_type":"Crashed"}}"#,
        )
        .unwrap();
        assert_eq!(profile_exit_type(&dir).as_deref(), Some("Crashed"));
        let _ = fs::remove_dir_all(&dir);
    }
}
