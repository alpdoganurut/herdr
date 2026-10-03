//! The action log (`<coordinator dir>/actions.jsonl`): one JSON line per
//! agent action, denial, completion, and user action on tabs and teams.
//!
//! The server is the only writer (synchronous, low-frequency appends, like
//! the notes store), so lines never interleave. The log rotates to
//! `actions.1.jsonl` past [`ROTATE_BYTES`].

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub use crate::api::schema::agents_model::AgentActionEntry;

/// The log rotates past this size (as the message log).
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Bytes read from the end per step while looking for the newest lines.
const TAIL_CHUNK: u64 = 64 * 1024;

pub fn actions_path(dir: &Path) -> PathBuf {
    dir.join("actions.jsonl")
}

pub fn rotated_path(dir: &Path) -> PathBuf {
    dir.join("actions.1.jsonl")
}

/// Append one entry, rotating first when the log is past [`ROTATE_BYTES`].
pub fn append(dir: &Path, entry: &AgentActionEntry) -> io::Result<()> {
    let mut line = serde_json::to_string(entry).map_err(io::Error::other)?;
    line.push('\n');
    let path = actions_path(dir);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > ROTATE_BYTES) {
        std::fs::rename(&path, rotated_path(dir))?;
    }
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// The newest `n` entries, newest first. Reads from the end of the log, and
/// from the rotated log when the current one holds fewer. Unparseable lines
/// are skipped; a missing log is empty.
pub fn read_tail(dir: &Path, n: usize) -> Vec<AgentActionEntry> {
    let mut out = tail_of(&actions_path(dir), n);
    if out.len() < n {
        let more = tail_of(&rotated_path(dir), n - out.len());
        out.extend(more);
    }
    out
}

/// The newest `n` parseable entries of one file, newest first.
fn tail_of(path: &Path, n: usize) -> Vec<AgentActionEntry> {
    if n == 0 {
        return Vec::new();
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Vec::new();
    };
    let mut window = TAIL_CHUNK.min(len);
    loop {
        let start = len - window;
        let mut bytes = Vec::with_capacity(window as usize);
        if file.seek(SeekFrom::Start(start)).is_err()
            || (&mut file).take(window).read_to_end(&mut bytes).is_err()
        {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut lines: Vec<&str> = text.lines().collect();
        // A window that does not start at the beginning may cut its first line.
        if start > 0 && !lines.is_empty() {
            lines.remove(0);
        }
        let entries: Vec<AgentActionEntry> = lines
            .iter()
            .rev()
            .filter_map(|line| serde_json::from_str(line).ok())
            .take(n)
            .collect();
        if entries.len() >= n || start == 0 {
            return entries;
        }
        window = (window * 2).min(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::agents_model::{AgentActorKind, AgentsActionOutcome};

    fn entry(id: &str) -> AgentActionEntry {
        AgentActionEntry {
            unix: 1,
            id: id.into(),
            actor: AgentActorKind::Agent,
            actor_pane: Some("w1:p1".into()),
            actor_name: Some("lead".into()),
            action: "rename_tab".into(),
            target_tab: Some("w1:t2".into()),
            target_pane: None,
            target_name: None,
            team: None,
            turn_origin: None,
            origin_detail: None,
            outcome: AgentsActionOutcome::Ok,
            code: None,
            detail: None,
            closed_ids: Vec::new(),
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-actions-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn appends_and_reads_the_newest_first() {
        let dir = test_dir("tail");
        assert!(read_tail(&dir, 5).is_empty());
        for i in 0..7 {
            append(&dir, &entry(&format!("a{i}"))).unwrap();
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(actions_path(&dir))
            .unwrap()
            .write_all(b"not json\n")
            .unwrap();
        let ids: Vec<String> = read_tail(&dir, 3).into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["a6", "a5", "a4"]);
        assert_eq!(read_tail(&dir, 100).len(), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_past_one_chunk_and_into_the_rotated_log() {
        let dir = test_dir("rotate");
        std::fs::create_dir_all(&dir).unwrap();
        let mut old = entry("old");
        old.detail = Some("x".repeat(100));
        let line = format!("{}\n", serde_json::to_string(&old).unwrap());
        std::fs::write(rotated_path(&dir), line.repeat(2)).unwrap();
        let mut big = entry("big");
        big.detail = Some("y".repeat(1000));
        for _ in 0..100 {
            append(&dir, &big).unwrap();
        }
        let all = read_tail(&dir, 102);
        assert_eq!(all.len(), 102);
        assert_eq!(all[101].id, "old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
