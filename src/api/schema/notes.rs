//! Notes and checkpoints (fork): the per-session markdown notes and the
//! checkpoint timeline behind the info pane, `herdr notes`, `herdr checkpoint`
//! and the agents' notes tools.
//!
//! Every field past the required core is optional or defaulted, and the closed
//! enums carry an `Unknown` fallback, so an older client can read a newer
//! server's reply. A write conflict is a result (`outcome: conflict` with the
//! current notes), never an error.

use serde::{Deserialize, Serialize};

/// The `notes.*` and `checkpoints.*` method names on the wire.
pub mod method {
    pub const GET: &str = "notes.get";
    pub const SET: &str = "notes.set";
    pub const APPEND: &str = "notes.append";
    pub const CP_LIST: &str = "checkpoints.list";
    pub const CP_ADD: &str = "checkpoints.add";
    pub const CP_UPDATE: &str = "checkpoints.update";
    pub const CP_REMOVE: &str = "checkpoints.remove";
    pub const CP_CONTEXT: &str = "checkpoints.context";

    /// Every method, in the order of the design's table.
    #[cfg(test)]
    pub const ALL: [&str; 8] = [
        GET, SET, APPEND, CP_LIST, CP_ADD, CP_UPDATE, CP_REMOVE, CP_CONTEXT,
    ];
}

/// The error codes the `notes.*` and `checkpoints.*` methods answer with.
pub mod error_code {
    /// A malformed target or argument (an empty target, an unsafe key, a
    /// title over its limit).
    pub const INVALID_PARAMS: &str = "invalid_params";
    /// The target (tab, pane, checkpoint id) does not exist.
    pub const NOT_FOUND: &str = "not_found";
    /// The notes file is over the size cap.
    pub const TOO_LARGE: &str = "too_large";
    /// Too many checkpoints added for this key in the last hour.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// `[notes] enabled = false`.
    pub const DISABLED: &str = "notes_disabled";
    /// Reading or writing a notes file failed.
    pub const IO: &str = "notes_io";
}

/// Which notes a request is about. Precedence: `key` > `pane_id` > `tab_id`;
/// all `None` is `invalid_params`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// A session key (`<agent>-<session id>` or `tab-<tab id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Who wrote something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotesAuthor {
    Agent,
    User,
    /// An author this client does not know (a newer server).
    #[serde(other)]
    Unknown,
}

fn default_author() -> NotesAuthor {
    NotesAuthor::Agent
}

/// A checkpoint's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    Decision,
    Milestone,
    Failure,
    Bookmark,
    Note,
    /// A kind this client does not know (a newer server).
    #[serde(other)]
    Unknown,
}

/// What a notes write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotesWriteOutcome {
    Written,
    /// `base_revision` was not the current revision; `notes` holds the
    /// current text and nothing was written.
    Conflict,
    /// The text was already the current text.
    Unchanged,
    /// An outcome this client does not know (a newer server).
    #[serde(other)]
    Unknown,
}

/// Where a checkpoint's transcript context came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointContextSource {
    /// The agent's live native transcript.
    Native,
    /// herdr's transcript backup.
    Backup,
    /// herdr's previous transcript backup.
    BackupPrevious,
    /// Still being read; ask again shortly.
    Pending,
    /// No transcript was found.
    Missing,
    /// The agent's transcripts are not readable by herdr, or the checkpoint
    /// has no anchor (tab notes).
    Unsupported,
    /// A source this client does not know (a newer server).
    #[serde(other)]
    Unknown,
}

/// `notes.get`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesGetParams {
    pub target: NotesTarget,
    /// The revision the caller already has; a match replies `unchanged`
    /// without the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<String>,
}

/// `notes.set`: replace the text when `base_revision` is current (compare
/// and swap). `None` only succeeds while no notes file exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesSetParams {
    pub target: NotesTarget,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(default = "default_author")]
    pub author: NotesAuthor,
}

/// `notes.append`: add text at the end, or at the end of `## <section>`
/// (created at the end when missing). Never conflicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesAppendParams {
    pub target: NotesTarget,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Prefix the text with `- HH:MM ` (local time).
    #[serde(default)]
    pub stamp: bool,
    #[serde(default = "default_author")]
    pub author: NotesAuthor,
}

