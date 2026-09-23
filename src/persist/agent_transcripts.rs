//! Backups of native agent conversation transcripts.
//!
//! Some agents delete their own transcripts after a period of inactivity, and
//! a later native resume then finds no conversation. Herdr keeps a copy of
//! the transcript behind each open or suspended agent pane under the session
//! directory:
//!
//! ```text
//! agent-transcripts/<agent>/<session-id>/transcript.jsonl
//! agent-transcripts/<agent>/<session-id>/side/...
//! agent-transcripts/<agent>/<session-id>/meta.json
//! ```
//!
//! and puts the copy back before a resume when the native file is gone.
//! Files are copied only when the source is newer or a different size, every
//! write goes through a temporary file in the same directory followed by a
//! rename, and nothing under the store is ever deleted by Herdr.
//!
//! The functions here are plain path operations with no application state so
//! they can run on a background thread and be tested with temporary
//! directories.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::agent_resume::{
    is_safe_path_component, native_transcript_locations, NativeTranscript, PersistedAgentSession,
};

/// Directory name of the store inside the session data directory.
pub const STORE_DIR_NAME: &str = "agent-transcripts";
const TRANSCRIPT_FILE: &str = "transcript.jsonl";
const SIDE_DIR: &str = "side";
const META_FILE: &str = "meta.json";
/// Deepest side-directory nesting copied; deeper entries are left alone.
const MAX_SIDE_DEPTH: usize = 8;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The store for the active session: next to its `session.json`.
pub fn store_dir() -> PathBuf {
    crate::session::data_dir().join(STORE_DIR_NAME)
}

/// One session to back up, with the pane context recorded in its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptBackupRequest {
    pub session: PersistedAgentSession,
    pub cwd: Option<PathBuf>,
    pub label: Option<String>,
}

