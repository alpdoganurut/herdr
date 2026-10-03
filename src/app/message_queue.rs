//! Agent message delivery (fork): `agent.message_send`, `agent.message_claim`
//! and the server-side queue behind them. The agents model's
//! `agents.send_message` (src/app/agents_model.rs) checks, limits and builds
//! its envelope, then delivers through [`App::deliver_agent_message`] here:
//! one delivery path for both methods.
//!
//! A `herdr_agents` message is typed into its target now when the target can
//! take it (its agent is live, idle or done, not starting, its user is not
//! typing in it or holding an unsent draft, and for the coordinator no turn
//! is live). Otherwise the server queues it (never a refusal) and types it in
//! once the target is free: idle for [`SETTLE`], typing guard clear. Per
//! target FIFO; when several wait for one target they go in as one paste,
//! each with its own envelope and id. A message is never typed into a blocked
//! target (a permission dialog) or a suspended one: it waits. It expires after
//! [`MESSAGE_TTL_S`] and is dropped when its target pane or agent is gone for
//! [`GONE_GRACE`] (long enough for a restart to detect the agent again).
//!
//! The log (`messages.jsonl` in the coordinator directory) stays append-only:
//! the queued message is logged in full with outcome `queued`, and later
//! `update` lines move it to `delivered`, `expired` or `dropped`. A waiter in
//! agents_wait_for_message that returns a queued reply claims it, so it is not
//! typed in as well.
//!
//! Compatibility shim: agents started before this build keep their old
//! `herdr coordinator mcp` (`plus mcp` in the POC) until they restart, and
//! that server calls `agent.prompt` itself (request ids `coordinator:…` /
//! `plus:…`), the oldest without the typing guard. Such a request is taken
//! for an agent message: the typing guard always applies, and when the target
//! cannot take it now it is queued (update lines only: the old sender logs
//! the message itself) and answered as a success, so the old sender neither
//! reports a refusal nor resends. A user's own `herdr agent prompt` (`cli:…`)
//! is untouched.
//!
//! Delivery is driven by agent events (`emit_event` marks the queue due, O(1)
//! when it is empty) and the server loop's deadlines (settle, typing retry,
//! expiry); nothing runs in render paths. File work (log lines and the queue
//! file, `message_queue.json` next to `session.json`, so it survives restarts
//! and live handoffs) goes through one writer thread, in order. The coordinator
//! turn marker is the exception: it is written on the server loop right
//! before a message is typed into the coordinator, as the MCP server did.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{
    AgentMessageClaimParams, AgentMessageOutcome, AgentMessageSendParams, AgentPromptParams,
    ResponseResult,
};
use crate::coordinator::messages::{
    self, AgentMessage, OUTCOME_DELIVERED, OUTCOME_DROPPED, OUTCOME_EXPIRED, OUTCOME_QUEUED,
};

/// How long a queued message waits before it expires.
pub(crate) const MESSAGE_TTL_S: u64 = 2 * 3600;
/// How long a target stays idle before queued messages are typed in.
pub(crate) const SETTLE: Duration = Duration::from_secs(3);
/// Retry interval while the user types in the target (or a delivery failed).
pub(crate) const RETRY: Duration = Duration::from_secs(3);
/// How long a target may be missing before its messages are dropped.
pub(crate) const GONE_GRACE: Duration = Duration::from_secs(60);
/// At most this many queued messages go into one paste.
const MAX_COMBINED: usize = 8;
const MAX_COMBINED_CHARS: usize = 16_000;
/// The whole queue's cap; past it `agent.message_send` answers `queue_full`.
const MAX_QUEUED: usize = 256;
const FILE_NAME: &str = "message_queue.json";
const FILE_VERSION: u32 = 1;
/// Request id prefixes of the `herdr coordinator mcp` / POC `plus mcp` socket
/// adapters (`coordinator:agent.prompt`, `plus:agent.prompt`).
const LEGACY_SENDER_PREFIXES: [&str; 2] = ["coordinator:", "plus:"];

/// One queued message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QueuedMessage {
    /// The log line (outcome `queued`); `to_pane` is the target's pane id
    /// when it was queued.
    pub(crate) message: AgentMessage,
    /// What is typed in.
    pub(crate) envelope: String,
    pub(crate) terminal_id: String,
    /// The target agent's native session, to find it again after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    pub(crate) expires_unix: u64,
    /// From an older sender that logs the message itself (only update lines
    /// are written here).
    #[serde(default)]
    pub(crate) legacy: bool,
    /// The sender's terminal, for the target's turn origin (agents v2);
    /// `None` for an older sender (the delivery counts as an API write).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from_terminal: Option<crate::terminal::TerminalId>,
}

