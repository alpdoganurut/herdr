//! Recently closed agent sessions: the record Herdr keeps when a tab (or
//! pane) holding a resumable agent session is closed, and the parameters of
//! `session.closed_reopen` / `session.closed_remove`.

use serde::{Deserialize, Serialize};

use super::tabs::{TabColor, TabRemindInterval};
use crate::agent_resume::AgentSessionRefKind;

/// One closed agent session, newest first in `session.closed_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClosedSessionInfo {
    /// Stable id of the record: the close time and the session id.
    pub id: String,
    /// Who reported the session (for example `herdr:claude`).
    pub source: String,
    /// The agent (for example `claude`).
    pub agent: String,
    pub session_ref_kind: AgentSessionRefKind,
    /// The agent's native session id (or path, for path references).
    pub session_id: String,
    /// Where the agent kept the native transcript, when it was known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// The tab's custom label, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<TabColor>,
    #[serde(default)]
    pub important: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_every: Option<TabRemindInterval>,
    /// Fork (sidebar v3): the tab was pinned (`tab.set_pinned`); absent when
    /// not. Reopening restores the pin.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// The space (workspace) the tab was in.
    pub space_id: String,
    pub space_name: String,
    /// The pane's working directory.
    pub cwd: String,
    /// When the tab was closed, in seconds since the Unix epoch.
    pub closed_at: u64,
}

impl ClosedSessionInfo {
    /// What a list row calls the tab: its label, else the agent.
    pub fn title(&self) -> &str {
        self.label.as_deref().unwrap_or(self.agent.as_str())
    }

    /// The last component of the pane's directory.
    pub fn dir_name(&self) -> &str {
        std::path::Path::new(&self.cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(self.cwd.as_str())
    }
}

/// A closed session by its record id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClosedSessionTarget {
    pub id: String,
}

/// How long ago a session closed, compact: `just now`, `5m ago`, `2h ago`,
/// `3d ago` (whole units, rounded down).
pub fn compact_closed_age(seconds: u64) -> String {
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{EmptyParams, Method, Request, ResponseResult};

    fn info() -> ClosedSessionInfo {
        ClosedSessionInfo {
            id: "1700000000-abc".into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref_kind: AgentSessionRefKind::Id,
            session_id: "abc".into(),
            transcript_path: Some("/home/me/.claude/projects/-p/abc.jsonl".into()),
            label: Some("review".into()),
            color: Some(TabColor::Blue),
            important: true,
            remind_every: Some(TabRemindInterval::M10),
            pinned: false,
            space_id: "w_1".into(),
            space_name: "leap".into(),
            cwd: "/tmp/p".into(),
            closed_at: 1_700_000_000,
        }
    }

    #[test]
    fn closed_ages_are_compact() {
        assert_eq!(compact_closed_age(0), "just now");
        assert_eq!(compact_closed_age(59), "just now");
        assert_eq!(compact_closed_age(300), "5m ago");
        assert_eq!(compact_closed_age(3_599), "59m ago");
        assert_eq!(compact_closed_age(7_200), "2h ago");
        assert_eq!(compact_closed_age(3 * 86_400 + 5), "3d ago");
    }

    #[test]
    fn closed_session_requests_round_trip() {
        for (method, name) in [
            (
                Method::SessionClosedList(EmptyParams::default()),
                "session.closed_list",
            ),
            (
                Method::SessionClosedReopen(ClosedSessionTarget { id: "x".into() }),
                "session.closed_reopen",
            ),
            (
                Method::SessionClosedRemove(ClosedSessionTarget { id: "x".into() }),
                "session.closed_remove",
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
        let json = serde_json::json!({
            "id": "req",
            "method": "session.closed_reopen",
            "params": { "id": "1700000000-abc" },
        });
        assert!(matches!(
            serde_json::from_value::<Request>(json).unwrap().method,
            Method::SessionClosedReopen(ClosedSessionTarget { id }) if id == "1700000000-abc"
        ));
    }

    #[test]
    fn closed_session_list_response_round_trips_with_optional_fields_absent() {
        let result = ResponseResult::SessionClosedList {
            sessions: vec![info()],
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "session_closed_list");
        assert_eq!(json["sessions"][0]["session_ref_kind"], "id");
        assert_eq!(json["sessions"][0]["remind_every"], "10m");
        assert_eq!(json["sessions"][0]["color"], "blue");
        assert_eq!(
            serde_json::from_value::<ResponseResult>(json).unwrap(),
            result
        );

        let bare = ClosedSessionInfo {
            transcript_path: None,
            label: None,
            color: None,
            important: false,
            remind_every: None,
            pinned: false,
            ..info()
        };
        let json = serde_json::to_value(&bare).unwrap();
        for field in ["transcript_path", "label", "color", "remind_every"] {
            assert!(json.get(field).is_none(), "{field} is omitted");
        }
        let mut minimal = json.clone();
        minimal.as_object_mut().unwrap().remove("important");
        assert_eq!(
            serde_json::from_value::<ClosedSessionInfo>(minimal).unwrap(),
            bare
        );
    }
}
