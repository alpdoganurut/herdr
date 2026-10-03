//! Agent messages (fork): server-side delivery of `herdr_agents` messages
//! (`agent.message_send`, `agent.message_claim`).
//!
//! The MCP server builds the message (envelope, id, log fields) and the
//! server types it into the target now when it can take it, or queues it and
//! types it in once the target is free (idle and settled, no user typing,
//! no live coordinator turn). Neither method is advertised to client shells.
//! Every field past the required core is optional or defaulted and the
//! outcome enum carries an `Unknown` fallback.

use serde::{Deserialize, Serialize};

/// The method names.
pub mod method {
    pub const MESSAGE_SEND: &str = "agent.message_send";
    pub const MESSAGE_CLAIM: &str = "agent.message_claim";
}

/// `agent.message_send`: deliver one agent message to `target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentMessageSendParams {
    /// The target agent (pane id or agent name, as `agent.prompt`).
    pub target: String,
    /// The message id (`m...`); the key of every log line about it.
    pub id: String,
    /// The text typed into the target (the sender's envelope around the body).
    pub envelope: String,
    /// The message body as the log shows it.
    pub text: String,
    /// When the sender sent it (unix seconds); `0` = now.
    #[serde(default)]
    pub unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The target's name as the sender knows it (the log's `to_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_name: Option<String>,
}

/// What `agent.message_send` did.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageOutcome {
    /// Typed into the target now; the sender logs it.
    #[default]
    Sent,
    /// Queued by the server (which logged it); typed in once the target is free.
    Queued,
    #[serde(other)]
    Unknown,
}

/// `agent.message_claim`: the target took a queued message some other way
/// (agents_wait_for_message returned it), so it is not typed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentMessageClaimParams {
    pub id: String,
    /// The claiming pane: only the message's target may claim it.
    pub pane: String,
}