/// `meta.json` next to a backed-up transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptBackupMeta {
    pub source: String,
    pub agent: String,
    pub session_id: String,
    /// Where the transcript lived when it was copied; the restore target when
    /// the agent's own location can no longer be derived.
    pub original_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// RFC 3339 timestamp of the last copy.
    pub backed_up_at: String,
    /// Size of the backed-up transcript file.
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupOutcome {
    /// Herdr does not know where this agent keeps its transcripts, or the
    /// session id cannot be used as a directory name.
    NotSupported,
    /// The agent has no transcript file for the session (yet, or any more).
    NoNativeTranscript,
    /// The backup already matched the native files.
    Unchanged,
    /// The transcript or its side data was copied.
    Updated { bytes: u64 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackupSummary {
    pub updated: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// Herdr does not know where this agent keeps its transcripts.
    NotSupported,
    /// The native transcript is still there; nothing was touched.
    NativePresent,
    /// No backup exists for the session.
    NoBackup,
    /// The backup was copied back to `path`.
    Restored { path: PathBuf },
}

/// A backed-up session as listed by `herdr agent transcripts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranscriptBackupEntry {
    pub agent: String,
    pub session_id: String,
    pub bytes: u64,
    pub backed_up_at: String,
    pub original_path: PathBuf,
    /// Whether the transcript currently exists at its original path.
    pub native_present: bool,
}

/// Directory holding one session's backup, when `agent` and `session_id`
/// are safe to use as path components.
pub fn backup_dir(store_dir: &Path, agent: &str, session_id: &str) -> Option<PathBuf> {
    (is_safe_path_component(agent) && is_safe_path_component(session_id))
        .then(|| store_dir.join(agent).join(session_id))
}

/// Back up every request, logging failures instead of stopping.
pub fn sync_backups(store_dir: &Path, requests: &[TranscriptBackupRequest]) -> BackupSummary {
    let mut summary = BackupSummary::default();
    for request in requests {
        match backup_session(store_dir, request) {
            Ok(BackupOutcome::Updated { bytes }) => {
                summary.updated += 1;
                tracing::info!(
                    event = "agent.transcript.backup",
                    agent = %request.session.agent,
                    session_id = %request.session.session_ref.value,
                    bytes,
                    "backed up native agent transcript"
                );
            }
            Ok(BackupOutcome::Unchanged) => summary.unchanged += 1,
            Ok(BackupOutcome::NotSupported | BackupOutcome::NoNativeTranscript) => {
                summary.skipped += 1;
            }
            Err(err) => {
                summary.failed += 1;
                tracing::warn!(
                    event = "agent.transcript.backup",
                    outcome = "error",
                    agent = %request.session.agent,
                    session_id = %request.session.session_ref.value,
                    err = %err,
                    "failed to back up native agent transcript"
                );
            }
        }
    }
    summary
}

/// Back up one session from the agent's own transcript location.
pub fn backup_session(
    store_dir: &Path,
    request: &TranscriptBackupRequest,
) -> io::Result<BackupOutcome> {
    let locations = native_transcript_locations(&request.session);
    backup_session_from(store_dir, request, locations.as_ref())
}

/// Back up one session from explicit native locations.
pub fn backup_session_from(
    store_dir: &Path,
    request: &TranscriptBackupRequest,
    locations: Option<&NativeTranscript>,
) -> io::Result<BackupOutcome> {
    let session = &request.session;
    let Some(locations) = locations else {
        return Ok(BackupOutcome::NotSupported);
    };
    let Some(dir) = backup_dir(store_dir, &session.agent, &session.session_ref.value) else {
        return Ok(BackupOutcome::NotSupported);
    };
    let source_meta = match fs::metadata(&locations.file) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(BackupOutcome::NoNativeTranscript),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(BackupOutcome::NoNativeTranscript);
        }
        Err(err) => return Err(err),
    };

    let transcript_backup = dir.join(TRANSCRIPT_FILE);
    let mut changed = copy_if_changed(&locations.file, &transcript_backup)?.is_some();
    if let Some(side_dir) = locations.side_dir.as_ref().filter(|dir| dir.is_dir()) {
        changed |= copy_tree_if_changed(side_dir, &dir.join(SIDE_DIR), 0)?;
    }

    let meta_path = dir.join(META_FILE);
    if !changed && meta_path.is_file() {
        return Ok(BackupOutcome::Unchanged);
    }
    let bytes = source_meta.len();
    let meta = TranscriptBackupMeta {
        source: session.source.clone(),
        agent: session.agent.clone(),
        session_id: session.session_ref.value.clone(),
        original_path: locations.file.clone(),
        cwd: request.cwd.clone(),
        label: request.label.clone(),
        backed_up_at: rfc3339_now(),
        bytes,
    };
    write_atomically(&meta_path, serde_json::to_string_pretty(&meta)?.as_bytes())?;
    Ok(BackupOutcome::Updated { bytes })
}

/// Put a backed-up transcript back where the agent expects it when the
/// native file is gone. An existing native file is never touched.
pub fn restore_if_missing(
    session: &PersistedAgentSession,
    store_dir: &Path,
) -> io::Result<RestoreOutcome> {
    let locations = native_transcript_locations(session);
    restore_if_missing_from(session, store_dir, locations.as_ref())
}

/// [`restore_if_missing`] with explicit native locations; `None` falls back
/// to the original path recorded in the backup's metadata.
pub fn restore_if_missing_from(
    session: &PersistedAgentSession,
    store_dir: &Path,
    locations: Option<&NativeTranscript>,
) -> io::Result<RestoreOutcome> {
    let Some(dir) = backup_dir(store_dir, &session.agent, &session.session_ref.value) else {
        return Ok(RestoreOutcome::NotSupported);
    };
    let transcript_backup = dir.join(TRANSCRIPT_FILE);
    if !transcript_backup.is_file() {
        return Ok(RestoreOutcome::NoBackup);
    }
    let target = match locations {
        Some(locations) => locations.file.clone(),
        None => match read_meta(&dir.join(META_FILE)) {
            Some(meta) => meta.original_path,
            None => return Ok(RestoreOutcome::NotSupported),
        },
    };
    if target.exists() {
        return Ok(RestoreOutcome::NativePresent);
    }
    let Some(parent) = target.parent() else {
        return Ok(RestoreOutcome::NotSupported);
    };
    fs::create_dir_all(parent)?;
    copy_if_missing(&transcript_backup, &target)?;

    let side_backup = dir.join(SIDE_DIR);
    if side_backup.is_dir() {
        let side_target = parent.join(&session.session_ref.value);
        copy_tree_if_missing(&side_backup, &side_target, 0)?;
    }
    tracing::info!(
        event = "agent.transcript.restore",
        agent = %session.agent,
        session_id = %session.session_ref.value,
        path = %target.display(),
        "restored native agent transcript from backup"
    );
    Ok(RestoreOutcome::Restored { path: target })
}