impl QueuedMessage {
    fn id(&self) -> &str {
        self.message.id.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct QueueFile {
    version: u32,
    #[serde(default)]
    messages: Vec<QueuedMessage>,
}

enum IoJob {
    Append(PathBuf, Box<AgentMessage>),
    Save(PathBuf, Vec<u8>),
}

/// The server's message queue (App state; see the module docs).
#[derive(Default)]
pub(crate) struct MessageQueue {
    pub(crate) entries: Vec<QueuedMessage>,
    /// An agent event arrived since the last pass.
    due: bool,
    /// The earliest settle / retry / grace deadline.
    retry_at: Option<Instant>,
    /// When each target terminal was first seen deliverable (settle).
    ready_since: HashMap<String, Instant>,
    /// When each target terminal was first seen missing (grace).
    missing_since: HashMap<String, Instant>,
    /// `message_queue.json`; `None` = not persisted (tests, unpersisted sessions).
    store: Option<PathBuf>,
    /// The coordinator directory (the message log); `None` = no log lines.
    log_dir: Option<PathBuf>,
    io: Option<mpsc::Sender<IoJob>>,
    /// Log lines written (tests).
    #[cfg(test)]
    pub(crate) written: Vec<AgentMessage>,
}

/// Why a target cannot take a message now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Check {
    /// Deliverable: its public pane id, and whether it is the coordinator.
    Ready { pane: String, coordinator: bool },
    /// Not now (the reason is shown to the sender).
    Wait(String),
    /// The pane or its agent is gone.
    Missing,
}

impl MessageQueue {
    /// The queue for a server: loaded from `message_queue.json` next to
    /// `session.json` when the session is persisted.
    pub(crate) fn new(persisted: bool, log_dir: PathBuf) -> Self {
        let mut queue = Self {
            log_dir: Some(log_dir),
            ..Self::default()
        };
        if persisted {
            queue.load(crate::session::data_dir().join(FILE_NAME));
        }
        queue
    }

    /// Take the queue saved at `path` (a restart or a live handoff) and keep
    /// saving there.
    fn load(&mut self, path: PathBuf) {
        self.entries = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<QueueFile>(&bytes).ok())
            .map(|file| file.messages)
            .unwrap_or_default();
        self.store = Some(path);
        // A restart looks at every target again.
        self.due = !self.entries.is_empty();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// An agent event that may make a target deliverable (or gone).
    pub(crate) fn mark_due(&mut self) {
        if !self.entries.is_empty() {
            self.due = true;
        }
    }

    fn log(&mut self, line: AgentMessage) {
        #[cfg(test)]
        self.written.push(line.clone());
        if let Some(dir) = self.log_dir.clone() {
            self.submit(IoJob::Append(dir, Box::new(line)));
        }
    }

    fn save(&mut self) {
        let Some(path) = self.store.clone() else {
            return;
        };
        let file = QueueFile {
            version: FILE_VERSION,
            messages: self.entries.clone(),
        };
        match serde_json::to_vec_pretty(&file) {
            Ok(bytes) => self.submit(IoJob::Save(path, bytes)),
            Err(err) => tracing::warn!(error = %err, "message queue: cannot encode the queue"),
        }
    }

    fn submit(&mut self, job: IoJob) {
        #[cfg(test)]
        if self.store.is_none() {
            // Unit tests keep the log in `written` only.
            return;
        }
        if self.io.is_none() {
            let (tx, rx) = mpsc::channel::<IoJob>();
            let spawned = std::thread::Builder::new()
                .name("herdr-message-queue".into())
                .spawn(move || {
                    for job in rx {
                        let result = match &job {
                            IoJob::Append(dir, line) => messages::append(dir, line),
                            IoJob::Save(path, bytes) => {
                                crate::coordinator::write_atomically(path, bytes)
                            }
                        };
                        if let Err(err) = result {
                            tracing::warn!(error = %err, "message queue: file write failed");
                        }
                    }
                });
            match spawned {
                Ok(_) => self.io = Some(tx),
                Err(err) => {
                    tracing::warn!(error = %err, "message queue: cannot start the writer");
                    return;
                }
            }
        }
        if let Some(io) = &self.io {
            let _ = io.send(job);
        }
    }

