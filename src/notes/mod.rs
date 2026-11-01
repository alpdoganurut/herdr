//! Notes (fork): the per-session markdown page behind the info pane, the
//! user's page (agents write it only when the user asks). Plain file I/O
//! under `<config_dir>/notes/`; no `App`.
//!
//! Each key has up to two files: `<key>.md` (the notes) and `<key>.meta.json`
//! (who wrote last, when, and the key the pane used before). Older builds
//! also kept `<key>.checkpoints.jsonl`; such files are left alone and never
//! read. Every write is a temp file
//! in the same directory renamed over the target, so a reader never sees a
//! partial file.

pub(crate) mod bounded;
pub(crate) mod recall;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_resume::{is_safe_path_component, AgentSessionRef};
use crate::api::schema::notes::{NotesAuthor, NotesWriteOutcome};

pub(crate) use bounded::BoundedMap;

/// The largest notes file served or written.
pub(crate) const MAX_NOTES_BYTES: u64 = 256 * 1024;
/// The longest valid key.
pub(crate) const MAX_KEY_LEN: usize = 128;
/// The revision of notes that do not exist yet.
pub(crate) const NO_REVISION: &str = "none";
/// Cached notes files.
const NOTES_CACHE_CAPACITY: usize = 64;

/// `<config_dir>/notes`.
pub(crate) fn notes_dir() -> PathBuf {
    crate::config::config_dir().join("notes")
}

/// Whether `key` is a valid notes key: `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
/// Keys are case-preserving and never lowercased.
pub(crate) fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= MAX_KEY_LEN
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The first 16 hex digits of `sha256(value)`.
fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether `id` is the `h<16 hex>` form of a hashed key.
fn is_hashed_id(id: &str) -> bool {
    id.len() == 17 && id.starts_with('h') && id[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// The agent label as a key prefix (`agent` when it is not a safe component).
fn agent_prefix(agent: &str) -> &str {
    if is_safe_path_component(agent) && agent.len() <= 32 {
        agent
    } else {
        "agent"
    }
}

/// The notes key of an agent session: `<agent>-<id>` when the id is a safe
/// path component and the key stays valid, else `<agent>-h<16 hex>` of the
/// session value (path refs, very long ids).
pub(crate) fn session_key(agent: &str, session: &AgentSessionRef) -> String {
    let agent = agent_prefix(agent);
    let value = session.value.as_str();
    if is_safe_path_component(value) && !is_hashed_id(value) {
        let key = format!("{agent}-{value}");
        if is_valid_key(&key) {
            return key;
        }
    }
    format!("{agent}-h{}", short_hash(value))
}

/// The notes key of a tab without an agent: `tab-<tab id with ':' → '-'>`,
/// else `tab-h<16 hex>`.
pub(crate) fn tab_key(tab_id: &str) -> String {
    let key = format!("tab-{}", tab_id.replace(':', "-"));
    if is_valid_key(&key) {
        key
    } else {
        format!("tab-h{}", short_hash(tab_id))
    }
}

/// The agent and session id a plain `<agent>-<id>` key names, for the agents
/// whose transcripts herdr reads (a hashed key cannot be reversed).
pub(crate) fn key_agent_session(key: &str) -> Option<(&str, &str)> {
    let (agent, id) = key.split_once('-')?;
    (matches!(agent, "claude" | "codex") && !id.is_empty() && !is_hashed_id(id))
        .then_some((agent, id))
}

/// `sha256:<first 12 hex>` of the text.
pub(crate) fn revision_of(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// Unix seconds now.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The local `HH:MM` for an appended stamp (`None` without a local clock).
pub(crate) fn local_hhmm() -> Option<String> {
    crate::platform::local_datetime()
        .map(|local| format!("{:02}:{:02}", local.hour(), local.minute()))
}

/// A store failure, mapped to an API error by the handlers.
#[derive(Debug)]
pub(crate) enum NotesError {
    /// A malformed key or argument.
    Invalid(String),
    /// Over a size cap.
    TooLarge(String),
    Io(io::Error),
}

impl NotesError {
    pub(crate) fn code(&self) -> &'static str {
        use crate::api::schema::notes::error_code;
        match self {
            Self::Invalid(_) => error_code::INVALID_PARAMS,
            Self::TooLarge(_) => error_code::TOO_LARGE,
            Self::Io(_) => error_code::IO,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Invalid(message) | Self::TooLarge(message) => message.clone(),
            Self::Io(err) => format!("notes I/O failed: {err}"),
        }
    }
}

impl From<io::Error> for NotesError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// `<key>.meta.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NotesMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<NotesAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    /// The key the pane used before (written on the first write only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// What a file's metadata says about its content, for the read caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
}

impl FileStamp {
    pub(crate) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            mtime: metadata.modified().ok(),
            len: metadata.len(),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedNotes {
    stamp: FileStamp,
    text: String,
    revision: String,
    meta: NotesMeta,
}

/// Notes as read. `text` is `None` when the caller already has the
/// revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotesSnapshot {
    pub path: PathBuf,
    pub revision: String,
    pub exists: bool,
    pub bytes: u64,
    pub text: Option<String>,
    pub meta: NotesMeta,
}

/// The current notes of one key, read fresh.
struct Current {
    text: String,
    revision: String,
    exists: bool,
    meta: NotesMeta,
}

/// One write's inputs besides the text.
struct WriteMeta<'a> {
    previous_meta: NotesMeta,
    existed: bool,
    author: NotesAuthor,
    previous_key: Option<&'a str>,
    now: u64,
}

