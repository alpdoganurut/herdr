//! Agent notices (fork): the sticky "agent card" an agent shows its user
//! (`agent.notify`, `agent.notices`, `agent.notice_dismiss`).
//!
//! The sender is always the pane named by `caller_pane`, resolved by the
//! server from its own pane record; the parameters never name the sender.
//! Every field past the required core is optional or defaulted, and the
//! closed enums carry an `Unknown` fallback, so an older client can read a
//! newer server's reply.

use serde::{Deserialize, Serialize};

/// The `agent.notify` / `agent.notices` / `agent.notice_dismiss` method names.
pub mod method {
    pub const NOTIFY: &str = "agent.notify";
    pub const NOTICES: &str = "agent.notices";
    pub const NOTICE_DISMISS: &str = "agent.notice_dismiss";
}

/// The error codes `agent.notify` answers with.
pub mod error_code {
    /// The sender's rate limit (1 per 20 s, 5 per 10 min, 20 per hour); the
    /// message says when to retry.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// `[agents] notices = false`.
    pub const NOTICES_OFF: &str = "notices_off";
    /// `caller_pane` is not a live pane.
    pub const PANE_NOT_FOUND: &str = "pane_not_found";
    /// The title is empty after sanitizing.
    pub const INVALID_PARAMS: &str = "invalid_params";
}

/// The card's accent: `info` (default), `question` (blocked on the user),
/// `done` (a long task finished), `warning` (needs the user's care).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentNoticeKind {
    #[default]
    Info,
    Question,
    Done,
    Warning,
    /// A kind this side does not know (a newer peer); shown as `info`.
    #[serde(other)]
    Unknown,
}

impl AgentNoticeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info | Self::Unknown => "info",
            Self::Question => "question",
            Self::Done => "done",
            Self::Warning => "warning",
        }
    }

    /// `info|question|done|warning` (case-insensitive); anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "info" => Some(Self::Info),
            "question" => Some(Self::Question),
            "done" => Some(Self::Done),
            "warning" => Some(Self::Warning),
            _ => None,
        }
    }
}

/// `agent.notify`: show the user a card from the agent in `caller_pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentNotifyParams {
    /// The calling pane (`HERDR_PANE_ID`); aliases of a moved pane resolve.
    pub caller_pane: String,
    #[serde(default)]
    pub kind: AgentNoticeKind,
    /// One line, at most 80 characters after sanitizing; must not be empty.
    pub title: String,
    /// At most 3 lines and 280 characters after sanitizing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// What `agent.notify` did.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentNotifyOutcome {
    /// A new card (it replaced the sender's previous card, if any).
    #[default]
    Shown,
    /// Same title and body as the sender's current card within 10 minutes:
    /// the card was refreshed, no rate-limit slot was used.
    Deduped,
    #[serde(other)]
    Unknown,
}

/// `agent.notice_dismiss`: the cards to remove (`all` removes every card).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentNoticeDismissParams {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub all: bool,
}

/// One card as `agent.notices`, `agent.notice_dismiss` and the
/// `endpoint.agent-notices.v1` push report it. Tab and workspace are resolved
/// when the list is built (the pane may have moved since the card was made).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentNoticeInfo {
    /// `n<seq>`, unique for the server's lifetime.
    pub id: String,
    #[serde(default)]
    pub kind: AgentNoticeKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The sender's agent kind (`claude`, `codex`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The sender: agent name, else agent kind, else pane id.
    pub name: String,
    /// The sender's pane now.
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_label: Option<String>,
    /// When the card was made (or last refreshed by a duplicate), seconds
    /// since the Unix epoch.
    pub unix: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_unknown_kinds_fall_back() {
        for kind in [
            AgentNoticeKind::Info,
            AgentNoticeKind::Question,
            AgentNoticeKind::Done,
            AgentNoticeKind::Warning,
        ] {
            let json = serde_json::to_value(kind).unwrap();
            assert_eq!(json, kind.as_str());
            assert_eq!(
                serde_json::from_value::<AgentNoticeKind>(json).unwrap(),
                kind
            );
            assert_eq!(AgentNoticeKind::parse(kind.as_str()), Some(kind));
        }
        let future: AgentNoticeKind = serde_json::from_str("\"celebration\"").unwrap();
        assert_eq!(future, AgentNoticeKind::Unknown);
        assert_eq!(future.as_str(), "info");
        assert_eq!(AgentNoticeKind::parse("nope"), None);
        let outcome: AgentNotifyOutcome = serde_json::from_str("\"queued\"").unwrap();
        assert_eq!(outcome, AgentNotifyOutcome::Unknown);
    }

    #[test]
    fn params_default_their_optional_fields() {
        let params: AgentNotifyParams =
            serde_json::from_str(r#"{"caller_pane":"w1:p2","title":"hi"}"#).unwrap();
        assert_eq!(params.kind, AgentNoticeKind::Info);
        assert_eq!(params.body, None);
        let dismiss: AgentNoticeDismissParams = serde_json::from_str("{}").unwrap();
        assert!(dismiss.ids.is_empty() && !dismiss.all);
        let info: AgentNoticeInfo = serde_json::from_str(
            r#"{"id":"n1","title":"t","name":"api","pane_id":"w1:p2","unix":5}"#,
        )
        .unwrap();
        assert_eq!(info.kind, AgentNoticeKind::Info);
        assert_eq!(info.tab_id, None);
    }
}
