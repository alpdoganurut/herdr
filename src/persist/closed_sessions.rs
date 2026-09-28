//! Recently closed agent sessions.
//!
//! When the user closes a tab (or pane) that holds a resumable agent
//! session, Herdr records the session with the tab's context in
//! `closed-sessions.json` next to `session.json`, newest first and capped at
//! [`MAX_ENTRIES`], so it can be reopened later with the agent's native
//! resume command.
//!
//! The functions here are plain path operations with no application state so
//! they can be tested with temporary directories. Writes go through a
//! temporary file and a rename; a missing file is an empty list, and an
//! unreadable or corrupt one is an empty list with a warning (a corrupt file
//! is moved aside before it is replaced). Entries this build cannot decode
//! are skipped one by one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::schema::ClosedSessionInfo;

/// File name inside the session data directory.
pub const FILE_NAME: &str = "closed-sessions.json";
/// Where a corrupt file is moved before it is replaced.
const CORRUPT_FILE_NAME: &str = "closed-sessions.corrupt.json";
/// The most entries kept; older ones are dropped.
pub const MAX_ENTRIES: usize = 100;
const FILE_VERSION: u32 = 1;
/// Longest session-id tail used in an entry id.
const ID_SESSION_CHARS: usize = 40;

/// The record for the active session: next to its `session.json`.
pub fn store_path() -> PathBuf {
    crate::session::data_dir().join(FILE_NAME)
}

#[derive(Serialize)]
struct StoreFileOut<'a> {
    version: u32,
    sessions: &'a [ClosedSessionInfo],
}

#[derive(Deserialize)]
struct StoreFileIn {
    #[serde(default)]
    sessions: Vec<serde_json::Value>,
}

enum ReadOutcome {
    Entries(Vec<ClosedSessionInfo>),
    /// The file exists but is not a closed-sessions record.
    Corrupt,
}

fn read(path: &Path) -> ReadOutcome {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return ReadOutcome::Entries(Vec::new())
        }
        Err(err) => {
            tracing::warn!(
                event = "persist.closed_sessions.load",
                outcome = "read_error",
                path = %path.display(),
                err = %err,
                "failed to read the closed sessions file"
            );
            return ReadOutcome::Entries(Vec::new());
        }
    };
    let file: StoreFileIn = match serde_json::from_str(&content) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!(
                event = "persist.closed_sessions.load",
                outcome = "parse_error",
                path = %path.display(),
                err = %err,
                "closed sessions file is corrupt; starting with an empty list"
            );
            return ReadOutcome::Corrupt;
        }
    };
    let mut entries = Vec::with_capacity(file.sessions.len());
    for value in file.sessions {
        match serde_json::from_value::<ClosedSessionInfo>(value) {
            Ok(entry) => entries.push(entry),
            Err(err) => tracing::warn!(
                event = "persist.closed_sessions.load",
                outcome = "entry_skipped",
                path = %path.display(),
                err = %err,
                "skipping a closed session entry this build cannot read"
            ),
        }
    }
    ReadOutcome::Entries(entries)
}

/// Every recorded closed session, newest first. Never fails: a missing,
/// unreadable or corrupt file is an empty list.
pub fn load(path: &Path) -> Vec<ClosedSessionInfo> {
    match read(path) {
        ReadOutcome::Entries(entries) => entries,
        ReadOutcome::Corrupt => Vec::new(),
    }
}

/// Load for a rewrite: a corrupt file is moved aside first so the rewrite
/// does not destroy it.
fn load_for_update(path: &Path) -> Vec<ClosedSessionInfo> {
    match read(path) {
        ReadOutcome::Entries(entries) => entries,
        ReadOutcome::Corrupt => {
            let aside = path.with_file_name(CORRUPT_FILE_NAME);
            if let Err(err) = fs::rename(path, &aside) {
                tracing::warn!(
                    event = "persist.closed_sessions.load",
                    outcome = "move_aside_failed",
                    path = %path.display(),
                    err = %err,
                    "failed to move the corrupt closed sessions file aside"
                );
            }
            Vec::new()
        }
    }
}

