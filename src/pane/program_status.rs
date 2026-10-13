//! OSC 7501 Program Status (fork, stage 1: observe only).
//!
//! A program reports its own status with `OSC 7501 ; state=…[:k=v…] ST|BEL`
//! after the terminal answered its `OSC 7501 ; ?` probe (spec rev 0.4; Claude
//! Code 2.1.295+ emits it). herdr answers the probe, parses the reports into a
//! per-pane record store (the root record plus one child per `id`), and shows
//! the records in `herdr agent explain`. Screen detection stays the authority:
//! nothing here changes an agent's state (src/pane/program_status_log.rs only
//! logs where the two disagree).
//!
//! All report content is untrusted pane output. The parser is lenient where
//! the spec allows (malformed pairs skipped, unknown keys ignored, last value
//! wins, `blocked` without `kind` accepted because Claude sends it) and
//! rejects the whole report on any limit, base64, UTF-8 or control character
//! violation. Bidi and invisible formatting characters are stripped.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use base64::Engine;

use crate::detect::AgentState;

static PROGRAM_STATUS_ENABLED: AtomicBool = AtomicBool::new(true);
/// Bumped whenever the switch changes, so records kept from before an
/// off period are dropped instead of shown again.
static PROGRAM_STATUS_EPOCH: AtomicU64 = AtomicU64::new(0);

/// `[agents] program_status`: off means no probe reply, reports ignored and
/// no disagreement log. Applied live.
pub(crate) fn set_enabled(enabled: bool) {
    if PROGRAM_STATUS_ENABLED.swap(enabled, Ordering::Relaxed) != enabled {
        PROGRAM_STATUS_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn enabled() -> bool {
    PROGRAM_STATUS_ENABLED.load(Ordering::Relaxed)
}

fn epoch() -> u64 {
    PROGRAM_STATUS_EPOCH.load(Ordering::Relaxed)
}

/// The reply to `OSC 7501 ; ?`. The lists name exactly what herdr parses.
pub(super) const PROBE_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// The collector caps an OSC body at 4096 bytes, so `7501;` leaves at most
/// 4091; the spec's whole-sequence limit leaves 4088 for the payload.
const MAX_PAYLOAD_BYTES: usize = 4088;
const MAX_KEY_BYTES: usize = 16;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_SEGMENTS: usize = 8;
const MAX_APP_BYTES: usize = 32;
const MAX_TITLE_ENCODED: usize = 256;
const MAX_TITLE_DECODED: usize = 192;
const MAX_MSG_ENCODED: usize = 2732;
const MAX_MSG_DECODED: usize = 2048;
/// Records per pane (the spec's minimum; Claude's own maximum is 1 + 32).
pub(super) const MAX_RECORDS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl ProgramState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    fn code(self) -> u64 {
        match self {
            Self::Idle => 1,
            Self::Working => 2,
            Self::Done => 3,
            Self::Blocked => 4,
            Self::Error => 5,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::Idle),
            2 => Some(Self::Working),
            3 => Some(Self::Done),
            4 => Some(Self::Blocked),
            5 => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStatusKind {
    Permission,
    Question,
    Auth,
    Other,
}

impl ProgramStatusKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
            Self::Other => "other",
        }
    }
}

/// How a record's state reads as a herdr agent state: working → Working,
/// blocked → Blocked, done / idle / error → Idle.
pub(crate) fn mapped_agent_state(state: ProgramState) -> AgentState {
    match state {
        ProgramState::Working => AgentState::Working,
        ProgramState::Blocked => AgentState::Blocked,
        ProgramState::Idle | ProgramState::Done | ProgramState::Error => AgentState::Idle,
    }
}

