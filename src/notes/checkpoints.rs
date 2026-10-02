//! The checkpoint store (fork): an append-only JSONL file per notes key,
//! `<key>.checkpoints.jsonl`.
//!
//! Every change is one record: `add` and `update` carry the whole
//! checkpoint (an `update` without an anchor keeps the anchor), `remove` is a
//! tombstone and `anchor` patches only the transcript anchor (the off-thread
//! capture). Folding keeps the last record per id; the file is never
//! rewritten. Its length is the change stamp (`seq`), so an unchanged list
//! costs one stat.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{BoundedMap, FileStamp, NotesError};
use crate::api::schema::notes::{
    CheckpointInfo, CheckpointKind, CheckpointWriteInfo, CheckpointsListInfo, NotesAuthor,
};

/// Parsed checkpoint files kept in memory.
const PARSE_CACHE_CAPACITY: usize = 64;
/// Files larger than this parse only their last this many bytes.
pub(crate) const MAX_PARSE_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_TITLE_CHARS: usize = 120;
pub(crate) const MAX_DETAIL_CHARS: usize = 2000;
pub(crate) const MAX_TAGS: usize = 8;
pub(crate) const MAX_TAG_CHARS: usize = 32;
/// Adds per key per rolling hour.
pub(crate) const MAX_ADDS_PER_HOUR: usize = 60;
/// An add with the same kind, title and author as a checkpoint this recent
/// (seconds) updates that checkpoint instead.
pub(crate) const FOLD_WINDOW_SECS: u64 = 120;
/// `checkpoints.list` default limit.
pub(crate) const DEFAULT_LIST_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Op {
    Add,
    Update,
    Remove,
    Anchor,
    #[serde(other)]
    Unknown,
}

/// Where in the agent's transcript a checkpoint was made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AnchorRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

impl AnchorRecord {
    pub(crate) fn from_anchor(anchor: &super::transcript::Anchor) -> Self {
        Self {
            path: anchor
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            offset: anchor.offset,
            uuid: anchor.uuid.clone(),
        }
    }

    pub(crate) fn to_anchor(&self) -> super::transcript::Anchor {
        super::transcript::Anchor {
            path: self.path.as_ref().map(PathBuf::from),
            offset: self.offset,
            uuid: self.uuid.clone(),
        }
    }

    /// Whether the anchor can locate anything.
    pub(crate) fn is_usable(&self) -> bool {
        self.path.is_some() || self.offset.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    v: u32,
    op: Op,
    id: String,
    ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<CheckpointKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<NotesAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor: Option<AnchorRecord>,
}

impl Record {
    fn bare(op: Op, id: &str, ts: u64) -> Self {
        Self {
            v: 1,
            op,
            id: id.to_owned(),
            ts,
            kind: None,
            author: None,
            title: None,
            detail: None,
            tags: None,
            anchor: None,
        }
    }
}

/// One folded checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub id: String,
    /// Unix seconds of the add.
    pub ts: u64,
    pub kind: CheckpointKind,
    pub author: NotesAuthor,
    pub title: String,
    pub detail: Option<String>,
    pub tags: Vec<String>,
    pub anchor: Option<AnchorRecord>,
}

impl Checkpoint {
    pub(crate) fn info(&self) -> CheckpointInfo {
        CheckpointInfo {
            id: self.id.clone(),
            ts: self.ts,
            kind: self.kind,
            author: self.author,
            title: self.title.clone(),
            detail: self.detail.clone(),
            tags: self.tags.clone(),
            has_context: self.anchor.as_ref().is_some_and(AnchorRecord::is_usable),
        }
    }

    fn record(&self, op: Op, ts: u64) -> Record {
        Record {
            kind: Some(self.kind),
            author: Some(self.author),
            title: Some(self.title.clone()),
            detail: self.detail.clone(),
            tags: Some(self.tags.clone()),
            anchor: self.anchor.clone(),
            ..Record::bare(op, &self.id, ts)
        }
    }
}

/// A new checkpoint's content.
#[derive(Debug, Clone)]
pub(crate) struct NewCheckpoint {
    pub kind: CheckpointKind,
    pub author: NotesAuthor,
    pub title: String,
    pub detail: Option<String>,
    pub tags: Vec<String>,
    pub anchor: Option<AnchorRecord>,
}