/// Write the list atomically (a temporary file in the same directory, then
/// a rename).
pub fn save(path: &Path, entries: &[ClosedSessionInfo]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&StoreFileOut {
        version: FILE_VERSION,
        sessions: entries,
    })?;
    let tmp = path.with_file_name(format!(".{FILE_NAME}.tmp-{}", std::process::id()));
    let result = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Which native session an entry names: one entry per session.
pub fn session_key(entry: &ClosedSessionInfo) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{:?}\u{1f}{}",
        entry.source, entry.agent, entry.session_ref_kind, entry.session_id
    )
}

/// A stable id for a new entry: the close time and the tail of the session
/// id (reduced to `[A-Za-z0-9_-]`), with a numeric suffix when taken.
pub fn entry_id<'a>(
    closed_at: u64,
    session_id: &str,
    taken: impl IntoIterator<Item = &'a str> + Clone,
) -> String {
    let slug: String = session_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();
    let slug: String = slug
        .chars()
        .skip(slug.chars().count().saturating_sub(ID_SESSION_CHARS))
        .collect();
    let base = if slug.is_empty() {
        closed_at.to_string()
    } else {
        format!("{closed_at}-{slug}")
    };
    let is_taken = |candidate: &str| taken.clone().into_iter().any(|id| id == candidate);
    if !is_taken(&base) {
        return base;
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !is_taken(candidate))
        .unwrap_or(base)
}

/// Put `new_entries` (newest first) in front of the recorded ones. An older
/// entry for the same session is dropped, ids are made unique, and the list
/// is cut to [`MAX_ENTRIES`]. Returns the list as written.
pub fn record(
    path: &Path,
    new_entries: Vec<ClosedSessionInfo>,
) -> io::Result<Vec<ClosedSessionInfo>> {
    let existing = load_for_update(path);
    let mut entries: Vec<ClosedSessionInfo> = Vec::with_capacity(existing.len() + 1);
    let mut keys = std::collections::HashSet::new();
    for mut entry in new_entries.into_iter().chain(existing) {
        if !keys.insert(session_key(&entry)) {
            continue;
        }
        if entries.iter().any(|kept| kept.id == entry.id) {
            entry.id = entry_id(
                entry.closed_at,
                &entry.session_id,
                entries.iter().map(|kept| kept.id.as_str()),
            );
        }
        entries.push(entry);
        if entries.len() == MAX_ENTRIES {
            break;
        }
    }
    save(path, &entries)?;
    Ok(entries)
}