/// One reported record, as parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramStatusRecord {
    pub state: ProgramState,
    /// Only for `blocked`; `None` is a kind-less blocked (Claude sends it).
    pub kind: Option<ProgramStatusKind>,
    /// As reported; inheritance is resolved when read.
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    /// 0..=100, only for working and blocked.
    pub progress: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStatusReport {
    Set {
        /// `""` is the root record.
        id: String,
        record: ProgramStatusRecord,
    },
    /// `None` clears every record.
    Clear { id: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStatusReject {
    TooLong,
    MissingState,
    UnknownState,
    KeyTooLong,
    IdLimit,
    AppTooLong,
    Base64,
    Utf8,
    ControlChar,
    MsgTooLong,
    TitleTooLong,
}

impl ProgramStatusReject {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::TooLong => "too_long",
            Self::MissingState => "missing_state",
            Self::UnknownState => "unknown_state",
            Self::KeyTooLong => "key_too_long",
            Self::IdLimit => "id_limit",
            Self::AppTooLong => "app_too_long",
            Self::Base64 => "base64",
            Self::Utf8 => "utf8",
            Self::ControlChar => "control_char",
            Self::MsgTooLong => "msg_too_long",
            Self::TitleTooLong => "title_too_long",
        }
    }
}

/// Whether an OSC 7501 payload is the probe. Exactly `?`: a reply (which
/// starts with `?:`) echoed back by a raw-mode program must not be answered
/// again, and an interrupted query carries the bytes that followed it.
pub(super) fn is_probe(payload: &[u8]) -> bool {
    payload == b"?"
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-')
}

/// Parses one OSC 7501 payload (the body after `7501;`).
pub(crate) fn parse_program_status(
    payload: &[u8],
) -> Result<ProgramStatusReport, ProgramStatusReject> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ProgramStatusReject::TooLong);
    }
    // ESC lands in a body only when a sequence was interrupted; the remains
    // could still hold a plausible `state` pair.
    if payload.iter().any(|&byte| byte < 0x20 || byte == 0x7f) {
        return Err(ProgramStatusReject::ControlChar);
    }
    let mut state = None;
    let mut id: Option<&[u8]> = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut msg = None;
    for pair in payload.split(|&byte| byte == b':') {
        let Some(eq) = pair.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let (key, value) = (&pair[..eq], &pair[eq + 1..]);
        if key.is_empty() || !key.iter().copied().all(is_key_byte) {
            continue;
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(ProgramStatusReject::KeyTooLong);
        }
        if !value.iter().copied().all(is_value_byte) {
            continue;
        }
        match key {
            b"state" => state = Some(value),
            b"id" => id = Some(value),
            b"kind" => kind = Some(value),
            b"progress" => progress = Some(value),
            b"app" => app = Some(value),
            b"title" => title = Some(value),
            b"msg" => msg = Some(value),
            _ => {}
        }
    }

    let state = match state.ok_or(ProgramStatusReject::MissingState)? {
        b"idle" => Some(ProgramState::Idle),
        b"working" => Some(ProgramState::Working),
        b"done" => Some(ProgramState::Done),
        b"blocked" => Some(ProgramState::Blocked),
        b"error" => Some(ProgramState::Error),
        b"clear" => None,
        _ => return Err(ProgramStatusReject::UnknownState),
    };
    let id = parse_id(id.unwrap_or_default())?;
    let Some(state) = state else {
        return Ok(ProgramStatusReport::Clear {
            id: (!id.is_empty()).then_some(id),
        });
    };

    let app = match app.filter(|app| !app.is_empty()) {
        Some(app) if app.len() > MAX_APP_BYTES => return Err(ProgramStatusReject::AppTooLong),
        // The value charset is ASCII.
        Some(app) => Some(String::from_utf8_lossy(app).into_owned()),
        None => None,
    };
    let kind = (state == ProgramState::Blocked)
        .then(|| {
            kind.map(|kind| match kind {
                b"permission" => ProgramStatusKind::Permission,
                b"question" => ProgramStatusKind::Question,
                b"auth" => ProgramStatusKind::Auth,
                _ => ProgramStatusKind::Other,
            })
        })
        .flatten();
    let progress = matches!(state, ProgramState::Working | ProgramState::Blocked)
        .then(|| progress.and_then(parse_progress))
        .flatten();
    let title = decode_text(
        title,
        MAX_TITLE_ENCODED,
        MAX_TITLE_DECODED,
        ProgramStatusReject::TitleTooLong,
    )?;
    let msg = decode_text(
        msg,
        MAX_MSG_ENCODED,
        MAX_MSG_DECODED,
        ProgramStatusReject::MsgTooLong,
    )?;
    Ok(ProgramStatusReport::Set {
        id,
        record: ProgramStatusRecord {
            state,
            kind,
            app,
            title,
            msg,
            progress,
        },
    })
}