    fn push(&mut self, entry: QueuedMessage) {
        let mut line = entry.message.clone();
        line.outcome = OUTCOME_QUEUED.into();
        if entry.legacy {
            self.log(AgentMessage::update(&line, OUTCOME_QUEUED, line.unix));
        } else {
            self.log(line);
        }
        self.entries.push(entry);
        self.due = true;
        self.save();
    }

    fn finish(&mut self, ids: &[String], outcome: &str, now_unix: u64) {
        let mut done = Vec::new();
        self.entries.retain(|entry| {
            if ids.iter().any(|id| id == entry.id()) {
                done.push(entry.message.clone());
                false
            } else {
                true
            }
        });
        for message in done {
            self.log(AgentMessage::update(&message, outcome, now_unix));
        }
        self.save();
    }

    fn queued_for(&self, terminal_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.terminal_id == terminal_id)
    }

    /// When the server loop should run the next pass.
    pub(crate) fn next_deadline(&self, now: Instant, now_unix: u64) -> Option<Instant> {
        if self.entries.is_empty() {
            return None;
        }
        if self.due {
            return Some(now);
        }
        let expiry = self
            .entries
            .iter()
            .map(|entry| now + Duration::from_secs(entry.expires_unix.saturating_sub(now_unix)))
            .min();
        [self.retry_at, expiry].into_iter().flatten().min()
    }
}

/// Whether an `agent.prompt` request comes from an older `herdr_agents` MCP server.
pub(crate) fn is_legacy_agent_message(request_id: &str) -> bool {
    LEGACY_SENDER_PREFIXES
        .iter()
        .any(|prefix| request_id.starts_with(prefix))
}

/// The message id an envelope carries (`[herdr+ message m… …`).
fn envelope_id(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("[herdr+ message ")?;
    let id: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect();
    messages::is_id(&id).then_some(id)
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Errors from a typing attempt that mean "not now" rather than "never".
fn retryable(code: &str) -> bool {
    matches!(code, "user_typing" | "agent_blocked" | "agent_not_ready")
}

/// A target's deliverability, read once by the sender (opaque outside).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageCheck(Check);

impl MessageCheck {
    /// The sender-facing status: `idle`, why it waits, or `offline`.
    pub(crate) fn status(&self) -> String {
        match &self.0 {
            Check::Ready { .. } => "idle".to_string(),
            Check::Wait(reason) => reason.clone(),
            Check::Missing => "offline".to_string(),
        }
    }
}

/// How [`App::deliver_agent_message`] delivered a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MessageDelivery {
    /// Typed in now (the caller logs it as `sent`).
    Sent,
    /// Queued (and logged `queued`); why it was not typed in now.
    Queued { reason: String },
}

/// Why a message was neither typed in nor queued: an error code and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageRefused {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl MessageRefused {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    fn from_response(response: &str) -> Self {
        let value = serde_json::from_str::<serde_json::Value>(response).unwrap_or_default();
        Self {
            code: value["error"]["code"]
                .as_str()
                .unwrap_or("failed")
                .to_string(),
            message: value["error"]["message"]
                .as_str()
                .unwrap_or("the prompt failed")
                .to_string(),
        }
    }
}

/// What the shim does with an older sender's `agent.prompt`.
pub(crate) enum LegacyPrompt {
    /// Type it now through the normal path (the typing guard forced on).
    TypeNow(AgentPromptParams),
    /// Answered already (queued, or an error).
    Answered(String),
}

impl App {
    /// The live target of a queued message: its terminal (by id, else by the
    /// agent's session after a restart) and pane.
    fn message_target(
        &self,
        terminal_id: &str,
        session: Option<&str>,
    ) -> Option<super::terminal_targets::TerminalTarget> {
        if let Ok(target) = self.resolve_terminal_target(terminal_id) {
            if target.terminal_id == terminal_id {
                return Some(target);
            }
        }
        let session = session?;
        let terminal = self.state.terminals.values().find(|terminal| {
            super::coordinator::terminal_session(terminal).as_deref() == Some(session)
        })?;
        let id = terminal.id.to_string();
        self.resolve_terminal_target(&id)
            .ok()
            .filter(|target| target.terminal_id == id)
    }

