//! The agents model (fork, agents v2): every tab is part of herdr+, and the
//! server decides what an agent may do to another tab.
//!
//! This module is pure where it can be: the policy, the envelope and the
//! limits take plain values. The action log is the one file it writes.

pub mod actions_log;
pub mod envelope;
pub mod limits;
pub mod policy;
pub mod reply_index;
pub mod turn;

use serde::{Deserialize, Serialize};

use crate::terminal::TerminalId;

/// Who opened a tab (`agents.open_tab` sets it; the user's paths leave it
/// `None`, shown as the user).
pub type OpenedBy = crate::api::schema::agents_model::AgentsWho;

/// Where the input written into a pane came from, recorded next to the write
/// (`App::note_input`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSource {
    /// A client: key, text and paste from a client shell, or raw bytes from a
    /// terminal-attach session (`attach`). `submit` when the input submits a
    /// line (a CR or LF, an Enter key).
    Client { submit: bool, attach: bool },
    /// herdr itself or a process through the API.
    Programmatic(Programmatic),
    /// Not input: mouse reports, focus events, probes, restore writes.
    Internal,
}

/// A programmatic write's source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Programmatic {
    /// `agents.send_message`.
    AgentMessage { id: String, from: TerminalId },
    /// The coordinator's wake-up.
    HerdrWake { seq: u64 },
    /// Any other API write (`agent.prompt`, `pane.send_*`, agent start).
    Api,
}

/// Where a pane's turn came from (runtime only, never persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TurnOrigin {
    /// The pane's user submitted input since the last idle edge.
    User,
    AgentMessage {
        id: String,
        from: TerminalId,
    },
    HerdrWake {
        seq: u64,
    },
    Programmatic,
    /// No input since the last idle edge: a background completion, a queued
    /// prompt, a scheduled wake.
    SelfStarted,
    /// After a restart, a handoff or a reopen; fails closed.
    #[default]
    Unknown,
}

impl From<&Programmatic> for TurnOrigin {
    fn from(source: &Programmatic) -> Self {
        match source {
            Programmatic::AgentMessage { id, from } => Self::AgentMessage {
                id: id.clone(),
                from: from.clone(),
            },
            Programmatic::HerdrWake { seq } => Self::HerdrWake { seq: *seq },
            Programmatic::Api => Self::Programmatic,
        }
    }
}

/// A pane's agent meta: replaces `managed.json`. On `TerminalState`,
/// persisted with the pane snapshot and carried through a close and reopen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentMeta {
    /// At most 32 characters, one line (`team::sanitize_role`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// At most 200 characters, one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by: Option<OpenedBy>,
    /// The group (workspace id) of a team the pane left or was removed from:
    /// it does not rejoin that team on detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_excluded: Option<String>,
    /// A role was set while the agent could not be renamed; applied when the
    /// agent is next detected.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_rename: bool,
    /// The role was cleared on purpose: migration never brings it back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub role_cleared: bool,
    /// The note was cleared on purpose: migration never brings it back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub note_cleared: bool,
    /// Public pane ids that still name this pane (an agent keeps the
    /// `HERDR_PANE_ID` it was spawned with across a cross-group move); they
    /// survive a restart and a live handoff with the pane.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_aliases: Vec<String>,
    #[serde(default)]
    pub updated_unix: u64,
}

impl PaneAgentMeta {
    /// Nothing to persist.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}