/// Who writes and when, for [`NotesStore::set`] and [`NotesStore::append`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteBy<'a> {
    pub author: NotesAuthor,
    /// The key the pane used before; recorded in the meta on the first
    /// write.
    pub previous_key: Option<&'a str>,
    /// Unix seconds.
    pub now: u64,
}

/// The notes files, with a small read cache.
#[derive(Debug)]
pub(crate) struct NotesStore {
    dir: PathBuf,
    cache: BoundedMap<String, CachedNotes>,
}

impl NotesStore {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cache: BoundedMap::new(NOTES_CACHE_CAPACITY),
        }
    }

    pub(crate) fn notes_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.md"))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.meta.json"))
    }

    pub(crate) fn check_key(key: &str) -> Result<(), NotesError> {
        if is_valid_key(key) {
            Ok(())
        } else {
            Err(NotesError::Invalid(format!("invalid notes key {key:?}")))
        }
    }

    fn read_meta(&self, key: &str) -> NotesMeta {
        fs::read(self.meta_path(key))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The notes of `key`; one stat decides whether the cached text is still
    /// current. A same-size rewrite within the filesystem's mtime granularity
    /// can be served stale until the next change; writes always re-read.
    pub(crate) fn get(
        &mut self,
        key: &str,
        known_revision: Option<&str>,
    ) -> Result<NotesSnapshot, NotesError> {
        Self::check_key(key)?;
        let path = self.notes_path(key);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.cache.remove(key);
                let unchanged = known_revision == Some(NO_REVISION);
                return Ok(NotesSnapshot {
                    path,
                    revision: NO_REVISION.into(),
                    exists: false,
                    bytes: 0,
                    text: (!unchanged).then(String::new),
                    meta: NotesMeta::default(),
                });
            }
            Err(err) => return Err(err.into()),
        };
        if metadata.len() > MAX_NOTES_BYTES {
            return Err(too_large(&path));
        }
        let stamp = FileStamp::of(&metadata);
        let cached = self
            .cache
            .get(key)
            .filter(|cached| cached.stamp == stamp)
            .cloned();
        let cached = match cached {
            Some(cached) => cached,
            None => {
                let text = read_text(&path)?;
                let cached = CachedNotes {
                    stamp,
                    revision: revision_of(&text),
                    text,
                    meta: self.read_meta(key),
                };
                self.cache.insert(key.to_owned(), cached.clone());
                cached
            }
        };
        let unchanged = known_revision == Some(cached.revision.as_str());
        Ok(NotesSnapshot {
            path,
            revision: cached.revision,
            exists: true,
            bytes: stamp.len,
            text: (!unchanged).then_some(cached.text),
            meta: cached.meta,
        })
    }

    /// The current notes, read fresh (writes never trust the cache).
    fn current(&self, key: &str) -> Result<Current, NotesError> {
        let path = self.notes_path(key);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_NOTES_BYTES => Err(too_large(&path)),
            Ok(_) => {
                let text = read_text(&path)?;
                Ok(Current {
                    revision: revision_of(&text),
                    text,
                    exists: true,
                    meta: self.read_meta(key),
                })
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Current {
                text: String::new(),
                revision: NO_REVISION.into(),
                exists: false,
                meta: NotesMeta::default(),
            }),
            Err(err) => Err(err.into()),
        }
    }

    fn snapshot_of(&self, key: &str, current: Current) -> NotesSnapshot {
        NotesSnapshot {
            path: self.notes_path(key),
            bytes: current.text.len() as u64,
            revision: current.revision,
            exists: current.exists,
            text: Some(current.text),
            meta: current.meta,
        }
    }

    /// Write `text` and the meta, and refresh the cache.
    fn write(
        &mut self,
        key: &str,
        text: &str,
        by: WriteMeta<'_>,
    ) -> Result<NotesSnapshot, NotesError> {
        let path = self.notes_path(key);
        if text.len() as u64 > MAX_NOTES_BYTES {
            return Err(too_large(&path));
        }
        fs::create_dir_all(&self.dir)?;
        write_atomic(&path, text.as_bytes())?;
        let meta = NotesMeta {
            updated_by: Some(by.author),
            updated_at: Some(by.now),
            previous: if by.existed {
                by.previous_meta.previous
            } else {
                by.previous_meta
                    .previous
                    .or_else(|| by.previous_key.map(str::to_owned))
            },
        };
        match serde_json::to_vec(&meta) {
            Ok(bytes) => {
                if let Err(err) = write_atomic(&self.meta_path(key), &bytes) {
                    tracing::warn!(key, err = %err, "notes: cannot write the meta file");
                }
            }
            Err(err) => tracing::warn!(key, err = %err, "notes: cannot encode the meta file"),
        }
        let revision = revision_of(text);
        match fs::metadata(&path) {
            Ok(metadata) => {
                self.cache.insert(
                    key.to_owned(),
                    CachedNotes {
                        stamp: FileStamp::of(&metadata),
                        text: text.to_owned(),
                        revision: revision.clone(),
                        meta: meta.clone(),
                    },
                );
            }
            Err(_) => {
                self.cache.remove(key);
            }
        }
        Ok(NotesSnapshot {
            path,
            revision,
            exists: true,
            bytes: text.len() as u64,
            text: Some(text.to_owned()),
            meta,
        })
    }

    /// Replace the notes when `base_revision` is current (`None` and `none`
    /// only match missing notes). Text equal to the current text is
    /// `unchanged` whatever the base. A mismatch is a `conflict` carrying the
    /// current notes; nothing is written.
    pub(crate) fn set(
        &mut self,
        key: &str,
        text: &str,
        base_revision: Option<&str>,
        by: WriteBy<'_>,
    ) -> Result<(NotesWriteOutcome, NotesSnapshot), NotesError> {
        Self::check_key(key)?;
        if text.len() as u64 > MAX_NOTES_BYTES {
            return Err(too_large(&self.notes_path(key)));
        }
        let current = self.current(key)?;
        if current.text == text {
            return Ok((NotesWriteOutcome::Unchanged, self.snapshot_of(key, current)));
        }
        if base_revision.unwrap_or(NO_REVISION) != current.revision {
            return Ok((NotesWriteOutcome::Conflict, self.snapshot_of(key, current)));
        }
        let snapshot = self.write(
            key,
            text,
            WriteMeta {
                previous_meta: current.meta,
                existed: current.exists,
                author: by.author,
                previous_key: by.previous_key,
                now: by.now,
            },
        )?;
        Ok((NotesWriteOutcome::Written, snapshot))
    }

    /// Add `text` at the end, or at the end of `## <section>` (the heading is
    /// created at the end when missing). `stamp` (`HH:MM`) prefixes the text
    /// with `- HH:MM `. Never conflicts.
    pub(crate) fn append(
        &mut self,
        key: &str,
        text: &str,
        section: Option<&str>,
        stamp: Option<&str>,
        by: WriteBy<'_>,
    ) -> Result<(NotesWriteOutcome, NotesSnapshot), NotesError> {
        Self::check_key(key)?;
        let section = section.map(str::trim).filter(|section| !section.is_empty());
        if section.is_some_and(|section| section.contains('\n')) {
            return Err(NotesError::Invalid("section must be one line".into()));
        }
        let text = text.trim_end_matches(['\n', '\r']);
        if text.trim().is_empty() {
            return Err(NotesError::Invalid("nothing to append".into()));
        }
        let entry = match stamp {
            Some(stamp) => format!("- {stamp} {text}"),
            None => text.to_owned(),
        };
        let current = self.current(key)?;
        let updated = append_text(&current.text, &entry, section);
        let snapshot = self.write(
            key,
            &updated,
            WriteMeta {
                previous_meta: current.meta,
                existed: current.exists,
                author: by.author,
                previous_key: by.previous_key,
                now: by.now,
            },
        )?;
        Ok((NotesWriteOutcome::Written, snapshot))
    }

    /// Copy `from`'s notes to `to` when `to` has none (the tab's notes when
    /// the tab starts running an agent). Never overwrites; returns whether a
    /// copy was made.
    pub(crate) fn migrate(&mut self, from: &str, to: &str) -> Result<bool, NotesError> {
        Self::check_key(from)?;
        Self::check_key(to)?;
        let target = self.notes_path(to);
        if target.exists() {
            return Ok(false);
        }
        let source = self.notes_path(from);
        let text = match fs::metadata(&source) {
            Ok(metadata) if metadata.len() > MAX_NOTES_BYTES => return Err(too_large(&source)),
            Ok(_) => read_text(&source)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        fs::create_dir_all(&self.dir)?;
        write_atomic(&target, text.as_bytes())?;
        let meta = NotesMeta {
            previous: Some(from.to_owned()),
            ..self.read_meta(from)
        };
        if let Ok(bytes) = serde_json::to_vec(&meta) {
            if let Err(err) = write_atomic(&self.meta_path(to), &bytes) {
                tracing::warn!(key = to, err = %err, "notes: cannot write the meta file");
            }
        }
        self.cache.remove(to);
        tracing::info!(
            from,
            to,
            "notes: copied the tab's notes to the agent session's notes"
        );
        Ok(true)
    }
}