/// A notes file as the server sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesInfo {
    pub key: String,
    pub path: String,
    /// `sha256:<12 hex>` of the text, or `none` while no file exists.
    pub revision: String,
    pub exists: bool,
    /// `known_revision` matched: `text` is omitted.
    #[serde(default)]
    pub unchanged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub bytes: u64,
    /// Unix seconds of the last write through herdr.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<NotesAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The key this pane used before its session changed (after `/clear`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

/// The result of `notes.set` and `notes.append`. `notes.revision` is the
/// revision after the write (the current one on a conflict).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NotesWriteInfo {
    pub outcome: NotesWriteOutcome,
    pub notes: NotesInfo,
}

/// One checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointInfo {
    pub id: String,
    /// Unix seconds.
    pub ts: u64,
    pub kind: CheckpointKind,
    pub author: NotesAuthor,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// The checkpoint has a transcript anchor (`checkpoints.context` can
    /// answer with text).
    #[serde(default)]
    pub has_context: bool,
}

/// `checkpoints.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointsListParams {
    pub target: NotesTarget,
    /// Only these kinds; empty means all.
    #[serde(default)]
    pub kinds: Vec<CheckpointKind>,
    /// The `seq` the caller already has; a match replies `unchanged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    /// The newest this many (default 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// The result of `checkpoints.list`, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointsListInfo {
    pub key: String,
    /// The store's change stamp.
    pub seq: u64,
    #[serde(default)]
    pub unchanged: bool,
    #[serde(default)]
    pub checkpoints: Vec<CheckpointInfo>,
    /// Unix seconds of the first checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

/// `checkpoints.add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointsAddParams {
    pub target: NotesTarget,
    pub kind: CheckpointKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_author")]
    pub author: NotesAuthor,
}

/// `checkpoints.update`: the given fields replace the stored ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointsUpdateParams {
    pub target: NotesTarget,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CheckpointKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// `checkpoints.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointTarget {
    pub target: NotesTarget,
    pub id: String,
}

/// `checkpoints.context`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointsContextParams {
    pub target: NotesTarget,
    pub id: String,
    /// Characters per side (default 600, at most 4000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<u32>,
}

/// The result of `checkpoints.add`, `checkpoints.update` and
/// `checkpoints.remove`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointWriteInfo {
    pub key: String,
    /// The store's change stamp after the write.
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<CheckpointInfo>,
    /// The add matched a recent checkpoint (same kind, title and author) and
    /// updated it instead.
    #[serde(default)]
    pub folded: bool,
    #[serde(default)]
    pub removed: bool,
}

/// The result of `checkpoints.context`: the prompt and reply around the
/// moment the checkpoint was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckpointContextInfo {
    pub id: String,
    pub source: CheckpointContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ts: Option<u64>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_ts: Option<u64>,
    /// The prompt is outside the read window (the turn started earlier).
    #[serde(default)]
    pub continued: bool,
    /// A side was cut to `chars`.
    #[serde(default)]
    pub truncated: bool,
}