    /// Whether the agent in this pane can take a message now.
    fn message_check(&self, target: &super::terminal_targets::TerminalTarget) -> Check {
        let Some(pane) = self.public_pane_id(target.ws_idx, target.pane_id) else {
            return Check::Missing;
        };
        let Some(terminal) = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
        else {
            return Check::Missing;
        };
        if terminal.suspended_agent.is_some() {
            return Check::Wait("suspended".into());
        }
        if terminal.effective_known_agent().is_none() {
            return Check::Missing;
        }
        match terminal.state {
            crate::detect::AgentState::Idle => {}
            crate::detect::AgentState::Working => return Check::Wait("working".into()),
            crate::detect::AgentState::Blocked => {
                return Check::Wait("blocked on its user (a question or an approval)".into())
            }
            crate::detect::AgentState::Unknown => return Check::Wait("status unknown".into()),
        }
        if terminal.managed_agent_launch_pending()
            || (terminal.managed_agent_kind().is_some()
                && !terminal.managed_agent_interactive_ready())
        {
            return Check::Wait("starting".into());
        }
        let coordinator = self.is_coordinator_pane(target.ws_idx, target.pane_id);
        if coordinator {
            if let Some(reason) = self.coordinator_message_hold() {
                return Check::Wait(reason);
            }
        }
        Check::Ready { pane, coordinator }
    }

    /// Type `text` into `pane` now (guarded). For the coordinator the turn
    /// marker is written first; it is cleared again when nothing was typed.
    /// `Err` carries the error response.
    fn type_message(
        &mut self,
        request_id: String,
        pane: &str,
        text: String,
        coordinator: bool,
        message_id: &str,
        from: Option<&crate::terminal::TerminalId>,
    ) -> Result<(), String> {
        let marker = if coordinator {
            match self.write_coordinator_message_turn(pane, message_id) {
                Ok(marker) => Some(marker),
                Err(reason) => {
                    return Err(encode_error(request_id, "busy", reason));
                }
            }
        } else {
            None
        };
        let queued = self.queue_agent_prompt(
            request_id,
            AgentPromptParams {
                target: pane.to_string(),
                text,
                wait: None,
                guard_user_typing: true,
            },
            crate::agents_model::InputSource::Programmatic(match from {
                Some(from) => crate::agents_model::Programmatic::AgentMessage {
                    id: message_id.to_string(),
                    from: from.clone(),
                },
                None => crate::agents_model::Programmatic::Api,
            }),
        );
        match queued {
            Ok(_) => Ok(()),
            Err(response) => {
                if let Some(marker) = marker {
                    crate::coordinator::turn::clear_if(&self.coordinator.dir, &marker);
                }
                Err(response)
            }
        }
    }

    fn queued_message(
        &self,
        target: &super::terminal_targets::TerminalTarget,
        message: AgentMessage,
        envelope: String,
        legacy: bool,
        from_terminal: Option<crate::terminal::TerminalId>,
        now_unix: u64,
    ) -> QueuedMessage {
        let session = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
            .and_then(super::coordinator::terminal_session);
        QueuedMessage {
            message,
            envelope,
            terminal_id: target.terminal_id.clone(),
            session,
            expires_unix: now_unix + MESSAGE_TTL_S,
            legacy,
            from_terminal,
        }
    }