fn parse_id(id: &[u8]) -> Result<String, ProgramStatusReject> {
    if id.is_empty() {
        return Ok(String::new());
    }
    if id.len() > MAX_ID_BYTES {
        return Err(ProgramStatusReject::IdLimit);
    }
    let mut segments = 0;
    for segment in id.split(|&byte| byte == b'/') {
        segments += 1;
        if segment.is_empty() || segment.len() > MAX_ID_SEGMENT_BYTES || segments > MAX_ID_SEGMENTS
        {
            return Err(ProgramStatusReject::IdLimit);
        }
    }
    Ok(String::from_utf8_lossy(id).into_owned())
}

fn parse_progress(value: &[u8]) -> Option<u8> {
    if value.is_empty() || value.len() > 3 || !value.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let number = value
        .iter()
        .fold(0u16, |acc, digit| acc * 10 + u16::from(digit - b'0'));
    Some(number.min(100) as u8)
}

fn decode_text(
    value: Option<&[u8]>,
    max_encoded: usize,
    max_decoded: usize,
    too_long: ProgramStatusReject,
) -> Result<Option<String>, ProgramStatusReject> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > max_encoded {
        return Err(too_long);
    }
    let decoded = decode_base64(value).ok_or(ProgramStatusReject::Base64)?;
    if decoded.len() > max_decoded {
        return Err(too_long);
    }
    let text = String::from_utf8(decoded).map_err(|_| ProgramStatusReject::Utf8)?;
    if text.chars().any(char::is_control) {
        return Err(ProgramStatusReject::ControlChar);
    }
    let text: String = text.chars().filter(|&ch| !is_stripped_char(ch)).collect();
    Ok((!text.is_empty()).then_some(text))
}

fn decode_base64(value: &[u8]) -> Option<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    const CONFIG: GeneralPurposeConfig =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    const STANDARD: GeneralPurpose = GeneralPurpose::new(&base64::alphabet::STANDARD, CONFIG);
    const URL_SAFE: GeneralPurpose = GeneralPurpose::new(&base64::alphabet::URL_SAFE, CONFIG);
    STANDARD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value))
        .ok()
}

/// Bidi controls and invisible formatting characters: neutralized, as the
/// spec asks, so a message cannot reorder or hide surrounding text.
fn is_stripped_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredRecord {
    record: ProgramStatusRecord,
    updated_seq: u64,
    updated_at: Instant,
}

/// Counters for one agent session, read once at the agent change for the
/// session summary line, and reset with the records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramStatusSessionCounters {
    pub probe_answered: bool,
    pub reports: u32,
    pub rejects: u32,
    pub root_transitions: u32,
}

/// A pane's records. Lives in the pane's OSC tracker, under the core lock.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatusStore {
    records: BTreeMap<String, StoredRecord>,
    seq: u64,
    /// Bumped on every change, including a rejected report.
    revision: u64,
    probe_answered: bool,
    rejected: u32,
    last_reject: Option<ProgramStatusReject>,
    last_reject_at: Option<Instant>,
    /// The switch epoch the records belong to.
    epoch: u64,
    packed: u64,
    packed_dirty: bool,
    session: ProgramStatusSessionCounters,
}