fn too_large(path: &Path) -> NotesError {
    NotesError::TooLarge(format!(
        "notes too large to show ({}; at most {} KiB)",
        path.display(),
        MAX_NOTES_BYTES / 1024
    ))
}

fn read_text(path: &Path) -> Result<String, NotesError> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Write through a temp file in the same directory, then rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("notes");
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let result = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `entry` appended to `text`: at the end, or as the last line of
/// `## <section>` (before the next `## ` heading, after the section's last
/// non-blank line), creating the heading at the end when it is missing.
pub(crate) fn append_text(text: &str, entry: &str, section: Option<&str>) -> String {
    let Some(section) = section else {
        let mut out = text.to_owned();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(entry);
        out.push('\n');
        return out;
    };
    let heading = format!("## {section}");
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(start) = lines
        .iter()
        .position(|line| line.trim_end() == heading.as_str())
    else {
        let mut out = text.to_owned();
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
        }
        out.push_str(&heading);
        out.push('\n');
        out.push_str(entry);
        out.push('\n');
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|line| line.starts_with("## "))
        .map_or(lines.len(), |offset| start + 1 + offset);
    // After the section's last non-blank line (the heading at least).
    let insert_at = (start + 1..end)
        .rev()
        .find(|&index| !lines[index].trim().is_empty())
        .map_or(start + 1, |index| index + 1);
    let mut out = String::with_capacity(text.len() + entry.len() + 2);
    for line in &lines[..insert_at] {
        out.push_str(line);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(entry);
    out.push('\n');
    for line in &lines[insert_at..] {
        out.push_str(line);
    }
    out
}