    /// `agent.message_send`.
    pub(crate) fn handle_agent_message_send(
        &mut self,
        id: String,
        params: AgentMessageSendParams,
    ) -> String {
        let now_unix = crate::coordinator::now_unix();
        if !messages::is_id(&params.id) || params.envelope.trim().is_empty() {
            return encode_error(id, "invalid_request", "a message id and text are required");
        }
        let Ok(target) = self.resolve_agent_target(&params.target) else {
            return encode_error(id, "offline", format!("{} is not running", params.target));
        };
        let check = self.agent_message_check(&target);
        let to_name = params.to_name.or_else(|| {
            self.state
                .terminals
                .values()
                .find(|terminal| terminal.id.to_string() == target.terminal_id)
                .and_then(|terminal| terminal.agent_name.clone())
        });
        let message = AgentMessage {
            unix: if params.unix == 0 {
                now_unix
            } else {
                params.unix
            },
            from_pane: params.from_pane,
            from_name: params.from_name,
            to_pane: self
                .public_pane_id(target.ws_idx, target.pane_id)
                .unwrap_or_else(|| params.target.clone()),
            to_name,
            text: params.text,
            outcome: OUTCOME_QUEUED.into(),
            id: Some(params.id.clone()),
            reply_to: params.reply_to,
            from_role: params.from_role,
            kind: None,
            team: params.team,
        };
        let status = check.status();
        // An older sender of this method logs a message it was told was
        // typed in; the server logs the queued ones.
        let from_terminal = message
            .from_pane
            .as_deref()
            .and_then(|pane| self.resolve_agent_target(pane).ok())
            .map(|from| from.terminal_id)
            .and_then(|from| {
                self.state
                    .terminals
                    .keys()
                    .find(|terminal| terminal.as_str() == from)
                    .cloned()
            });
        match self.deliver_agent_message(
            &id,
            &target,
            check,
            message,
            params.envelope,
            from_terminal,
            now_unix,
        ) {
            Ok(MessageDelivery::Sent) => encode_success(
                id,
                ResponseResult::AgentMessageSend {
                    id: params.id,
                    outcome: AgentMessageOutcome::Sent,
                    status,
                    reason: None,
                },
            ),
            Ok(MessageDelivery::Queued { reason }) => encode_success(
                id,
                ResponseResult::AgentMessageSend {
                    id: params.id,
                    outcome: AgentMessageOutcome::Queued,
                    status,
                    reason: Some(reason),
                },
            ),
            Err(refused) => encode_error(id, &refused.code, refused.message),
        }
    }

    /// Whether the agent in `target` can take a message now (for
    /// [`App::deliver_agent_message`] and the sender's status line).
    pub(crate) fn agent_message_check(
        &self,
        target: &super::terminal_targets::TerminalTarget,
    ) -> MessageCheck {
        MessageCheck(self.message_check(target))
    }

    /// The one delivery path for an agent message (`agent.message_send` and
    /// `agents.send_message`): typed in now when the target can take it
    /// (guarded) and nothing is queued for it, else queued. `message` is its
    /// log line (the queue logs it as `queued`; a message typed in now is
    /// logged by the caller); `from` is the sender's terminal, the target's
    /// turn origin.
    #[allow(clippy::too_many_arguments)] // One message's parts, each from a different source.
    pub(crate) fn deliver_agent_message(
        &mut self,
        request_id: &str,
        target: &super::terminal_targets::TerminalTarget,
        check: MessageCheck,
        message: AgentMessage,
        envelope: String,
        from: Option<crate::terminal::TerminalId>,
        now_unix: u64,
    ) -> Result<MessageDelivery, MessageRefused> {
        let message_id = message.id.clone().unwrap_or_default();
        let reason = match check.0 {
            Check::Missing => {
                return Err(MessageRefused::new(
                    "offline",
                    format!("{} is not running", message.to_pane),
                ))
            }
            _ if self.message_queue.queued_for(&target.terminal_id) => {
                "earlier messages are waiting for it".to_string()
            }
            Check::Wait(reason) => reason,
            Check::Ready { pane, coordinator } => {
                match self.type_message(
                    request_id.to_string(),
                    &pane,
                    envelope.clone(),
                    coordinator,
                    &message_id,
                    from.as_ref(),
                ) {
                    Ok(()) => return Ok(MessageDelivery::Sent),
                    Err(response) => {
                        let code = error_code(&response);
                        if !retryable(&code) && code != "busy" {
                            return Err(MessageRefused::from_response(&response));
                        }
                        match code.as_str() {
                            "user_typing" => "its user is typing in it".to_string(),
                            "busy" => "a coordinator turn is live".to_string(),
                            _ => "not ready".to_string(),
                        }
                    }
                }
            }
        };
        if self.message_queue.entries.len() >= MAX_QUEUED {
            return Err(MessageRefused::new(
                "queue_full",
                "too many queued agent messages",
            ));
        }
        let entry = self.queued_message(target, message, envelope, false, from, now_unix);
        self.message_queue.push(entry);
        Ok(MessageDelivery::Queued { reason })
    }

    /// `agent.message_claim`.
    pub(crate) fn handle_agent_message_claim(
        &mut self,
        id: String,
        params: AgentMessageClaimParams,
    ) -> String {
        let claimer = self
            .resolve_agent_target(&params.pane)
            .or_else(|_| self.resolve_terminal_target(&params.pane))
            .ok()
            .map(|target| target.terminal_id);
        let claimed = claimer.is_some_and(|terminal_id| {
            self.message_queue
                .entries
                .iter()
                .any(|entry| entry.id() == params.id && entry.terminal_id == terminal_id)
        });
        if claimed {
            self.message_queue.finish(
                std::slice::from_ref(&params.id),
                OUTCOME_DELIVERED,
                crate::coordinator::now_unix(),
            );
        }
        encode_success(id, ResponseResult::AgentMessageClaim { claimed })
    }