impl ProgramStatusStore {
    pub(crate) fn apply(&mut self, report: ProgramStatusReport, now: Instant) {
        let root_before = self.root_state();
        self.session.reports = self.session.reports.saturating_add(1);
        match report {
            ProgramStatusReport::Set { id, record } => {
                self.seq = self.seq.wrapping_add(1);
                if !self.records.contains_key(&id) && self.records.len() >= MAX_RECORDS {
                    self.evict_oldest_child();
                }
                self.records.insert(
                    id,
                    StoredRecord {
                        record,
                        updated_seq: self.seq,
                        updated_at: now,
                    },
                );
                self.bump();
            }
            ProgramStatusReport::Clear { id: None } => {
                if !self.records.is_empty() {
                    self.records.clear();
                    self.bump();
                }
            }
            ProgramStatusReport::Clear { id: Some(id) } => {
                let before = self.records.len();
                let prefix = format!("{id}/");
                self.records
                    .retain(|key, _| key != &id && !key.starts_with(&prefix));
                if self.records.len() != before {
                    self.bump();
                }
            }
        }
        if self.root_state() != root_before {
            self.session.root_transitions = self.session.root_transitions.saturating_add(1);
        }
    }

    fn evict_oldest_child(&mut self) {
        let oldest = self
            .records
            .iter()
            .filter(|(id, _)| !id.is_empty())
            .min_by_key(|(_, stored)| stored.updated_seq)
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            self.records.remove(&id);
        }
    }

    pub(crate) fn note_reject(&mut self, reject: ProgramStatusReject, now: Instant) {
        self.rejected = self.rejected.saturating_add(1);
        self.session.rejects = self.session.rejects.saturating_add(1);
        self.last_reject = Some(reject);
        self.last_reject_at = Some(now);
        self.bump();
    }

    /// A new `?` probe: a new program starts reporting, so earlier records
    /// (a script's, a previous run's) must not leak into it.
    pub(crate) fn note_probe(&mut self) {
        self.records.clear();
        self.probe_answered = true;
        self.session.probe_answered = true;
        self.bump();
    }

    /// RIS (`ESC c`) removes every record; a soft reset keeps them.
    pub(crate) fn clear_records(&mut self) {
        if !self.records.is_empty() {
            self.records.clear();
            self.bump();
        }
    }

    /// The foreground agent changed: a blank slate, counters included.
    pub(crate) fn clear_retained(&mut self) {
        let changed = !self.records.is_empty() || self.probe_answered || self.rejected > 0;
        self.records.clear();
        self.probe_answered = false;
        self.rejected = 0;
        self.last_reject = None;
        self.last_reject_at = None;
        self.session = ProgramStatusSessionCounters::default();
        if changed {
            self.bump();
        }
    }

    /// Drops records kept from before the switch last changed.
    pub(crate) fn sync_epoch(&mut self, epoch: u64) {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.clear_records();
        }
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        let packed = self.compute_packed();
        if packed != self.packed {
            self.packed = packed;
            self.packed_dirty = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn session_counters(&self) -> ProgramStatusSessionCounters {
        self.session
    }

    /// False while the records predate the switch's last change (they are
    /// dropped by the next report's `sync_epoch`; until then, every reader
    /// sees none).
    fn records_current(&self) -> bool {
        self.epoch == epoch()
    }

    fn root_state(&self) -> Option<ProgramState> {
        self.root().map(|record| record.state)
    }

    pub(crate) fn root(&self) -> Option<&ProgramStatusRecord> {
        if !self.records_current() {
            return None;
        }
        self.records.get("").map(|stored| &stored.record)
    }

    fn child_count(&self) -> usize {
        if !self.records_current() {
            return 0;
        }
        self.records.len() - usize::from(self.records.contains_key(""))
    }

    fn compute_packed(&self) -> u64 {
        pack_root(PackedRoot {
            state: self.root_state(),
            children: self.child_count().min(255) as u8,
            rejected: (self.rejected & 0xff) as u8,
            revision: self.revision,
        })
    }

    /// The packed root when it changed since the last call, for the pane's
    /// lock-free mirror. Called under the core lock.
    pub(crate) fn take_packed_if_changed(&mut self) -> Option<u64> {
        std::mem::take(&mut self.packed_dirty).then_some(self.packed)
    }

    /// The root as an `osc_status` detection region would see it, for
    /// example `blocked/permission` or `working`.
    #[allow(dead_code)] // stage 2: an osc_status detection region
    pub(crate) fn root_region_text(&self) -> String {
        match self.root() {
            None => String::new(),
            Some(record) => match (record.state, record.kind) {
                (ProgramState::Blocked, Some(kind)) => format!("blocked/{}", kind.label()),
                (state, _) => state.label().to_string(),
            },
        }
    }

    fn resolved_app(&self, id: &str) -> Option<String> {
        let mut current = id;
        loop {
            if let Some(app) = self
                .records
                .get(current)
                .and_then(|stored| stored.record.app.as_ref())
            {
                return Some(app.clone());
            }
            if current.is_empty() {
                return None;
            }
            current = current.rfind('/').map_or("", |index| &current[..index]);
        }
    }

    fn view(&self, id: &str, stored: &StoredRecord, now: Instant) -> ProgramStatusRecordView {
        ProgramStatusRecordView {
            id: id.to_string(),
            record: stored.record.clone(),
            app: self.resolved_app(id),
            age_ms: now
                .saturating_duration_since(stored.updated_at)
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        }
    }

    /// An owned copy for explain. Records from before a switch change, or
    /// while the switch is off, are hidden.
    pub(crate) fn snapshot(&self, now: Instant) -> ProgramStatusSnapshot {
        let enabled = enabled();
        let visible = enabled && self.records_current();
        let (root, children) = if visible {
            (
                self.records
                    .get("")
                    .map(|stored| self.view("", stored, now)),
                self.records
                    .iter()
                    .filter(|(id, _)| !id.is_empty())
                    .map(|(id, stored)| self.view(id, stored, now))
                    .collect(),
            )
        } else {
            (None, Vec::new())
        };
        ProgramStatusSnapshot {
            enabled,
            probe_answered: self.probe_answered,
            revision: self.revision,
            rejected: self.rejected,
            last_reject: self.last_reject,
            last_reject_age_ms: self
                .last_reject_at
                .map(|at| now.saturating_duration_since(at).as_millis() as u64),
            root,
            children,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramStatusRecordView {
    pub id: String,
    pub record: ProgramStatusRecord,
    /// The record's app, else the nearest ancestor's, else the root's.
    pub app: Option<String>,
    pub age_ms: u64,
}

impl ProgramStatusRecordView {
    fn to_json(&self, with_id: bool) -> serde_json::Value {
        let mut value = serde_json::json!({
            "state": self.record.state.label(),
            "kind": self.record.kind.map(ProgramStatusKind::label),
            "app": self.app,
            "progress": self.record.progress,
            "title": self.record.title,
            "msg": self.record.msg,
            "age_ms": self.age_ms,
        });
        if with_id {
            if let Some(object) = value.as_object_mut() {
                object.insert("id".into(), serde_json::Value::String(self.id.clone()));
            }
        }
        value
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramStatusSnapshot {
    pub enabled: bool,
    pub probe_answered: bool,
    pub revision: u64,
    pub rejected: u32,
    pub last_reject: Option<ProgramStatusReject>,
    pub last_reject_age_ms: Option<u64>,
    pub root: Option<ProgramStatusRecordView>,
    pub children: Vec<ProgramStatusRecordView>,
}

impl ProgramStatusSnapshot {
    pub(crate) fn mapped_state(&self) -> Option<AgentState> {
        self.root
            .as_ref()
            .map(|root| mapped_agent_state(root.record.state))
    }

    /// The explain object. `screen_state` is the explain's own state; `None`
    /// (or an unknown state, or no root) leaves `agrees_with_screen` null.
    pub(crate) fn to_json(&self, screen_state: Option<AgentState>) -> serde_json::Value {
        let mapped = self.mapped_state();
        let agrees = match (mapped, screen_state) {
            (Some(mapped), Some(screen)) if screen != AgentState::Unknown => Some(mapped == screen),
            _ => None,
        };
        serde_json::json!({
            "enabled": self.enabled,
            "probe_answered": self.probe_answered,
            "revision": self.revision,
            "root": self.root.as_ref().map(|root| root.to_json(false)),
            "children": self
                .children
                .iter()
                .map(|child| child.to_json(true))
                .collect::<Vec<_>>(),
            "mapped_state": mapped.map(crate::detect::manifest::agent_state_label),
            "agrees_with_screen": agrees,
            "rejected_reports": self.rejected,
            "last_reject": self.last_reject.map(ProgramStatusReject::label),
            "last_reject_age_ms": self.last_reject_age_ms,
        })
    }
}

/// The root as the detection loop reads it without the core lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PackedRoot {
    pub state: Option<ProgramState>,
    /// Child records, saturating at 255.
    pub children: u8,
    /// Rejected reports, wrapping at 256 (only differences are meaningful).
    pub rejected: u8,
    /// The store revision, wrapping at 2^44.
    pub revision: u64,
}

const PACK_CODE_BITS: u32 = 4;
const PACK_CHILDREN_SHIFT: u32 = PACK_CODE_BITS;
const PACK_REJECTED_SHIFT: u32 = PACK_CHILDREN_SHIFT + 8;
const PACK_REVISION_SHIFT: u32 = PACK_REJECTED_SHIFT + 8;

/// Layout: bits 0..4 the root state code (0 = no root), 4..12 the child
/// count, 12..20 the rejected count, 20..64 the revision.
pub(crate) fn pack_root(root: PackedRoot) -> u64 {
    root.state.map_or(0, ProgramState::code)
        | u64::from(root.children) << PACK_CHILDREN_SHIFT
        | u64::from(root.rejected) << PACK_REJECTED_SHIFT
        | root.revision << PACK_REVISION_SHIFT
}

pub(crate) fn unpack_root(packed: u64) -> PackedRoot {
    PackedRoot {
        state: ProgramState::from_code(packed & ((1 << PACK_CODE_BITS) - 1)),
        children: ((packed >> PACK_CHILDREN_SHIFT) & 0xff) as u8,
        rejected: ((packed >> PACK_REJECTED_SHIFT) & 0xff) as u8,
        revision: packed >> PACK_REVISION_SHIFT,
    }
}

/// The OSC 7501 arm of the pane's OSC tracker. Records the probe's end
/// offset in `probe` (the first probe of a chunk only: one reply per chunk).
pub(super) fn observe_report(
    store: &mut ProgramStatusStore,
    probe: &mut Option<usize>,
    payload: &[u8],
    end_offset: usize,
) {
    if !enabled() {
        return;
    }
    store.sync_epoch(epoch());
    if is_probe(payload) {
        if probe.is_none() {
            *probe = Some(end_offset);
            store.note_probe();
        }
        return;
    }
    let now = Instant::now();
    match parse_program_status(payload) {
        Ok(report) => store.apply(report, now),
        Err(reject) => {
            tracing::debug!(
                target: "herdr::program_status",
                reason = reject.label(),
                len = payload.len(),
                "rejected program status report"
            );
            store.note_reject(reject, now);
        }
    }
}

/// Turns the switch off for one test and back on when dropped, so a
/// failing assertion cannot leave it off for the tests after it.
#[cfg(test)]
pub(crate) struct SwitchOffForTest;

#[cfg(test)]
impl SwitchOffForTest {
    pub(crate) fn new() -> Self {
        set_enabled(false);
        Self
    }
}

#[cfg(test)]
impl Drop for SwitchOffForTest {
    fn drop(&mut self) {
        set_enabled(true);
    }
}

#[cfg(test)]
mod tests;