/// Every backed-up session in the store, ordered by agent and session id.
pub fn list_backups(store_dir: &Path) -> io::Result<Vec<TranscriptBackupEntry>> {
    let mut entries = Vec::new();
    let agents = match fs::read_dir(store_dir) {
        Ok(agents) => agents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(entries),
        Err(err) => return Err(err),
    };
    for agent_dir in agents.filter_map(Result::ok) {
        if !agent_dir.path().is_dir() {
            continue;
        }
        let agent = agent_dir.file_name().to_string_lossy().into_owned();
        for session_dir in fs::read_dir(agent_dir.path())?.filter_map(Result::ok) {
            let dir = session_dir.path();
            let transcript = dir.join(TRANSCRIPT_FILE);
            if !transcript.is_file() {
                continue;
            }
            let session_id = session_dir.file_name().to_string_lossy().into_owned();
            let meta = read_meta(&dir.join(META_FILE));
            let bytes = meta
                .as_ref()
                .map(|meta| meta.bytes)
                .or_else(|| fs::metadata(&transcript).ok().map(|meta| meta.len()))
                .unwrap_or(0);
            let original_path = meta
                .as_ref()
                .map(|meta| meta.original_path.clone())
                .unwrap_or_default();
            entries.push(TranscriptBackupEntry {
                native_present: !original_path.as_os_str().is_empty() && original_path.is_file(),
                agent: agent.clone(),
                session_id,
                bytes,
                backed_up_at: meta.map(|meta| meta.backed_up_at).unwrap_or_default(),
                original_path,
            });
        }
    }
    entries.sort_by(|a, b| (&a.agent, &a.session_id).cmp(&(&b.agent, &b.session_id)));
    Ok(entries)
}

fn read_meta(path: &Path) -> Option<TranscriptBackupMeta> {
    let content = fs::read(path).ok()?;
    serde_json::from_slice(&content).ok()
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn modified(meta: &fs::Metadata) -> SystemTime {
    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

fn temp_path_for(target: &Path) -> io::Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no file name"))?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{}.tmp-{}-{counter}",
        name.to_string_lossy(),
        std::process::id()
    )))
}