/// The fields `checkpoints.update` replaces (`None` keeps the stored one).
#[derive(Debug, Clone, Default)]
pub(crate) struct CheckpointPatch {
    pub kind: Option<CheckpointKind>,
    pub title: Option<String>,
    pub detail: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct Parsed {
    stamp: FileStamp,
    /// Folded, ordered by `(ts, id)`.
    checkpoints: Vec<Checkpoint>,
    /// Unix seconds of every `add` record, for the rate limit.
    add_times: Vec<u64>,
}

/// The checkpoint files, with a parse cache keyed by `(len, mtime)`.
#[derive(Debug)]
pub(crate) struct CheckpointStore {
    dir: PathBuf,
    cache: BoundedMap<String, Parsed>,
    last_id_ms: u64,
    id_counter: u64,
}

impl CheckpointStore {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            cache: BoundedMap::new(PARSE_CACHE_CAPACITY),
            last_id_ms: 0,
            id_counter: 0,
        }
    }

    pub(crate) fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.checkpoints.jsonl"))
    }

    /// The change stamp: the file's length (0 while it does not exist).
    pub(crate) fn seq(&self, key: &str) -> Result<u64, NotesError> {
        match fs::metadata(self.path(key)) {
            Ok(metadata) => Ok(metadata.len()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(err.into()),
        }
    }

    /// The folded checkpoints of `key`, parsed again only when the file's
    /// length or mtime changed.
    fn parsed(&mut self, key: &str) -> Result<Parsed, NotesError> {
        super::NotesStore::check_key(key)?;
        let path = self.path(key);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.cache.remove(key);
                return Ok(Parsed {
                    stamp: FileStamp {
                        mtime: None,
                        len: 0,
                    },
                    checkpoints: Vec::new(),
                    add_times: Vec::new(),
                });
            }
            Err(err) => return Err(err.into()),
        };
        let stamp = FileStamp::of(&metadata);
        if let Some(parsed) = self.cache.get(key).filter(|parsed| parsed.stamp == stamp) {
            return Ok(parsed.clone());
        }
        let bytes = read_tail(&path, stamp.len)?;
        let (checkpoints, add_times) = fold(&bytes);
        let parsed = Parsed {
            stamp,
            checkpoints,
            add_times,
        };
        self.cache.insert(key.to_owned(), parsed.clone());
        Ok(parsed)
    }

    pub(crate) fn find(&mut self, key: &str, id: &str) -> Result<Option<Checkpoint>, NotesError> {
        Ok(self
            .parsed(key)?
            .checkpoints
            .into_iter()
            .find(|checkpoint| checkpoint.id == id))
    }

    /// `checkpoints.list`: `unchanged` (one stat) when `since_seq` is the
    /// current stamp; otherwise the newest `limit` of the given kinds, oldest
    /// first.
    pub(crate) fn list(
        &mut self,
        key: &str,
        kinds: &[CheckpointKind],
        since_seq: Option<u64>,
        limit: Option<u32>,
    ) -> Result<CheckpointsListInfo, NotesError> {
        super::NotesStore::check_key(key)?;
        let seq = self.seq(key)?;
        if since_seq == Some(seq) {
            return Ok(CheckpointsListInfo {
                key: key.to_owned(),
                seq,
                unchanged: true,
                checkpoints: Vec::new(),
                started_at: None,
            });
        }
        let parsed = self.parsed(key)?;
        let started_at = parsed.checkpoints.first().map(|checkpoint| checkpoint.ts);
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT) as usize;
        let matching: Vec<&Checkpoint> = parsed
            .checkpoints
            .iter()
            .filter(|checkpoint| kinds.is_empty() || kinds.contains(&checkpoint.kind))
            .collect();
        let skip = matching.len().saturating_sub(limit);
        Ok(CheckpointsListInfo {
            key: key.to_owned(),
            seq: parsed.stamp.len,
            unchanged: false,
            checkpoints: matching
                .into_iter()
                .skip(skip)
                .map(Checkpoint::info)
                .collect(),
            started_at,
        })
    }

    /// A new id: `cp_` + Crockford base32 of `ms << 12 | counter`, strictly
    /// increasing within this store.
    fn next_id(&mut self, now_ms: u64) -> String {
        if now_ms > self.last_id_ms {
            self.last_id_ms = now_ms;
            self.id_counter = 0;
        } else {
            self.id_counter += 1;
            if self.id_counter >= 1 << 12 {
                self.last_id_ms += 1;
                self.id_counter = 0;
            }
        }
        format!(
            "cp_{}",
            crockford_base32((self.last_id_ms << 12) | self.id_counter)
        )
    }

    fn append_record(&mut self, key: &str, record: &Record) -> Result<u64, NotesError> {
        let path = self.path(key);
        let mut line =
            serde_json::to_string(record).map_err(|err| NotesError::Io(io::Error::other(err)))?;
        line.push('\n');
        fs::create_dir_all(&self.dir)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;
        let len = file.metadata()?.len();
        if len > 0 {
            // Never glue a record onto a torn last line.
            let mut last = [0u8; 1];
            file.seek(SeekFrom::Start(len - 1))?;
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(file.metadata()?.len())
    }

    /// `checkpoints.add`. An add matching a checkpoint of the same kind,
    /// title and author from the last two minutes updates it (`folded`).
    pub(crate) fn add(
        &mut self,
        key: &str,
        new: NewCheckpoint,
        now_ms: u64,
    ) -> Result<CheckpointWriteInfo, NotesError> {
        if new.kind == CheckpointKind::Unknown {
            return Err(NotesError::Invalid("unknown checkpoint kind".into()));
        }
        let new = NewCheckpoint {
            title: clean_title(&new.title)?,
            detail: clean_detail(new.detail)?,
            tags: clean_tags(new.tags)?,
            ..new
        };
        let now = now_ms / 1000;
        let parsed = self.parsed(key)?;
        if let Some(existing) = parsed.checkpoints.iter().rev().find(|checkpoint| {
            checkpoint.kind == new.kind
                && checkpoint.author == new.author
                && checkpoint.title == new.title
                && now.saturating_sub(checkpoint.ts) <= FOLD_WINDOW_SECS
        }) {
            let mut updated = existing.clone();
            if new.detail.is_some() {
                updated.detail = new.detail;
            }
            if !new.tags.is_empty() {
                updated.tags = new.tags;
            }
            if updated.anchor.is_none() {
                updated.anchor = new.anchor;
            }
            let seq = self.append_record(key, &updated.record(Op::Update, now))?;
            return Ok(CheckpointWriteInfo {
                key: key.to_owned(),
                seq,
                checkpoint: Some(updated.info()),
                folded: true,
                removed: false,
            });
        }
        let hour_ago = now.saturating_sub(3600);
        let recent = parsed.add_times.iter().filter(|&&ts| ts > hour_ago).count();
        if recent >= MAX_ADDS_PER_HOUR {
            return Err(NotesError::RateLimited(format!(
                "at most {MAX_ADDS_PER_HOUR} checkpoints per hour for one session"
            )));
        }
        let checkpoint = Checkpoint {
            id: self.next_id(now_ms),
            ts: now,
            kind: new.kind,
            author: new.author,
            title: new.title,
            detail: new.detail,
            tags: new.tags,
            anchor: new.anchor,
        };
        let seq = self.append_record(key, &checkpoint.record(Op::Add, now))?;
        Ok(CheckpointWriteInfo {
            key: key.to_owned(),
            seq,
            checkpoint: Some(checkpoint.info()),
            folded: false,
            removed: false,
        })
    }

    /// `checkpoints.update`: the given fields replace the stored ones (an
    /// empty detail clears it).
    pub(crate) fn update(
        &mut self,
        key: &str,
        id: &str,
        patch: CheckpointPatch,
        now: u64,
    ) -> Result<CheckpointWriteInfo, NotesError> {
        let mut checkpoint = self.find(key, id)?.ok_or_else(|| not_found(id))?;
        if let Some(kind) = patch.kind {
            if kind == CheckpointKind::Unknown {
                return Err(NotesError::Invalid("unknown checkpoint kind".into()));
            }
            checkpoint.kind = kind;
        }
        if let Some(title) = patch.title {
            checkpoint.title = clean_title(&title)?;
        }
        if let Some(detail) = patch.detail {
            checkpoint.detail = clean_detail(Some(detail))?;
        }
        if let Some(tags) = patch.tags {
            checkpoint.tags = clean_tags(tags)?;
        }
        let seq = self.append_record(key, &checkpoint.record(Op::Update, now))?;
        Ok(CheckpointWriteInfo {
            key: key.to_owned(),
            seq,
            checkpoint: Some(checkpoint.info()),
            folded: false,
            removed: false,
        })
    }

    /// `checkpoints.remove`: a tombstone.
    pub(crate) fn remove(
        &mut self,
        key: &str,
        id: &str,
        now: u64,
    ) -> Result<CheckpointWriteInfo, NotesError> {
        let checkpoint = self.find(key, id)?.ok_or_else(|| not_found(id))?;
        let seq = self.append_record(key, &Record::bare(Op::Remove, id, now))?;
        Ok(CheckpointWriteInfo {
            key: key.to_owned(),
            seq,
            checkpoint: Some(checkpoint.info()),
            folded: false,
            removed: true,
        })
    }

    /// Patch only the anchor of `id` (the worker's late capture). A removed
    /// or unknown id is left alone; returns whether a record was written.
    pub(crate) fn set_anchor(
        &mut self,
        key: &str,
        id: &str,
        anchor: AnchorRecord,
        now: u64,
    ) -> Result<bool, NotesError> {
        if self.find(key, id)?.is_none() {
            return Ok(false);
        }
        self.append_record(
            key,
            &Record {
                anchor: Some(anchor),
                ..Record::bare(Op::Anchor, id, now)
            },
        )?;
        Ok(true)
    }
}