/// The pane → key memory behind `previous` (a pane whose session changed,
/// for example after `/clear`), keyed by public pane id.
#[derive(Debug, Default)]
pub(crate) struct PaneKeys {
    keys: HashMap<String, (String, Option<String>)>,
}

impl PaneKeys {
    /// Record that `pane_id` now resolves to `key`; returns the key it used
    /// before, for as long as it keeps this key.
    pub(crate) fn observe(&mut self, pane_id: &str, key: &str) -> Option<String> {
        match self.keys.get_mut(pane_id) {
            Some((current, previous)) if current == key => previous.clone(),
            Some((current, previous)) => {
                let old = std::mem::replace(current, key.to_owned());
                *previous = Some(old);
                previous.clone()
            }
            None => {
                self.keys.insert(pane_id.to_owned(), (key.to_owned(), None));
                None
            }
        }
    }

    /// Forget panes that no longer exist.
    pub(crate) fn retain(&mut self, mut live: impl FnMut(&str) -> bool) {
        self.keys.retain(|pane_id, _| live(pane_id));
    }

    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }
}

/// The server's notes runtime (`App.notes`): the store and the pane → key
/// memory.
#[derive(Debug)]
pub(crate) struct NotesRuntime {
    /// `[notes] enabled`.
    pub enabled: bool,
    pub store: NotesStore,
    pub panes: PaneKeys,
    /// The latest `notes.reveal`s (`endpoint.notes-reveal.v1`), oldest
    /// first, at most [`REVEAL_CAP`]; never persisted.
    pub reveals: std::collections::VecDeque<NotesReveal>,
}

/// The most `notes.reveal`s kept for a render pass to send.
pub(crate) const REVEAL_CAP: usize = 16;

/// One `notes.reveal`: its revision (1 and up, per server run) and the
/// public tab id whose info pane the clients show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotesReveal {
    pub revision: u64,
    pub tab_id: String,
}

impl Default for NotesRuntime {
    fn default() -> Self {
        Self::new(true, notes_dir())
    }
}

impl NotesRuntime {
    pub(crate) fn new(enabled: bool, dir: PathBuf) -> Self {
        Self {
            enabled,
            store: NotesStore::new(dir),
            panes: PaneKeys::default(),
            reveals: std::collections::VecDeque::new(),
        }
    }

    /// Record a reveal of `tab_id` under the next revision.
    pub(crate) fn note_reveal(&mut self, tab_id: String) -> u64 {
        let revision = self
            .reveals
            .back()
            .map_or(1, |reveal| reveal.revision.wrapping_add(1).max(1));
        if self.reveals.len() >= REVEAL_CAP {
            self.reveals.pop_front();
        }
        self.reveals.push_back(NotesReveal { revision, tab_id });
        revision
    }
}