    /// The compatibility shim for an older sender's `agent.prompt` (module docs).
    pub(crate) fn legacy_agent_message(
        &mut self,
        request_id: &str,
        mut params: AgentPromptParams,
    ) -> LegacyPrompt {
        params.guard_user_typing = true;
        let Ok(target) = self.resolve_agent_target(&params.target) else {
            return LegacyPrompt::TypeNow(params);
        };
        if !self.message_queue.queued_for(&target.terminal_id)
            && matches!(self.message_check(&target), Check::Ready { .. })
        {
            return LegacyPrompt::TypeNow(params);
        }
        LegacyPrompt::Answered(self.queue_legacy_message(request_id, params))
    }

    /// Queue an older sender's prompt and answer it as typed in. Without a
    /// recognisable target it answers like `agent.prompt` would.
    pub(crate) fn queue_legacy_message(
        &mut self,
        request_id: &str,
        params: AgentPromptParams,
    ) -> String {
        let now_unix = crate::coordinator::now_unix();
        let Ok(target) = self.resolve_agent_target(&params.target) else {
            return encode_error(
                request_id.to_string(),
                "agent_not_found",
                format!("agent {} not found", params.target),
            );
        };
        let Some(agent) = self.agent_info(target.ws_idx, target.pane_id) else {
            return encode_error(
                request_id.to_string(),
                "agent_not_found",
                format!("agent {} not found", params.target),
            );
        };
        if self.message_queue.entries.len() >= MAX_QUEUED {
            return encode_error(
                request_id.to_string(),
                "queue_full",
                "too many queued agent messages",
            );
        }
        let message_id = envelope_id(&params.text);
        let message = AgentMessage {
            unix: now_unix,
            to_pane: self
                .public_pane_id(target.ws_idx, target.pane_id)
                .unwrap_or_else(|| params.target.clone()),
            outcome: OUTCOME_QUEUED.into(),
            // Without an id no log line can name it; it is still delivered.
            id: message_id.or_else(|| Some(messages::new_id())),
            ..AgentMessage::default()
        };
        let entry = self.queued_message(&target, message, params.text, true, None, now_unix);
        self.message_queue.push(entry);
        tracing::info!(
            request = request_id,
            target = %params.target,
            "message queue: an older herdr_agents server's prompt was queued"
        );
        encode_success(
            request_id.to_string(),
            ResponseResult::AgentPrompted { agent },
        )
    }

    /// Mark the queue due on events that can change a target's deliverability.
    pub(crate) fn note_message_queue_event(&mut self, event: &crate::api::schema::EventKind) {
        use crate::api::schema::EventKind;
        if self.message_queue.is_empty() {
            return;
        }
        if matches!(
            event,
            EventKind::PaneAgentStatusChanged
                | EventKind::PaneAgentDetected
                | EventKind::PaneClosed
                | EventKind::PaneExited
                | EventKind::PaneMoved
                | EventKind::TabClosed
                | EventKind::WorkspaceClosed
        ) {
            self.message_queue.mark_due();
        }
    }

    pub(crate) fn next_message_queue_deadline(&self, now: Instant) -> Option<Instant> {
        self.message_queue
            .next_deadline(now, crate::coordinator::now_unix())
    }

    /// One delivery pass (the server loop, at the queue's deadline). Returns
    /// whether anything was typed in or removed.
    pub(crate) fn handle_message_queue_tasks(&mut self, now: Instant) -> bool {
        let now_unix = crate::coordinator::now_unix();
        self.message_queue_pass(now, now_unix)
    }

    pub(crate) fn message_queue_pass(&mut self, now: Instant, now_unix: u64) -> bool {
        if self.message_queue.is_empty() {
            return false;
        }
        let due = self.message_queue.due
            || self.message_queue.retry_at.is_some_and(|at| now >= at)
            || self
                .message_queue
                .entries
                .iter()
                .any(|entry| entry.expires_unix <= now_unix);
        if !due {
            return false;
        }
        self.message_queue.due = false;
        self.message_queue.retry_at = None;
        let mut changed = false;

        let expired: Vec<String> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.expires_unix <= now_unix)
            .map(|entry| entry.id().to_string())
            .collect();
        if !expired.is_empty() {
            self.message_queue
                .finish(&expired, OUTCOME_EXPIRED, now_unix);
            changed = true;
        }