fn not_found(id: &str) -> NotesError {
    NotesError::NotFound(format!("no checkpoint {id:?}"))
}

/// The whole file, or its last [`MAX_PARSE_BYTES`] starting at the first
/// complete line.
fn read_tail(path: &Path, len: u64) -> Result<Vec<u8>, NotesError> {
    let mut file = fs::File::open(path)?;
    if len <= MAX_PARSE_BYTES {
        let mut bytes = Vec::with_capacity(len as usize);
        file.read_to_end(&mut bytes)?;
        return Ok(bytes);
    }
    file.seek(SeekFrom::Start(len - MAX_PARSE_BYTES))?;
    let mut bytes = Vec::with_capacity(MAX_PARSE_BYTES as usize);
    file.take(MAX_PARSE_BYTES).read_to_end(&mut bytes)?;
    let start = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |index| index + 1);
    Ok(bytes.split_off(start))
}

/// Fold the records: the last record per id wins, `remove` drops the id and
/// `anchor` patches only the anchor. Unreadable lines are skipped.
fn fold(bytes: &[u8]) -> (Vec<Checkpoint>, Vec<u64>) {
    let mut by_id: std::collections::HashMap<String, Checkpoint> = std::collections::HashMap::new();
    let mut add_times = Vec::new();
    for line in bytes.split(|&byte| byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record: Record = match serde_json::from_slice(line) {
            Ok(record) => record,
            Err(err) => {
                tracing::debug!(err = %err, "checkpoints: skipping an unreadable record");
                continue;
            }
        };
        match record.op {
            Op::Add | Op::Update => {
                if record.op == Op::Add {
                    add_times.push(record.ts);
                }
                let existing = by_id.get(&record.id);
                // An update keeps the add's time and, without one of its
                // own, the anchor.
                let ts = match (record.op, existing) {
                    (Op::Update, Some(existing)) => existing.ts,
                    _ => record.ts,
                };
                let anchor = record
                    .anchor
                    .or_else(|| existing.and_then(|existing| existing.anchor.clone()));
                let checkpoint = Checkpoint {
                    id: record.id.clone(),
                    ts,
                    kind: record.kind.unwrap_or(CheckpointKind::Note),
                    author: record.author.unwrap_or(NotesAuthor::Agent),
                    title: record.title.unwrap_or_default(),
                    detail: record.detail,
                    tags: record.tags.unwrap_or_default(),
                    anchor,
                };
                by_id.insert(record.id, checkpoint);
            }
            Op::Remove => {
                by_id.remove(&record.id);
            }
            Op::Anchor => {
                if let Some(existing) = by_id.get_mut(&record.id) {
                    existing.anchor = record.anchor;
                }
            }
            Op::Unknown => {}
        }
    }
    let mut checkpoints: Vec<Checkpoint> = by_id.into_values().collect();
    checkpoints.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
    (checkpoints, add_times)
}