/// Drop the entry with `id`; `Ok(None)` when there is none (nothing is
/// written then).
pub fn remove(path: &Path, id: &str) -> io::Result<Option<ClosedSessionInfo>> {
    let mut entries = load_for_update(path);
    let Some(index) = entries.iter().position(|entry| entry.id == id) else {
        return Ok(None);
    };
    let removed = entries.remove(index);
    save(path, &entries)?;
    Ok(Some(removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::AgentSessionRefKind;
    use crate::api::schema::{TabColor, TabRemindInterval};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "herdr-closed-sessions-{name}-{}",
                std::process::id()
            ));
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

    fn entry(session_id: &str, closed_at: u64) -> ClosedSessionInfo {
        ClosedSessionInfo {
            id: entry_id(closed_at, session_id, std::iter::empty()),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref_kind: AgentSessionRefKind::Id,
            session_id: session_id.into(),
            transcript_path: Some(format!("/home/me/.claude/projects/-p/{session_id}.jsonl")),
            label: Some("review".into()),
            color: Some(TabColor::Green),
            important: true,
            remind_every: Some(TabRemindInterval::H1),
            space_id: "w_abc".into(),
            space_name: "leap".into(),
            cwd: "/tmp/project".into(),
            closed_at,
        }
    }

    #[test]
    fn record_round_trips_newest_first() {
        let dir = TempDir::new("round-trip");
        assert!(load(&dir.file()).is_empty(), "a missing file is empty");

        record(&dir.file(), vec![entry("first", 100)]).unwrap();
        record(&dir.file(), vec![entry("second", 200), entry("third", 200)]).unwrap();
        let entries = load(&dir.file());
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.session_id.as_str())
                .collect::<Vec<_>>(),
            ["second", "third", "first"]
        );
        assert_eq!(entries[2], entry("first", 100), "every field survives");
        assert_eq!(entries[0].id, "200-second");

        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.file()).unwrap()).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["sessions"][2]["remind_every"], "1h");
        assert_eq!(json["sessions"][2]["color"], "green");
        let leftovers: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "no temporary files: {leftovers:?}");
    }

    #[test]
    fn record_keeps_one_entry_per_session_and_unique_ids() {
        let dir = TempDir::new("dedupe");
        record(&dir.file(), vec![entry("same", 100)]).unwrap();
        let mut again = entry("same", 300);
        again.label = Some("renamed".into());
        record(&dir.file(), vec![again]).unwrap();
        let entries = load(&dir.file());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label.as_deref(), Some("renamed"));
        assert_eq!(entries[0].closed_at, 300);

        // Two sessions whose ids reduce to the same slug in the same second.
        let mut a = entry("a/b", 400);
        a.session_ref_kind = AgentSessionRefKind::Path;
        let b = entry("ab", 400);
        assert_eq!(a.id, b.id);
        record(&dir.file(), vec![a, b]).unwrap();
        let ids: Vec<String> = load(&dir.file()).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["400-ab", "400-ab-2", "300-same"]);
    }

    #[test]
    fn record_caps_the_list_dropping_the_oldest() {
        let dir = TempDir::new("cap");
        for index in 0..(MAX_ENTRIES as u64 + 5) {
            record(&dir.file(), vec![entry(&format!("s{index}"), index)]).unwrap();
        }
        let entries = load(&dir.file());
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].session_id, format!("s{}", MAX_ENTRIES + 4));
        assert_eq!(entries[MAX_ENTRIES - 1].session_id, "s5");
    }

    #[test]
    fn corrupt_file_loads_empty_and_is_moved_aside_before_a_rewrite() {
        let dir = TempDir::new("corrupt");
        fs::write(dir.file(), "{ not json").unwrap();
        assert!(load(&dir.file()).is_empty());
        // Reading alone never touches it.
        assert_eq!(fs::read_to_string(dir.file()).unwrap(), "{ not json");

        record(&dir.file(), vec![entry("fresh", 1)]).unwrap();
        assert_eq!(load(&dir.file()).len(), 1);
        assert_eq!(
            fs::read_to_string(dir.0.join(CORRUPT_FILE_NAME)).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn undecodable_entries_are_skipped_one_by_one() {
        let dir = TempDir::new("skip");
        let good = serde_json::to_value(entry("good", 5)).unwrap();
        let file = serde_json::json!({
            "version": 1,
            "sessions": [{ "id": "broken" }, good, { "future": true }],
        });
        fs::write(dir.file(), file.to_string()).unwrap();
        let entries = load(&dir.file());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "good");

        // Unknown color and interval names decode to the fallbacks.
        let mut odd = serde_json::to_value(entry("odd", 6)).unwrap();
        odd["color"] = "magenta".into();
        odd["remind_every"] = "2h".into();
        let odd: ClosedSessionInfo = serde_json::from_value(odd).unwrap();
        assert_eq!(odd.color, Some(TabColor::Unknown));
        assert_eq!(odd.remind_every, Some(TabRemindInterval::Unknown));
    }

    #[test]
    fn remove_drops_one_entry_by_id() {
        let dir = TempDir::new("remove");
        record(&dir.file(), vec![entry("keep", 1)]).unwrap();
        record(&dir.file(), vec![entry("drop", 2)]).unwrap();
        assert_eq!(remove(&dir.file(), "nope").unwrap(), None);
        let removed = remove(&dir.file(), "2-drop").unwrap().expect("removed");
        assert_eq!(removed.session_id, "drop");
        let entries = load(&dir.file());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "keep");
    }

    #[test]
    fn entry_ids_use_a_safe_session_tail() {
        assert_eq!(entry_id(7, "abc-123", std::iter::empty()), "7-abc-123");
        assert_eq!(
            entry_id(7, "/home/me/.pi/sessions/x.jsonl", std::iter::empty()),
            "7-homemepisessionsxjsonl"
        );
        assert_eq!(entry_id(7, "///", std::iter::empty()), "7");
        let long = "a".repeat(60);
        assert_eq!(
            entry_id(7, &long, std::iter::empty()),
            format!("7-{}", "a".repeat(ID_SESSION_CHARS))
        );
        assert_eq!(entry_id(7, "x", ["7-x", "7-x-2"]), "7-x-3");
    }
}
