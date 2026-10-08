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
//! once the target is free: idle for [`SETTLE`], typing guard clear. A
//! target whose screen reads working but whose own hooks reported its turn's
//! end (src/app/hook_turn.rs: Claude's title spinner while background agents
//! run) counts as free too, always through the queue so SETTLE applies. Per
//! target FIFO; when several wait for one target they go in together, each
//! with its own envelope and id. A target whose herdr_agents server reads
//! messages by id gets one typed pointer line for them instead of the paste
//! (src/app/message_pointer.rs). A message is never typed into a blocked
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
use super::message_pointer::{DeliveredMessage, Outgoing};
use super::App;
use crate::agents_model::envelope::PointerFrom;
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
/// A queued message has waited this long on a target that reads working
/// while its screen above the prompt box has not changed for
/// [`STUCK_SCREEN`]: its sender gets one herdr notice.
pub(crate) const STUCK_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long the screen above the prompt box must stay the same.
pub(crate) const STUCK_SCREEN: Duration = Duration::from_secs(10 * 60);
/// At most one stuck notice per target this often.
pub(crate) const STUCK_NOTICE_EVERY: Duration = Duration::from_secs(10 * 60);
/// How often a target that waits on `working` is sampled.
pub(crate) const STUCK_SAMPLE: Duration = Duration::from_secs(60);
/// After an urgent message's Esc: no second Esc into the same target for
/// this long, and the turn end it waits for is looked for this long; past
/// it the message simply stays queued.
pub(crate) const INTERRUPT_WAIT: Duration = Duration::from_secs(10);
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
    /// The sender as a pointer line names it; `None` from an older queue
    /// file or sender (read from the envelope's header).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pointer: Option<PointerFrom>,
    /// Its sender was told it seems stuck (at most once per message).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) stuck_notified: bool,
}

impl QueuedMessage {
    fn id(&self) -> &str {
        self.message.id.as_deref().unwrap_or("")
    }