fn clean_title(title: &str) -> Result<String, NotesError> {
    let title = single_line(title.trim());
    if title.is_empty() {
        return Err(NotesError::Invalid("a checkpoint needs a title".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(NotesError::Invalid(format!(
            "the title is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

fn clean_detail(detail: Option<String>) -> Result<Option<String>, NotesError> {
    let Some(detail) = detail.map(|detail| detail.trim().to_owned()) else {
        return Ok(None);
    };
    if detail.is_empty() {
        return Ok(None);
    }
    if detail.chars().count() > MAX_DETAIL_CHARS {
        return Err(NotesError::Invalid(format!(
            "the detail is longer than {MAX_DETAIL_CHARS} characters"
        )));
    }
    Ok(Some(detail))
}

fn clean_tags(tags: Vec<String>) -> Result<Vec<String>, NotesError> {
    let tags: Vec<String> = tags
        .into_iter()
        .map(|tag| single_line(tag.trim()))
        .filter(|tag| !tag.is_empty())
        .collect();
    if tags.len() > MAX_TAGS {
        return Err(NotesError::Invalid(format!("at most {MAX_TAGS} tags")));
    }
    if tags.iter().any(|tag| tag.chars().count() > MAX_TAG_CHARS) {
        return Err(NotesError::Invalid(format!(
            "a tag is longer than {MAX_TAG_CHARS} characters"
        )));
    }
    Ok(tags)
}

/// Line breaks and other controls as spaces.
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Crockford base32 (`0-9A-Z` without `I L O U`), most significant first.
pub(crate) fn crockford_base32(mut value: u64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut digits = Vec::with_capacity(13);
    loop {
        digits.push(ALPHABET[(value & 31) as usize]);
        value >>= 5;
        if value == 0 {
            break;
        }
    }
    digits.reverse();
    String::from_utf8_lossy(&digits).into_owned()
}