/// One request per method, in [`method::ALL`] order.
#[cfg(test)]
pub(crate) fn sample_methods() -> Vec<crate::api::schema::Method> {
    use crate::api::schema::Method;
    let target = NotesTarget {
        tab_id: Some("w1:t1".into()),
        ..NotesTarget::default()
    };
    vec![
        Method::NotesGet(NotesGetParams {
            target: target.clone(),
            known_revision: None,
        }),
        Method::NotesSet(NotesSetParams {
            target: target.clone(),
            text: "x".into(),
            base_revision: None,
            author: NotesAuthor::User,
        }),
        Method::NotesAppend(NotesAppendParams {
            target: target.clone(),
            text: "x".into(),
            section: None,
            stamp: false,
            author: NotesAuthor::Agent,
        }),
        Method::CheckpointsList(CheckpointsListParams {
            target: target.clone(),
            ..CheckpointsListParams::default()
        }),
        Method::CheckpointsAdd(CheckpointsAddParams {
            target: target.clone(),
            kind: CheckpointKind::Bookmark,
            title: "t".into(),
            detail: None,
            tags: Vec::new(),
            author: NotesAuthor::User,
        }),
        Method::CheckpointsUpdate(CheckpointsUpdateParams {
            target: target.clone(),
            id: "cp_1".into(),
            kind: None,
            title: None,
            detail: None,
            tags: None,
        }),
        Method::CheckpointsRemove(CheckpointTarget {
            target: target.clone(),
            id: "cp_1".into(),
        }),
        Method::CheckpointsContext(CheckpointsContextParams {
            target,
            id: "cp_1".into(),
            chars: None,
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, Request};

    #[test]
    fn every_method_round_trips_under_its_wire_name() {
        let target = NotesTarget {
            pane_id: Some("w1:p1".into()),
            ..NotesTarget::default()
        };
        for (method, name) in [
            (
                Method::NotesGet(NotesGetParams {
                    target: target.clone(),
                    known_revision: Some("none".into()),
                }),
                method::GET,
            ),
            (
                Method::NotesSet(NotesSetParams {
                    target: target.clone(),
                    text: "x".into(),
                    base_revision: None,
                    author: NotesAuthor::User,
                }),
                method::SET,
            ),
            (
                Method::NotesAppend(NotesAppendParams {
                    target: target.clone(),
                    text: "x".into(),
                    section: Some("Plan".into()),
                    stamp: true,
                    author: NotesAuthor::Agent,
                }),
                method::APPEND,
            ),
            (
                Method::CheckpointsList(CheckpointsListParams {
                    target: target.clone(),
                    kinds: vec![CheckpointKind::Decision],
                    since_seq: Some(3),
                    limit: Some(20),
                }),
                method::CP_LIST,
            ),
            (
                Method::CheckpointsAdd(CheckpointsAddParams {
                    target: target.clone(),
                    kind: CheckpointKind::Milestone,
                    title: "t".into(),
                    detail: None,
                    tags: vec!["a".into()],
                    author: NotesAuthor::Agent,
                }),
                method::CP_ADD,
            ),
            (
                Method::CheckpointsUpdate(CheckpointsUpdateParams {
                    target: target.clone(),
                    id: "cp_1".into(),
                    kind: None,
                    title: Some("t2".into()),
                    detail: None,
                    tags: None,
                }),
                method::CP_UPDATE,
            ),
            (
                Method::CheckpointsRemove(CheckpointTarget {
                    target: target.clone(),
                    id: "cp_1".into(),
                }),
                method::CP_REMOVE,
            ),
            (
                Method::CheckpointsContext(CheckpointsContextParams {
                    target,
                    id: "cp_1".into(),
                    chars: Some(600),
                }),
                method::CP_CONTEXT,
            ),
        ] {
            let request = Request {
                id: "req".into(),
                method,
            };
            let json = serde_json::to_value(&request).unwrap();
            assert_eq!(json["method"], name);
            assert_eq!(crate::api::api_method_name(&request.method), name);
            assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
        }
    }

    #[test]
    fn author_defaults_to_agent_and_unknown_enum_values_fall_back() {
        let params: NotesSetParams =
            serde_json::from_str(r#"{"target":{"tab_id":"w1:t1"},"text":"x"}"#).unwrap();
        assert_eq!(params.author, NotesAuthor::Agent);
        assert_eq!(params.base_revision, None);
        let kind: CheckpointKind = serde_json::from_str(r#""epiphany""#).unwrap();
        assert_eq!(kind, CheckpointKind::Unknown);
        let source: CheckpointContextSource = serde_json::from_str(r#""cloud""#).unwrap();
        assert_eq!(source, CheckpointContextSource::Unknown);
        let outcome: NotesWriteOutcome = serde_json::from_str(r#""merged""#).unwrap();
        assert_eq!(outcome, NotesWriteOutcome::Unknown);
        let author: NotesAuthor = serde_json::from_str(r#""robot""#).unwrap();
        assert_eq!(author, NotesAuthor::Unknown);
        let pending: CheckpointContextSource = serde_json::from_str(r#""pending""#).unwrap();
        assert_eq!(pending, CheckpointContextSource::Pending);
    }

    #[test]
    fn replies_omit_absent_optional_fields() {
        let info = NotesInfo {
            key: "tab-w1-t1".into(),
            path: "/tmp/notes/tab-w1-t1.md".into(),
            revision: "none".into(),
            ..NotesInfo::default()
        };
        let json = serde_json::to_value(&info).unwrap();
        assert!(json.get("text").is_none());
        assert!(json.get("previous").is_none());
        let back: NotesInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }
}