/// Copy `src` to `dst` through a temporary file and rename, keeping the
/// source's modification time so later comparisons can skip unchanged files.
fn copy_atomically(src: &Path, dst: &Path, src_meta: &fs::Metadata) -> io::Result<u64> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_path_for(dst)?;
    let result = fs::copy(src, &tmp).and_then(|bytes| {
        if let Ok(file) = fs::File::options().write(true).open(&tmp) {
            // A backup with an older mtime than its source is copied again
            // next time; failing to set it only costs one extra copy.
            let _ = file.set_modified(modified(src_meta));
        }
        fs::rename(&tmp, dst).map(|()| bytes)
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_atomically(target: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_path_for(target)?;
    let result = fs::write(&tmp, content).and_then(|()| fs::rename(&tmp, target));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copy when the destination is missing, a different size, or older than
/// the source. Returns the copied byte count.
fn copy_if_changed(src: &Path, dst: &Path) -> io::Result<Option<u64>> {
    let src_meta = fs::metadata(src)?;
    if let Ok(dst_meta) = fs::metadata(dst) {
        if dst_meta.len() == src_meta.len() && modified(&dst_meta) >= modified(&src_meta) {
            return Ok(None);
        }
    }
    copy_atomically(src, dst, &src_meta).map(Some)
}

fn copy_if_missing(src: &Path, dst: &Path) -> io::Result<bool> {
    if dst.exists() {
        return Ok(false);
    }
    let src_meta = fs::metadata(src)?;
    copy_atomically(src, dst, &src_meta).map(|_| true)
}

fn copy_tree_if_changed(src: &Path, dst: &Path, depth: usize) -> io::Result<bool> {
    copy_tree(src, dst, depth, &copy_if_changed_flag)
}

fn copy_tree_if_missing(src: &Path, dst: &Path, depth: usize) -> io::Result<bool> {
    copy_tree(src, dst, depth, &copy_if_missing)
}

fn copy_if_changed_flag(src: &Path, dst: &Path) -> io::Result<bool> {
    copy_if_changed(src, dst).map(|copied| copied.is_some())
}

/// Copy regular files below `src` into `dst` with `copy_file`; symlinks and
/// special files are skipped. Returns whether anything was copied.
fn copy_tree(
    src: &Path,
    dst: &Path,
    depth: usize,
    copy_file: &dyn Fn(&Path, &Path) -> io::Result<bool>,
) -> io::Result<bool> {
    if depth > MAX_SIDE_DEPTH {
        return Ok(false);
    }
    let mut changed = false;
    for entry in fs::read_dir(src)?.filter_map(Result::ok) {
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            changed |= copy_tree(&entry.path(), &target, depth + 1, copy_file)?;
        } else if file_type.is_file() {
            changed |= copy_file(&entry.path(), &target)?;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::AgentSessionRef;
    use std::time::Duration;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "herdr-agent-transcripts-{name}-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn claude_session(id: &str, transcript_path: Option<PathBuf>) -> PersistedAgentSession {
        PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(id).unwrap(),
            transcript_path,
        }
    }

    fn native_fixture(home: &Path, id: &str) -> NativeTranscript {
        let project = home.join(".claude").join("projects").join("-tmp-project");
        let file = project.join(format!("{id}.jsonl"));
        fs::create_dir_all(&project).unwrap();
        fs::write(&file, "{\"type\":\"user\"}\n").unwrap();
        let side_dir = project.join(id);
        fs::create_dir_all(side_dir.join("subagents")).unwrap();
        fs::write(side_dir.join("subagents").join("agent-1.jsonl"), "sub\n").unwrap();
        fs::write(side_dir.join("tool-results.json"), "{}").unwrap();
        NativeTranscript {
            file,
            side_dir: Some(side_dir),
        }
    }

    fn request(session: PersistedAgentSession) -> TranscriptBackupRequest {
        TranscriptBackupRequest {
            session,
            cwd: Some(PathBuf::from("/tmp/project")),
            label: Some("reviewer".into()),
        }
    }

    #[test]
    fn backup_copies_transcript_side_data_and_writes_meta() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        let native = native_fixture(home.path(), "session-1");
        let session = claude_session("session-1", Some(native.file.clone()));

        let outcome = backup_session_from(store.path(), &request(session), Some(&native)).unwrap();
        assert_eq!(
            outcome,
            BackupOutcome::Updated {
                bytes: "{\"type\":\"user\"}\n".len() as u64
            }
        );

        let dir = store.path().join("claude").join("session-1");
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_FILE)).unwrap(),
            "{\"type\":\"user\"}\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join(SIDE_DIR).join("subagents").join("agent-1.jsonl")).unwrap(),
            "sub\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join(SIDE_DIR).join("tool-results.json")).unwrap(),
            "{}"
        );
        let meta: TranscriptBackupMeta =
            serde_json::from_str(&fs::read_to_string(dir.join(META_FILE)).unwrap()).unwrap();
        assert_eq!(meta.source, "herdr:claude");
        assert_eq!(meta.agent, "claude");
        assert_eq!(meta.session_id, "session-1");
        assert_eq!(meta.original_path, native.file);
        assert_eq!(meta.cwd.as_deref(), Some(Path::new("/tmp/project")));
        assert_eq!(meta.label.as_deref(), Some("reviewer"));
        assert_eq!(meta.bytes, "{\"type\":\"user\"}\n".len() as u64);
        assert!(meta.backed_up_at.contains('T'), "{}", meta.backed_up_at);
        assert!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp-")),
            "temporary files were left behind"
        );
    }

    #[test]
    fn backup_skips_unchanged_sources_and_recopies_changed_ones() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        let native = native_fixture(home.path(), "session-2");
        let session = claude_session("session-2", Some(native.file.clone()));
        let request = request(session);

        assert!(matches!(
            backup_session_from(store.path(), &request, Some(&native)).unwrap(),
            BackupOutcome::Updated { .. }
        ));
        assert_eq!(
            backup_session_from(store.path(), &request, Some(&native)).unwrap(),
            BackupOutcome::Unchanged
        );

        // Same size, newer mtime: copied again.
        let newer = SystemTime::now() + Duration::from_secs(60);
        fs::write(&native.file, "{\"type\":\"aser\"}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&native.file)
            .unwrap()
            .set_modified(newer)
            .unwrap();
        assert!(matches!(
            backup_session_from(store.path(), &request, Some(&native)).unwrap(),
            BackupOutcome::Updated { .. }
        ));
        let dir = store.path().join("claude").join("session-2");
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_FILE)).unwrap(),
            "{\"type\":\"aser\"}\n"
        );

        // Different size, older mtime: copied again.
        fs::write(&native.file, "{}\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&native.file)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            backup_session_from(store.path(), &request, Some(&native)).unwrap(),
            BackupOutcome::Updated { bytes: 3 }
        );
        assert_eq!(
            fs::read_to_string(dir.join(TRANSCRIPT_FILE)).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn backup_reports_missing_native_transcript_and_unsupported_sessions() {
        let store = TempDir::new("store");
        let missing = NativeTranscript {
            file: store.path().join("nowhere").join("session-3.jsonl"),
            side_dir: None,
        };
        let session = claude_session("session-3", Some(missing.file.clone()));
        assert_eq!(
            backup_session_from(store.path(), &request(session.clone()), Some(&missing)).unwrap(),
            BackupOutcome::NoNativeTranscript
        );
        assert_eq!(
            backup_session_from(store.path(), &request(session), None).unwrap(),
            BackupOutcome::NotSupported
        );
        let codex = PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: AgentSessionRef::id("codex-session").unwrap(),
            transcript_path: None,
        };
        assert_eq!(
            backup_session(store.path(), &request(codex)).unwrap(),
            BackupOutcome::NotSupported
        );
        assert!(!store.path().join("claude").exists());
    }

    #[test]
    fn unsafe_session_ids_never_escape_the_store() {
        let store = TempDir::new("store");
        let home = TempDir::new("home");
        let native = native_fixture(home.path(), "session-4");
        for id in ["../escape", "..", "a/b", ".hidden"] {
            let session = PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: AgentSessionRef::id(id).unwrap(),
                transcript_path: Some(native.file.clone()),
            };
            assert_eq!(
                backup_session_from(store.path(), &request(session.clone()), Some(&native))
                    .unwrap(),
                BackupOutcome::NotSupported,
                "{id}"
            );
            assert_eq!(
                restore_if_missing_from(&session, store.path(), Some(&native)).unwrap(),
                RestoreOutcome::NotSupported,
                "{id}"
            );
        }
        assert!(fs::read_dir(store.path()).unwrap().next().is_none());
    }

    #[test]
    fn restore_puts_transcript_and_side_data_back_only_when_missing() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        let native = native_fixture(home.path(), "session-5");
        let session = claude_session("session-5", Some(native.file.clone()));
        backup_session_from(store.path(), &request(session.clone()), Some(&native)).unwrap();

        // Native file present: untouched.
        fs::write(&native.file, "live\n").unwrap();
        assert_eq!(
            restore_if_missing_from(&session, store.path(), Some(&native)).unwrap(),
            RestoreOutcome::NativePresent
        );
        assert_eq!(fs::read_to_string(&native.file).unwrap(), "live\n");

        // The agent deleted its files: the backup comes back.
        fs::remove_dir_all(native.file.parent().unwrap()).unwrap();
        assert_eq!(
            restore_if_missing_from(&session, store.path(), Some(&native)).unwrap(),
            RestoreOutcome::Restored {
                path: native.file.clone()
            }
        );
        assert_eq!(
            fs::read_to_string(&native.file).unwrap(),
            "{\"type\":\"user\"}\n"
        );
        let side_dir = native.side_dir.clone().unwrap();
        assert_eq!(
            fs::read_to_string(side_dir.join("subagents").join("agent-1.jsonl")).unwrap(),
            "sub\n"
        );
        assert_eq!(
            fs::read_to_string(side_dir.join("tool-results.json")).unwrap(),
            "{}"
        );
        // The backup stays in place.
        assert!(store
            .path()
            .join("claude")
            .join("session-5")
            .join(TRANSCRIPT_FILE)
            .is_file());
    }

    #[test]
    fn restore_uses_recorded_original_path_when_locations_are_unknown() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        let native = native_fixture(home.path(), "session-6");
        let session = claude_session("session-6", None);
        backup_session_from(store.path(), &request(session.clone()), Some(&native)).unwrap();
        fs::remove_dir_all(native.file.parent().unwrap()).unwrap();

        assert_eq!(
            restore_if_missing_from(&session, store.path(), None).unwrap(),
            RestoreOutcome::Restored {
                path: native.file.clone()
            }
        );
        assert!(native.file.is_file());
    }

    #[test]
    fn restore_without_backup_does_nothing() {
        let store = TempDir::new("store");
        let native = NativeTranscript {
            file: store.path().join("elsewhere").join("session-7.jsonl"),
            side_dir: None,
        };
        let session = claude_session("session-7", Some(native.file.clone()));
        assert_eq!(
            restore_if_missing_from(&session, store.path(), Some(&native)).unwrap(),
            RestoreOutcome::NoBackup
        );
        assert!(!native.file.exists());
    }

    #[test]
    fn listing_reports_every_backup_and_native_presence() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        assert!(list_backups(store.path()).unwrap().is_empty());
        assert!(list_backups(&store.path().join("missing"))
            .unwrap()
            .is_empty());

        let present = native_fixture(home.path(), "session-b");
        let gone = native_fixture(home.path(), "session-a");
        for native in [&present, &gone] {
            let id = native
                .file
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let session = claude_session(&id, Some(native.file.clone()));
            backup_session_from(store.path(), &request(session), Some(native)).unwrap();
        }
        fs::remove_file(&gone.file).unwrap();

        let entries = list_backups(store.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agent, "claude");
        assert_eq!(entries[0].session_id, "session-a");
        assert!(!entries[0].native_present);
        assert_eq!(entries[1].session_id, "session-b");
        assert!(entries[1].native_present);
        assert_eq!(entries[1].original_path, present.file);
        assert_eq!(entries[1].bytes, "{\"type\":\"user\"}\n".len() as u64);
        assert!(!entries[1].backed_up_at.is_empty());
    }

    #[test]
    fn sync_counts_outcomes_without_stopping_on_failures() {
        let home = TempDir::new("home");
        let store = TempDir::new("store");
        let native = native_fixture(home.path(), "session-8");
        let requests = vec![
            request(claude_session("session-8", Some(native.file.clone()))),
            request(claude_session(
                "session-9",
                Some(store.path().join("missing.jsonl")),
            )),
            request(PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: AgentSessionRef::id("codex-session").unwrap(),
                transcript_path: None,
            }),
        ];
        let summary = sync_backups(store.path(), &requests);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.skipped, 2);
        assert_eq!(summary.failed, 0);
        let summary = sync_backups(store.path(), &requests);
        assert_eq!(summary.unchanged, 1);
    }
}