        // Targets in the order their oldest message was queued.
        let mut targets: Vec<(String, Option<String>)> = Vec::new();
        for entry in &self.message_queue.entries {
            if !targets.iter().any(|(id, _)| *id == entry.terminal_id) {
                targets.push((entry.terminal_id.clone(), entry.session.clone()));
            }
        }
        let mut next: Vec<Instant> = Vec::new();
        for (terminal_id, session) in targets {
            changed |=
                self.deliver_queued_to(&terminal_id, session.as_deref(), now, now_unix, &mut next);
        }
        let live: Vec<String> = self
            .message_queue
            .entries
            .iter()
            .map(|entry| entry.terminal_id.clone())
            .collect();
        self.message_queue
            .ready_since
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue
            .missing_since
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue.retry_at = next.into_iter().min();
        changed
    }

    fn deliver_queued_to(
        &mut self,
        terminal_id: &str,
        session: Option<&str>,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let Some(target) = self.message_target(terminal_id, session) else {
            return self.target_missing(terminal_id, now, now_unix, next);
        };
        if target.terminal_id != terminal_id {
            // Found again under a new terminal id (a restart): follow it.
            for entry in &mut self.message_queue.entries {
                if entry.terminal_id == terminal_id {
                    entry.terminal_id = target.terminal_id.clone();
                }
            }
            self.message_queue.save();
        }
        let terminal_id = target.terminal_id.clone();
        let (pane, coordinator) = match self.message_check(&target) {
            Check::Missing => return self.target_missing(&terminal_id, now, now_unix, next),
            Check::Wait(_) => {
                self.message_queue.missing_since.remove(&terminal_id);
                self.message_queue.ready_since.remove(&terminal_id);
                return false;
            }
            Check::Ready { pane, coordinator } => (pane, coordinator),
        };
        self.message_queue.missing_since.remove(&terminal_id);
        let since = *self
            .message_queue
            .ready_since
            .entry(terminal_id.clone())
            .or_insert(now);
        if now < since + SETTLE {
            next.push(since + SETTLE);
            return false;
        }
        // One paste: the oldest messages for this target, each with its envelope.
        let mut ids = Vec::new();
        let mut text = String::new();
        for entry in self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == terminal_id)
        {
            if !ids.is_empty()
                && (ids.len() >= MAX_COMBINED
                    || text.len() + entry.envelope.len() > MAX_COMBINED_CHARS)
            {
                break;
            }
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&entry.envelope);
            ids.push(entry.id().to_string());
        }
        let first = ids.first().cloned().unwrap_or_default();
        // One paste, one turn: its origin is the first message's sender.
        let from = self
            .message_queue
            .entries
            .iter()
            .find(|entry| entry.id() == first)
            .and_then(|entry| entry.from_terminal.clone());
        match self.type_message(
            format!("agent-message:{first}"),
            &pane,
            text,
            coordinator,
            &first,
            from.as_ref(),
        ) {
            Ok(()) => {
                self.message_queue.ready_since.remove(&terminal_id);
                self.message_queue.finish(&ids, OUTCOME_DELIVERED, now_unix);
                if self.message_queue.queued_for(&terminal_id) {
                    // The rest goes in once the target settled again.
                    self.message_queue.due = true;
                }
                true
            }
            Err(response) => {
                let code = error_code(&response);
                if !retryable(&code) && code != "busy" {
                    tracing::warn!(code, "message queue: delivery failed, retrying");
                }
                next.push(now + RETRY);
                false
            }
        }
    }

    fn target_missing(
        &mut self,
        terminal_id: &str,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        self.message_queue.ready_since.remove(terminal_id);
        let since = *self
            .message_queue
            .missing_since
            .entry(terminal_id.to_string())
            .or_insert(now);
        if now < since + GONE_GRACE {
            next.push(since + GONE_GRACE);
            return false;
        }
        let ids: Vec<String> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == terminal_id)
            .map(|entry| entry.id().to_string())
            .collect();
        self.message_queue.finish(&ids, OUTCOME_DROPPED, now_unix);
        self.message_queue.missing_since.remove(terminal_id);
        true
    }
}

#[cfg(test)]
mod tests;