    /// The message on its way in.
    fn outgoing(&self) -> Outgoing {
        Outgoing {
            id: self.id().to_string(),
            envelope: self.envelope.clone(),
            pointer: self
                .pointer
                .clone()
                .unwrap_or_else(|| PointerFrom::from_envelope(self.id(), &self.envelope)),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct QueueFile {
    version: u32,
    #[serde(default)]
    messages: Vec<QueuedMessage>,
    /// Envelopes of messages typed in as a pointer line (older builds
    /// ignore the field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    delivered: Vec<DeliveredMessage>,
}

/// One target's screen samples for the stuck check.
#[derive(Debug, Clone, Copy)]
struct ScreenWatch {
    /// A hash of the detection text above the prompt box.
    hash: u64,
    /// Since when that text has not changed.
    since: Instant,
    sampled: Instant,
}

enum IoJob {
    Append(PathBuf, Box<AgentMessage>),
    Save(PathBuf, Vec<u8>),
}

/// The server's message queue (App state; see the module docs).
#[derive(Default)]
pub(crate) struct MessageQueue {
    pub(crate) entries: Vec<QueuedMessage>,
    /// Envelopes of messages typed in as a pointer line, for
    /// `agents.read_messages` (src/app/message_pointer.rs).
    pub(crate) delivered: Vec<DeliveredMessage>,
    /// An agent event arrived since the last pass.
    due: bool,
    /// The earliest settle / retry / grace deadline.
    retry_at: Option<Instant>,
    /// When each target terminal was first seen deliverable (settle).
    ready_since: HashMap<String, Instant>,
    /// When each target terminal was first seen missing (grace).
    missing_since: HashMap<String, Instant>,
    /// Targets waiting on `working`: the screen above their prompt box as
    /// last sampled (runtime only).
    screens: HashMap<String, ScreenWatch>,
    /// When each target's last stuck notice went out.
    stuck_notice_at: HashMap<String, Instant>,
    /// Targets an urgent message interrupted (Esc), and when.
    interrupted_at: HashMap<String, Instant>,
    /// `message_queue.json`; `None` = not persisted (tests, unpersisted sessions).
    pub(super) store: Option<PathBuf>,
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
    /// `hook_ended`: its screen reads working but its own hooks reported
    /// its turn's end (src/app/hook_turn.rs); typed in only after
    /// [`SETTLE`], through the queue.
    Ready {
        pane: String,
        coordinator: bool,
        hook_ended: bool,
    },
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
    pub(super) fn load(&mut self, path: PathBuf) {
        let file = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<QueueFile>(&bytes).ok())
            .unwrap_or_default();
        self.entries = file.messages;
        self.delivered = file.delivered;
        super::message_pointer::prune_delivered(
            &mut self.delivered,
            crate::coordinator::now_unix(),
        );
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

    pub(super) fn save(&mut self) {
        let Some(path) = self.store.clone() else {
            return;
        };
        let file = QueueFile {
            version: FILE_VERSION,
            messages: self.entries.clone(),
            delivered: self.delivered.clone(),
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
        self.push_at(entry, false);
    }

    /// Queue `entry`; `front`: ahead of every message queued for the same
    /// target (an urgent message).
    fn push_at(&mut self, entry: QueuedMessage, front: bool) {
        let mut line = entry.message.clone();
        line.outcome = OUTCOME_QUEUED.into();
        if entry.legacy {
            self.log(AgentMessage::update(&line, OUTCOME_QUEUED, line.unix));
        } else {
            self.log(line);
        }
        let at = front
            .then(|| {
                self.entries
                    .iter()
                    .position(|queued| queued.terminal_id == entry.terminal_id)
            })
            .flatten()
            .unwrap_or(self.entries.len());
        self.entries.insert(at, entry);
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

/// A short, stable hash of the detection text above the prompt box.
fn screen_hash(detection_text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    crate::detect::manifest::above_prompt_box_text(detection_text).hash(&mut hasher);
    hasher.finish()
}

/// `12m`, `1h05m`; under a minute rounds up to `1m`.
fn minutes(duration: Duration) -> String {
    let total = duration.as_secs().div_ceil(60).max(1);
    if total < 60 {
        format!("{total}m")
    } else {
        format!("{}h{:02}m", total / 60, total % 60)
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
        // An agent herdr is starting is not detected yet: its messages wait
        // for it (a launch that never comes up ends as missing, then the
        // queue's grace drops them).
        if terminal.managed_agent_launch_pending() {
            return Check::Wait("starting".into());
        }
        if terminal.effective_known_agent().is_none() {
            return Check::Missing;
        }
        let hook_ended = terminal.state == crate::detect::AgentState::Working
            && terminal.turn().hook_turn_ended().is_some();
        match terminal.state {
            crate::detect::AgentState::Idle => {}
            // Its own hooks say its last turn ended (src/app/hook_turn.rs):
            // the title spinner of background work is not a turn.
            crate::detect::AgentState::Working if hook_ended => {}
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
        // Fork: a voice session (live or muted) is the user talking to the
        // agent; typed text would land in it (src/app/voice.rs).
        if terminal.agent_voice().active() {
            return Check::Wait("voice mode".into());
        }
        let coordinator = self.is_coordinator_pane(target.ws_idx, target.pane_id);
        if coordinator {
            if let Some(reason) = self.coordinator_message_hold() {
                return Check::Wait(reason);
            }
        }
        Check::Ready {
            pane,
            coordinator,
            hook_ended,
        }
    }

    /// Type `items` into `pane` now (guarded): one pointer line when the
    /// target reads messages by id, else the envelopes as one paste. For the
    /// coordinator the turn marker is written first; it is cleared again
    /// when nothing was typed. The first item's id is the turn's message.
    /// `Err` carries the error response.
    #[allow(clippy::too_many_arguments)] // One delivery's parts, each from a different source.
    fn type_message(
        &mut self,
        request_id: String,
        terminal_id: &str,
        pane: &str,
        items: &[Outgoing],
        coordinator: bool,
        from: Option<&crate::terminal::TerminalId>,
    ) -> Result<(), String> {
        let message_id = items.first().map(|item| item.id.as_str()).unwrap_or("");
        let input = self.message_input(terminal_id, items);
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
        let queued = self.queue_agent_prompt_as(
            request_id,
            AgentPromptParams {
                target: pane.to_string(),
                text: input.text,
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
            input.typed,
        );
        match queued {
            Ok(_) => {
                if input.typed {
                    self.remember_delivered(terminal_id, items);
                }
                Ok(())
            }
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
        pointer: Option<PointerFrom>,
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
            pointer,
            stuck_notified: false,
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
        // The pointer facts from the envelope's header, with the sender
        // this method names.
        let mut pointer = PointerFrom::from_envelope(&params.id, &params.envelope);
        pointer.reply_to = message.reply_to.clone();
        if let Some(pane) = message.from_pane.clone() {
            pointer.pane = pane;
        }
        if let Some(name) = message.from_name.clone() {
            pointer.name = name;
        }
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
            pointer,
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
        pointer: PointerFrom,
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
            // Its turn ended a moment ago by its hooks: the queue types it
            // in once that held for SETTLE.
            Check::Ready {
                hook_ended: true, ..
            } => "its turn just ended".to_string(),
            Check::Ready {
                pane, coordinator, ..
            } => {
                let items = [Outgoing {
                    id: message_id.clone(),
                    envelope: envelope.clone(),
                    pointer: pointer.clone(),
                }];
                match self.type_message(
                    request_id.to_string(),
                    &target.terminal_id,
                    &pane,
                    &items,
                    coordinator,
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
        let entry = self.queued_message(
            target,
            message,
            envelope,
            Some(pointer),
            false,
            from,
            now_unix,
        );
        self.message_queue.push(entry);
        Ok(MessageDelivery::Queued { reason })
    }

    /// `agents.queued`: the caller's own messages still queued, oldest
    /// first, with their age and why each waits now.
    pub(crate) fn handle_agents_queued(
        &mut self,
        id: String,
        params: crate::api::schema::agents_model::AgentsQueuedParams,
    ) -> String {
        let caller = match self.required_caller(&params.caller_pane) {
            Ok(caller) => caller,
            Err(error) => return Self::model_reply(id, Err(error)),
        };
        let now_unix = crate::coordinator::now_unix();
        let mine: Vec<QueuedMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| {
                entry.from_terminal.as_ref() == Some(&caller.terminal_id)
                    || (entry.from_terminal.is_none()
                        && entry.message.from_pane.as_deref() == Some(caller.public.as_str()))
            })
            .cloned()
            .collect();
        let messages = mine
            .into_iter()
            .map(|entry| {
                let reason = match self.message_target(&entry.terminal_id, entry.session.as_deref())
                {
                    _ if self
                        .message_queue
                        .entries
                        .iter()
                        .find(|other| other.terminal_id == entry.terminal_id)
                        .is_some_and(|first| first.id() != entry.id()) =>
                    {
                        "earlier messages are waiting for it".to_string()
                    }
                    Some(target) => match self.message_check(&target) {
                        Check::Ready {
                            hook_ended: true, ..
                        } => "its turn just ended".to_string(),
                        check => MessageCheck(check).status(),
                    },
                    None => "offline".to_string(),
                };
                crate::api::schema::agents_model::AgentsQueuedMessage {
                    id: entry.id().to_string(),
                    to_pane: entry.message.to_pane.clone(),
                    to_name: entry.message.to_name.clone().unwrap_or_default(),
                    queued_unix: entry.message.unix,
                    age_s: now_unix.saturating_sub(entry.message.unix),
                    reason,
                    notified: entry.stuck_notified,
                }
            })
            .collect();
        encode_success(id, ResponseResult::AgentsQueued { messages })
    }

    /// An urgent message (`agents.send_message urgent`, the policy and the
    /// rate limit already passed): typed in now when the target is free;
    /// otherwise queued ahead of the target's other messages, and a working
    /// Claude or Codex turn is interrupted with one Esc first (never a
    /// blocked, suspended, starting or typing target, never the
    /// coordinator, never twice within [`INTERRUPT_WAIT`]). The queue types
    /// it in once the turn ended (its Stop hook or the screen reading idle)
    /// and SETTLE passed. Returns the delivery and whether it interrupted.
    #[allow(clippy::too_many_arguments)] // One message's parts, as deliver_agent_message.
    pub(crate) fn deliver_urgent_message(
        &mut self,
        request_id: &str,
        target: &super::terminal_targets::TerminalTarget,
        check: MessageCheck,
        message: AgentMessage,
        envelope: String,
        pointer: PointerFrom,
        from: Option<crate::terminal::TerminalId>,
        now_unix: u64,
    ) -> Result<(MessageDelivery, bool), MessageRefused> {
        let free = matches!(
            check.0,
            Check::Ready {
                hook_ended: false,
                ..
            }
        ) && !self.message_queue.queued_for(&target.terminal_id);
        if free || check.0 == Check::Missing {
            return self
                .deliver_agent_message(
                    request_id, target, check, message, envelope, pointer, from, now_unix,
                )
                .map(|delivery| (delivery, false));
        }
        if self.message_queue.entries.len() >= MAX_QUEUED {
            return Err(MessageRefused::new(
                "queue_full",
                "too many queued agent messages",
            ));
        }
        let message_id = message.id.clone().unwrap_or_default();
        let blocked = self.urgent_interrupt_block(target);
        let interrupted = match blocked {
            None => self.interrupt_turn(target, &message_id, from.as_ref()),
            Some(why) => {
                tracing::info!(
                    message = %message_id,
                    why,
                    "message queue: an urgent message queued without an interrupt"
                );
                false
            }
        };
        let reason = if interrupted {
            "interrupted its turn; typed in once the turn has ended".to_string()
        } else {
            match (&check.0, blocked) {
                (Check::Wait(reason), _) => format!("{reason}; not interrupted"),
                (_, Some(why)) => format!("{why}; not interrupted"),
                _ => "queued ahead of its other messages".to_string(),
            }
        };
        let entry = self.queued_message(
            target,
            message,
            envelope,
            Some(pointer),
            false,
            from,
            now_unix,
        );
        self.message_queue.push_at(entry, true);
        Ok((MessageDelivery::Queued { reason }, interrupted))
    }

    /// Why an urgent message may not interrupt `target`'s turn now; `None`
    /// when one Esc may go in. Read in the same `&mut App` borrow as the
    /// write, so no client input lands in between.
    fn urgent_interrupt_block(
        &self,
        target: &super::terminal_targets::TerminalTarget,
    ) -> Option<&'static str> {
        let Some(terminal) = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
        else {
            return Some("gone");
        };
        if terminal.suspended_agent.is_some() {
            return Some("suspended");
        }
        if terminal.managed_agent_launch_pending()
            || (terminal.managed_agent_kind().is_some()
                && !terminal.managed_agent_interactive_ready())
        {
            return Some("starting");
        }
        match terminal.state {
            crate::detect::AgentState::Working => {}
            crate::detect::AgentState::Blocked => return Some("blocked on its user"),
            _ => return Some("not working"),
        }
        if terminal.turn().hook_turn_ended().is_some() {
            return Some("its turn already ended");
        }
        let Some(agent) = terminal.effective_known_agent() else {
            return Some("no agent");
        };
        // Esc ends a turn in Claude Code and Codex; other agents get no
        // interrupt (an Esc may mean something else there).
        if !matches!(
            agent,
            crate::detect::Agent::Claude | crate::detect::Agent::Codex
        ) {
            return Some("no interrupt for this agent");
        }
        if terminal.agent_voice().active() {
            return Some("voice mode");
        }
        if self.is_coordinator_pane(target.ws_idx, target.pane_id) {
            return Some("the coordinator is never interrupted");
        }
        if self
            .message_queue
            .interrupted_at
            .get(&target.terminal_id)
            .is_some_and(|at| Instant::now() < *at + INTERRUPT_WAIT)
        {
            return Some("already interrupted");
        }
        let Some(runtime) = self.lookup_runtime_sender(target.ws_idx, target.pane_id) else {
            return Some("no runtime");
        };
        if super::typing_guard::runtime_typing_block(Some(agent), runtime, Instant::now()).is_some()
        {
            return Some("its user is typing");
        }
        None
    }

    /// Send one Esc into `target` (an urgent message's interrupt), recorded
    /// as the sender's programmatic write for the turn origin.
    fn interrupt_turn(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        message_id: &str,
        from: Option<&crate::terminal::TerminalId>,
    ) -> bool {
        let Some(runtime) = self.lookup_runtime_sender(target.ws_idx, target.pane_id) else {
            return false;
        };
        let bytes: Vec<u8> = match super::api_helpers::encode_api_keys(runtime, &["esc".into()]) {
            Ok(encoded) => encoded.into_iter().flatten().collect(),
            Err(_) => return false,
        };
        if let Err(err) = runtime.try_send_bytes(bytes::Bytes::from(bytes)) {
            tracing::warn!(error = %err, "message queue: the urgent interrupt failed");
            return false;
        }
        self.note_pane_input(
            target.ws_idx,
            target.pane_id,
            crate::agents_model::InputSource::Programmatic(match from {
                Some(from) => crate::agents_model::Programmatic::AgentMessage {
                    id: message_id.to_string(),
                    from: from.clone(),
                },
                None => crate::agents_model::Programmatic::Api,
            }),
        );
        self.message_queue
            .interrupted_at
            .insert(target.terminal_id.clone(), Instant::now());
        tracing::info!(
            message = %message_id,
            target = %target.terminal_id,
            "message queue: an urgent message interrupted the target's turn"
        );
        true
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
        if self.message_queue.queued_for(&target.terminal_id) {
            return LegacyPrompt::Answered(self.queue_legacy_message(request_id, params));
        }
        let Check::Ready {
            pane,
            coordinator,
            hook_ended: false,
        } = self.message_check(&target)
        else {
            return LegacyPrompt::Answered(self.queue_legacy_message(request_id, params));
        };
        // A target that reads messages by id gets the pointer line; the
        // coordinator keeps the paste (an older sender writes its turn
        // marker itself).
        let message_id = envelope_id(&params.text);
        let (Some(message_id), false) = (message_id, coordinator) else {
            return LegacyPrompt::TypeNow(params);
        };
        if !self.reads_messages(&target.terminal_id) {
            return LegacyPrompt::TypeNow(params);
        }
        let items = [Outgoing {
            pointer: PointerFrom::from_envelope(&message_id, &params.text),
            id: message_id,
            envelope: params.text.clone(),
        }];
        let typed = self.type_message(
            request_id.to_string(),
            &target.terminal_id,
            &pane,
            &items,
            false,
            None,
        );
        LegacyPrompt::Answered(match typed {
            Ok(()) => match self.agent_info(target.ws_idx, target.pane_id) {
                Some(agent) => encode_success(
                    request_id.to_string(),
                    ResponseResult::AgentPrompted { agent },
                ),
                None => encode_error(
                    request_id.to_string(),
                    "agent_not_found",
                    format!("agent {} not found", params.target),
                ),
            },
            Err(response) if retryable(&error_code(&response)) => {
                self.queue_legacy_message(request_id, params)
            }
            Err(response) => response,
        })
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
        let entry = self.queued_message(&target, message, params.text, None, true, None, now_unix);
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
        self.message_queue
            .screens
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
            Check::Wait(reason) => {
                self.message_queue.missing_since.remove(&terminal_id);
                self.message_queue.ready_since.remove(&terminal_id);
                if let Some(at) = self.message_queue.interrupted_at.get(&terminal_id).copied() {
                    if now >= at + INTERRUPT_WAIT {
                        tracing::info!(
                            target = %terminal_id,
                            reason = %reason,
                            "message queue: an interrupted turn did not end in time; the urgent message stays queued"
                        );
                        self.message_queue.interrupted_at.remove(&terminal_id);
                    } else {
                        next.push(at + INTERRUPT_WAIT);
                    }
                }
                if reason == "working" {
                    return self.watch_stuck_target(&target, &reason, now, now_unix, next);
                }
                self.message_queue.screens.remove(&terminal_id);
                return false;
            }
            Check::Ready {
                pane, coordinator, ..
            } => (pane, coordinator),
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
        // One delivery: the oldest messages for this target, each with its
        // envelope (one paste, or one pointer line listing their ids).
        // The coordinator gets one message per turn: it may answer on its own
        // only the message that started its turn (the turn origin and marker
        // name one id), so a batch would leave messages 2..N unanswerable.
        let max_combined = if coordinator { 1 } else { MAX_COMBINED };
        let mut items: Vec<Outgoing> = Vec::new();
        let mut chars = 0;
        for entry in self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == terminal_id)
        {
            if !items.is_empty()
                && (items.len() >= max_combined
                    || chars + entry.envelope.len() > MAX_COMBINED_CHARS)
            {
                break;
            }
            chars += entry.envelope.len() + 2;
            items.push(entry.outgoing());
        }
        let ids: Vec<String> = items.iter().map(|item| item.id.clone()).collect();
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
            &terminal_id,
            &pane,
            &items,
            coordinator,
            from.as_ref(),
        ) {
            Ok(()) => {
                self.message_queue.ready_since.remove(&terminal_id);
                self.message_queue.interrupted_at.remove(&terminal_id);
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

    /// A target waits on `working`: sample the screen above its prompt box
    /// every [`STUCK_SAMPLE`], and once it has not changed for
    /// [`STUCK_SCREEN`] while a message waited [`STUCK_WAIT`], tell that
    /// message's sender (once per message, once per target per
    /// [`STUCK_NOTICE_EVERY`]). A live turn's spinner and streamed text
    /// change the region; a panel ticking under the prompt box does not.
    /// Returns whether a notice went out.
    fn watch_stuck_target(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        reason: &str,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let terminal_id = target.terminal_id.clone();
        let sample_due = self
            .message_queue
            .screens
            .get(&terminal_id)
            .is_none_or(|watch| now >= watch.sampled + STUCK_SAMPLE);
        if sample_due {
            let Some(hash) = self
                .lookup_runtime_sender(target.ws_idx, target.pane_id)
                .map(|runtime| screen_hash(&runtime.detection_text()))
            else {
                return false;
            };
            let watch = self
                .message_queue
                .screens
                .entry(terminal_id.clone())
                .or_insert(ScreenWatch {
                    hash,
                    since: now,
                    sampled: now,
                });
            if watch.hash != hash {
                watch.hash = hash;
                watch.since = now;
            }
            watch.sampled = now;
        }
        let Some(watch) = self.message_queue.screens.get(&terminal_id).copied() else {
            return false;
        };
        next.push(watch.sampled + STUCK_SAMPLE);
        let unchanged = now.saturating_duration_since(watch.since);
        let cooled = self
            .message_queue
            .stuck_notice_at
            .get(&terminal_id)
            .is_none_or(|at| now >= *at + STUCK_NOTICE_EVERY);
        if unchanged < STUCK_SCREEN || !cooled {
            return false;
        }
        let Some(entry) = self
            .message_queue
            .entries
            .iter()
            .find(|entry| {
                entry.terminal_id == terminal_id
                    && !entry.stuck_notified
                    && !entry.legacy
                    && now_unix.saturating_sub(entry.message.unix) >= STUCK_WAIT.as_secs()
            })
            .cloned()
        else {
            return false;
        };
        let status = self
            .message_status_name(target)
            .unwrap_or_else(|| reason.to_string());
        self.send_stuck_notice(&entry, reason, &status, unchanged, now_unix);
        if let Some(queued) = self
            .message_queue
            .entries
            .iter_mut()
            .find(|queued| queued.id() == entry.id())
        {
            queued.stuck_notified = true;
        }
        self.message_queue.stuck_notice_at.insert(terminal_id, now);
        self.message_queue.save();
        true
    }

    /// The target's displayed status word (`working`, `blocked`, ...).
    fn message_status_name(
        &self,
        target: &super::terminal_targets::TerminalTarget,
    ) -> Option<String> {
        let pane = self
            .state
            .workspaces
            .get(target.ws_idx)?
            .pane_state(target.pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let status = crate::workspace::agent_status(
            terminal.state,
            pane.seen,
            terminal.suspended_agent.is_some(),
        );
        serde_json::to_value(status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
    }

    /// Tell `entry`'s sender, through the same delivery path, that its
    /// message seems stuck. Logged (message log and tracing); a sender that
    /// is gone gets nothing.
    fn send_stuck_notice(
        &mut self,
        entry: &QueuedMessage,
        reason: &str,
        status: &str,
        unchanged: Duration,
        now_unix: u64,
    ) {
        let message_id = entry.id().to_string();
        let target_name = entry
            .message
            .to_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| entry.message.to_pane.clone());
        let target = format!("{target_name} ({})", entry.message.to_pane);
        let waited = Duration::from_secs(now_unix.saturating_sub(entry.message.unix));
        let text = format!(
            "herdr: your message {message_id} to {target} has waited {}; {target_name} reads {status} but its screen has not changed for {} (queued: {reason}). It stays queued; check it with agents_messages.",
            minutes(waited),
            minutes(unchanged),
        );
        tracing::info!(
            message = %message_id,
            target = %entry.message.to_pane,
            waited_s = waited.as_secs(),
            unchanged_s = unchanged.as_secs(),
            "message queue: a queued message seems stuck; telling its sender"
        );
        let sender = entry
            .from_terminal
            .as_ref()
            .and_then(|terminal| self.public_pane_of_terminal(terminal))
            .or_else(|| entry.message.from_pane.clone());
        let Some(sender_target) = sender
            .as_deref()
            .and_then(|pane| self.resolve_agent_target(pane).ok())
        else {
            return;
        };
        let sender_pane = self
            .public_pane_id(sender_target.ws_idx, sender_target.pane_id)
            .unwrap_or_default();
        let id = messages::new_id();
        let envelope = format!(
            "[herdr+ notice {id} from herdr {} \u{2014} about your message {message_id}, not your user]\n{text}\n[no reply needed]",
            crate::agents_model::envelope::clock(now_unix),
        );
        let pointer = PointerFrom {
            id: id.clone(),
            reply_to: None,
            name: "herdr".into(),
            pane: String::new(),
            relation: crate::agents_model::envelope::PointerRelation::Herdr,
            urgent: false,
        };
        let line = AgentMessage {
            unix: now_unix,
            from_pane: None,
            from_name: Some("herdr".into()),
            to_pane: sender_pane,
            to_name: entry.message.from_name.clone(),
            text,
            outcome: OUTCOME_QUEUED.into(),
            id: Some(id.clone()),
            reply_to: None,
            from_role: None,
            kind: None,
            team: None,
        };
        let check = self.agent_message_check(&sender_target);
        match self.deliver_agent_message(
            &format!("stuck-notice:{id}"),
            &sender_target,
            check,
            line.clone(),
            envelope,
            pointer,
            None,
            now_unix,
        ) {
            Ok(MessageDelivery::Sent) => {
                let mut sent = line;
                sent.outcome = messages::OUTCOME_SENT.into();
                self.message_queue.log(sent);
            }
            Ok(MessageDelivery::Queued { .. }) => {}
            Err(refused) => tracing::warn!(
                code = %refused.code,
                "message queue: the stuck notice could not reach its sender"
            ),
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
