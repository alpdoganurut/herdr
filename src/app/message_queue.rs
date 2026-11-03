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
//! run) counts as free too, always through the queue so SETTLE applies, and
//! only once the screen above its prompt box stood still for SETTLE. Per
//! target FIFO; when several wait for one target they go in together, each
//! with its own envelope and id. A target whose herdr_agents server reads
//! messages by id gets one typed pointer line for them instead of the paste
//! (src/app/message_pointer.rs). A message is never typed into a blocked
//! target (a permission dialog) or a suspended one: it waits (an agent
//! herdr suspended for idling is woken instead, src/app/agent_autosuspend.rs:
//! the message goes in once it is up; a wake that keeps failing tells the
//! senders once and the message waits). Blocked means
//! the screen reads blocked, or the agent's own OSC 7501 status report says
//! blocked (a veto only: an idle report never frees a blocked screen), or
//! its input box is not on its screen (the typing guard fails closed,
//! src/app/typing_guard.rs). It expires after [`MESSAGE_TTL_S`], or while
//! its target waits on its user up to [`MESSAGE_TTL_USER_BLOCKED_S`] after it
//! was sent; every expiry tells its sender. It is dropped when its target
//! pane or agent is gone for [`GONE_GRACE`] (long enough for a restart to
//! detect the agent again). A target that waits on its user for
//! [`STUCK_WAIT`] with messages held tells their senders (again after
//! [`USER_BLOCKED_NOTICE_BACKOFF`]) and its user (one agent card).
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
//! Into Claude Code and Codex a message goes in as two writes, its text and
//! then its Enter once the text shows in the input box; every delivered
//! message waits for its target to take it in (`confirmed`, else
//! `unconfirmed`): src/app/message_queue/submit.rs.
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
/// While its target waits on its user (a dialog, a question, a permission),
/// a queued message is kept alive up to this long after it was sent: each
/// queue pass that sees the wait moves its expiry to [`MESSAGE_TTL_S`] from
/// now, never past this cap.
pub(crate) const MESSAGE_TTL_USER_BLOCKED_S: u64 = 24 * 3600;
/// After the first notice about a target that waits on its user, the next
/// notices go out this long after the previous one (the last value repeats).
pub(crate) const USER_BLOCKED_NOTICE_BACKOFF: [Duration; 2] =
    [Duration::from_secs(3600), Duration::from_secs(4 * 3600)];
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
/// An urgent message never interrupts a turn that herdr itself started by
/// typing into the pane this recently (a delivered message, a wake-up): the
/// turn is about to read that message.
pub(crate) const INTERRUPT_AFTER_TYPING: Duration = Duration::from_secs(30);
/// The wait reason while a target's hooks say its turn is live but its
/// screen reads idle.
const HOOK_LIVE_REASON: &str = "its turn is live by its hooks";
/// The wait reason while a target's screen reads blocked.
const SCREEN_BLOCKED_REASON: &str = "blocked on its user (a question or an approval)";
/// The wait reason while a target's own status report (OSC 7501) reads
/// blocked.
const PROGRAM_STATUS_BLOCKED_REASON: &str = "blocked on its user (its own status report)";
/// Fork (lost-message fix): the wait reason, and a direct send's queued
/// reason, while a message's text is typed in and its Enter waits for the
/// input box to show it (src/app/message_queue/submit.rs).
pub(crate) const SUBMITTING_REASON: &str = "submitting";
/// The wait reason while a target is suspended.
const SUSPENDED_REASON: &str = "suspended";
/// Fork (autosuspend): the sender's reason when its message woke an agent
/// herdr had suspended for idling (src/app/agent_autosuspend.rs).
pub(crate) const WAKING_REASON: &str =
    "herdr had suspended it while idle; waking it, the message goes in once it is ready";
/// Fork (autosuspend): the sender's reason while the agent herdr suspended
/// for idling is still exiting (its wake is retried).
pub(crate) const WAKE_PENDING_REASON: &str =
    "herdr had suspended it while idle; waking it once its process has exited, the message goes in once it is ready";
/// Fork (autosuspend): the sender's reason while the pane of the agent
/// herdr suspended for idling is not at a shell prompt (its wake is retried).
pub(crate) const WAKE_NO_SHELL_REASON: &str =
    "herdr had suspended it while idle; its pane is not at a shell prompt, so it is relaunched once it is; it stays queued and herdr tries again";
/// Fork (autosuspend): why a wake waits while the user types in the pane.
pub(crate) const USER_TYPING_WAKE_REASON: &str = "its user is typing in it";
/// Fork (autosuspend): how often the queue retries a wake that failed.
pub(crate) const WAKE_RETRY: Duration = Duration::from_secs(10);
/// Fork (autosuspend): after a wake failed this long, the senders of the
/// messages waiting for it hear once.
pub(crate) const WAKE_NOTICE_AFTER: Duration = Duration::from_secs(60);
/// Fork (restart screen): how long one retried wake shows the pane waking;
/// the queue pass retries every [`WAKE_RETRY`], so it lapses only once the
/// retries stopped.
pub(crate) const WAKE_MARK_TTL: Duration = Duration::from_secs(30);

mod submit;
pub(crate) use submit::DeliveryWatch;
/// Tests elsewhere: how long the input box may take to show typed text.
#[cfg(test)]
pub(crate) fn submit_window_for_test() -> Duration {
    submit::SUBMIT_WINDOW
}

/// Whether a wait reason means the target waits on its user: its screen or
/// its own status report reads blocked, or its input box is not on screen.
fn is_user_blocked(reason: &str) -> bool {
    reason == SCREEN_BLOCKED_REASON
        || reason == PROGRAM_STATUS_BLOCKED_REASON
        || reason == super::typing_guard::TypingBlock::InputBoxAbsent.reason()
}
// At most this many queued messages go into one paste (shared with
// agents_wait_for_message all=true).
use crate::api::schema::agent_messages::{MAX_COMBINED, MAX_COMBINED_CHARS};
/// The whole queue's cap; past it `agent.message_send` answers `queue_full`.
pub(crate) const MAX_QUEUED: usize = 256;
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
    /// Fork: a notice from herdr about its target's own pane
    /// ([`App::queue_herdr_notice`]): it waits for that agent past the
    /// gone grace while the pane still records the same session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) herdr_notice: bool,
    /// Fork (lost-message fix): typing attempts whose text never showed in
    /// the target's empty input box (no Enter went in); held at
    /// [`submit::MAX_SUBMIT_ATTEMPTS`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) attempts: u32,
    /// Fork (lost-message fix): when its text was typed in and its Enter
    /// had not gone in yet. Saved, so a restart or a live handoff inside
    /// that window looks at the input box before typing it again
    /// ([`submit`]); cleared when it is typed again (it leaves the queue
    /// once delivered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) typed_unix: Option<u64>,
    /// Fork: sent by the coordinator (`agents.send_message` as
    /// `Actor::Coordinator`, server-resolved, never from a caller's word): a
    /// do-not-disturb an agent turned on lets it through (src/app/tab_dnd.rs).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) from_coordinator: bool,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
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
    /// Fork: recreated-directory notices not queued yet
    /// (src/app/missing_cwd.rs; older builds ignore the field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    missing_cwd: Vec<super::missing_cwd::SavedMissingCwdNotice>,
}

/// A target that waits on its user while messages are queued for it
/// (runtime only).
#[derive(Debug, Clone)]
pub(crate) struct UserBlocked {
    /// Since when the queue has seen the wait.
    since: Instant,
    /// The same moment as a Unix time (`AgentInfo.blocked_since`).
    pub(crate) since_unix: u64,
    /// The latest wait reason.
    pub(crate) reason: String,
    /// Notices sent to senders about this wait.
    notices: usize,
    /// Its user was shown the agent card about this wait.
    carded: bool,
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
    /// Targets settling on their Stop hook alone (`Check::Ready.hook_ended`):
    /// the hash of the screen above their prompt box at the last pass
    /// (runtime only; [`App::settle_on_a_still_screen`]).
    settle_screens: HashMap<String, u64>,
    /// When each target's last stuck notice went out.
    stuck_notice_at: HashMap<String, Instant>,
    /// Targets whose last check was a wait on their user (runtime only).
    pub(crate) user_blocked: HashMap<String, UserBlocked>,
    /// Targets an urgent message interrupted (Esc), and when.
    interrupted_at: HashMap<String, Instant>,
    /// Fork (lost-message fix): batches waiting for their Enter, late
    /// renders and confirmations (runtime only; submit.rs).
    pub(crate) delivery: DeliveryWatch,
    /// Fork (autosuspend): auto-suspended targets whose wake failed (runtime
    /// only): since when, why, and whether their senders were told.
    pub(crate) wake_failed: HashMap<String, WakeFailure>,
    /// Agent panes whose missing directory herdr recreated, waiting to be
    /// told so once their agent is up (src/app/missing_cwd.rs; saved in the
    /// queue file by public pane id).
    pub(crate) missing_cwd: super::missing_cwd::MissingCwdNotices,
    /// `message_queue.json`; `None` = not persisted (tests, unpersisted sessions).
    pub(super) store: Option<PathBuf>,
    /// The coordinator directory (the message log); `None` = no log lines.
    log_dir: Option<PathBuf>,
    io: Option<mpsc::Sender<IoJob>>,
    /// Log lines written (tests).
    #[cfg(test)]
    pub(crate) written: Vec<AgentMessage>,
    /// What the urgent interrupt's fresh screen check answers (tests).
    #[cfg(test)]
    pub(crate) screen_blocker_for_test: Option<bool>,
}

/// Why a target cannot take a message now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Check {
    /// Deliverable: its public pane id, and whether it is the coordinator.
    /// `hook_ended`: its screen reads working but its own hooks reported
    /// its turn's end (src/app/hook_turn.rs); typed in only after
    /// [`SETTLE`] with the screen above its prompt box still, through the
    /// queue.
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
        self.missing_cwd.load_saved_notices(file.missing_cwd);
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

    /// Tests: save to `path`, write no log lines.
    #[cfg(test)]
    pub(crate) fn persist_for_test(&mut self, path: PathBuf) {
        self.store = Some(path);
        self.log_dir = None;
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
            missing_cwd: self.missing_cwd.saved(),
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

    /// Fork: a quiet message released back to the queue (its tab left
    /// do-not-disturb, src/app/quiet_messages.rs): logged as an update line
    /// `queued` (its full line is in the log already), queued last.
    pub(super) fn requeue(&mut self, entry: QueuedMessage) {
        self.log(AgentMessage::update(
            &entry.message,
            OUTCOME_QUEUED,
            crate::coordinator::now_unix(),
        ));
        self.entries.push(entry);
        self.due = true;
        self.save();
    }

    /// Queue `entry`; `front`: ahead of every plain message queued for the
    /// same target, behind the urgent ones queued before it (an urgent
    /// message; urgent ones keep their sending order).
    fn push_at(&mut self, entry: QueuedMessage, front: bool) {
        let mut line = entry.message.clone();
        line.outcome = OUTCOME_QUEUED.into();
        if entry.legacy {
            self.log(AgentMessage::update(&line, OUTCOME_QUEUED, line.unix));
        } else {
            self.log(line);
        }
        let same_target = |queued: &QueuedMessage| queued.terminal_id == entry.terminal_id;
        let at = front
            .then(|| {
                self.entries
                    .iter()
                    .rposition(|queued| same_target(queued) && is_urgent(queued))
                    .map(|last_urgent| last_urgent + 1)
                    .or_else(|| self.entries.iter().position(same_target))
            })
            .flatten()
            .unwrap_or(self.entries.len());
        self.entries.insert(at, entry);
        self.due = true;
        self.save();
    }

    pub(super) fn finish(&mut self, ids: &[String], outcome: &str, now_unix: u64) {
        let mut done = Vec::new();
        let mut targets: Vec<String> = Vec::new();
        self.entries.retain(|entry| {
            if ids.iter().any(|id| id == entry.id()) {
                done.push(entry.message.clone());
                if !targets.contains(&entry.terminal_id) {
                    targets.push(entry.terminal_id.clone());
                }
                false
            } else {
                true
            }
        });
        // A target with nothing queued any more (a claim takes the last one
        // outside a pass) has no user-blocked wait to report.
        for terminal_id in &targets {
            if !self.queued_for(terminal_id) {
                self.user_blocked.remove(terminal_id);
            }
        }
        // An interrupt is over once no urgent message waits on it (typed in,
        // claimed, expired or dropped): the target's hook hold applies again.
        for terminal_id in targets {
            if !self
                .entries
                .iter()
                .any(|entry| entry.terminal_id == terminal_id && is_urgent(entry))
            {
                self.interrupted_at.remove(&terminal_id);
            }
        }
        for message in done {
            self.log(AgentMessage::update(&message, outcome, now_unix));
        }
        self.save();
    }

    pub(crate) fn queued_for(&self, terminal_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.terminal_id == terminal_id)
    }

    /// When the server loop should run the next pass.
    pub(crate) fn next_deadline(&self, now: Instant, now_unix: u64) -> Option<Instant> {
        let missing_cwd = self.missing_cwd.next_deadline();
        let delivery = self.delivery.next_deadline();
        if self.entries.is_empty() {
            return missing_cwd.into_iter().chain(delivery).min();
        }
        if let Some(at) = missing_cwd.filter(|at| *at <= now) {
            return Some(at);
        }
        if self.due {
            return Some(now);
        }
        let expiry = self
            .entries
            .iter()
            .map(|entry| now + Duration::from_secs(entry.expires_unix.saturating_sub(now_unix)))
            .min();
        [self.retry_at, expiry, missing_cwd, delivery]
            .into_iter()
            .flatten()
            .min()
    }
}

/// Whether a queued message is urgent (its pointer says so).
fn is_urgent(entry: &QueuedMessage) -> bool {
    entry.pointer.as_ref().is_some_and(|pointer| pointer.urgent)
}

/// Whether a fresh classification of a target's screen forbids an Esc: it
/// shows a dialog or a question for its user. A transcript viewer
/// (`skip_state_update`) says nothing about the prompt; its blocker flag
/// still counts.
fn detection_blocks_interrupt(detection: &crate::detect::AgentDetection) -> bool {
    detection.visible_blocker
        || (!detection.skip_state_update && detection.state == crate::detect::AgentState::Blocked)
}

/// A short, stable hash of the detection text above the prompt box.
fn screen_hash(detection_text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    crate::detect::manifest::above_prompt_box_text(detection_text).hash(&mut hasher);
    hasher.finish()
}

/// The target as a notice names it: its agent name, else its pane.
fn notice_target_name(entry: &QueuedMessage) -> String {
    entry
        .message
        .to_name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| entry.message.to_pane.clone())
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

/// The wait reason for a guarded prompt refused because its target waits on
/// its user (`input_box_absent`, `agent_blocked`); `None` for other codes.
fn user_blocked_reason(code: &str) -> Option<&'static str> {
    match code {
        "input_box_absent" => Some(super::typing_guard::TypingBlock::InputBoxAbsent.reason()),
        "agent_blocked" => Some(PROGRAM_STATUS_BLOCKED_REASON),
        _ => None,
    }
}

fn error_code(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| value["error"]["code"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Errors from a typing attempt that mean "not now" rather than "never".
fn retryable(code: &str) -> bool {
    matches!(
        code,
        "user_typing" | "input_box_absent" | "agent_blocked" | "agent_not_ready"
    )
}

/// Fork (autosuspend): a wake of an auto-suspended target that failed.
#[derive(Debug, Clone)]
pub(crate) struct WakeFailure {
    pub(crate) since: Instant,
    /// The last attempt; the next waits [`WAKE_RETRY`].
    pub(crate) last_attempt: Instant,
    pub(crate) reason: String,
    pub(crate) notified: bool,
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
    pub(super) fn message_target(
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
    pub(super) fn message_check(&self, target: &super::terminal_targets::TerminalTarget) -> Check {
        self.message_check_with(target, true)
    }

    /// Fork (agent handoff, src/app/agent_handoff.rs): whether herdr may
    /// type into the agent of `terminal_id` now by the queue's own rules:
    /// `Ok(pane)` when it could take a message, is not the coordinator, its
    /// screen reads idle (not working with only its Stop hook's word) and,
    /// before the run holds the queue, nothing is queued for it (those
    /// messages go first; once the prompt is typed they wait for the run
    /// instead, so they cannot keep it from `/compact`); `Err(None)` when
    /// the pane or its agent is gone, `Err(Some(reason))` otherwise.
    pub(super) fn agent_handoff_target(&self, terminal_id: &str) -> Result<String, Option<String>> {
        let target = self.message_target(terminal_id, None).ok_or(None)?;
        match self.message_check_with(&target, false) {
            Check::Missing => Err(None),
            Check::Wait(reason) => Err(Some(reason)),
            Check::Ready {
                coordinator: true, ..
            } => Err(Some("the coordinator".into())),
            Check::Ready {
                hook_ended: true, ..
            } => Err(Some("working".into())),
            Check::Ready { .. }
                if !self.agent_handoff.holds_messages(terminal_id)
                    && self.message_queue.queued_for(terminal_id) =>
            {
                Err(Some("messages are queued for it".into()))
            }
            Check::Ready { pane, .. } => Ok(pane),
        }
    }

    /// [`Self::message_check`]; `handoff_hold` makes a pane whose agent
    /// handoff is on wait (src/app/agent_handoff.rs).
    fn message_check_with(
        &self,
        target: &super::terminal_targets::TerminalTarget,
        handoff_hold: bool,
    ) -> Check {
        self.message_check_from(target, handoff_hold, false)
    }

    /// [`Self::message_check_with`] for a message from the coordinator
    /// (`from_coordinator`): a do-not-disturb an agent turned on does not
    /// hold it (src/app/tab_dnd.rs).
    fn message_check_from(
        &self,
        target: &super::terminal_targets::TerminalTarget,
        handoff_hold: bool,
        from_coordinator: bool,
    ) -> Check {
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
            return Check::Wait(SUSPENDED_REASON.into());
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
        // Fork (lost-message fix): a message's text is in, its Enter waits
        // for the input box to show it; nothing else goes in meanwhile.
        if self.message_queue.delivery.submitting(&target.terminal_id) {
            return Check::Wait(SUBMITTING_REASON.into());
        }
        // Fork (OSC 7501): the agent's own status report says it waits on
        // its user. A veto before every ready arm (an idle screen, a working
        // screen its hooks ended, the hook hold); never a release.
        if self
            .lookup_runtime_sender(target.ws_idx, target.pane_id)
            .is_some_and(crate::terminal::TerminalRuntime::program_status_blocked)
        {
            return Check::Wait(PROGRAM_STATUS_BLOCKED_REASON.into());
        }
        let hook_ended = terminal.state == crate::detect::AgentState::Working
            && terminal.turn().hook_turn_ended().is_some();
        match terminal.state {
            // Its own hooks say a turn is live (a start after its last
            // end): wait a bounded time for its Stop (src/agents_model/turn.rs).
            crate::detect::AgentState::Idle
                if self.hook_live_until(&target.terminal_id).is_some() =>
            {
                return Check::Wait(HOOK_LIVE_REASON.into());
            }
            crate::detect::AgentState::Idle => {}
            // Its own hooks say its last turn ended (src/app/hook_turn.rs):
            // the title spinner of background work is not a turn.
            crate::detect::AgentState::Working if hook_ended => {}
            crate::detect::AgentState::Working => return Check::Wait("working".into()),
            crate::detect::AgentState::Blocked => return Check::Wait(SCREEN_BLOCKED_REASON.into()),
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
        // Fork: a tab in do-not-disturb (src/app/tab_dnd.rs) gets nothing
        // typed in; the user's own handoff still goes, and the coordinator's
        // messages through a do-not-disturb an agent turned on.
        if handoff_hold && self.pane_dnd_holds(target.ws_idx, target.pane_id, from_coordinator) {
            return Check::Wait(super::tab_dnd::DND_WAIT_REASON.into());
        }
        // Fork (agent handoff): nothing lands between the handoff turn,
        // /compact and the resume line.
        if handoff_hold && self.agent_handoff.holds_messages(&target.terminal_id) {
            return Check::Wait("handoff and compaction in progress".into());
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

    /// Until when `terminal_id`'s hooks hold its idle screen as a live turn
    /// ([`crate::agents_model::turn::TurnState::hook_turn_live_until`]).
    /// Not within [`INTERRUPT_WAIT`] of an urgent Esc whose message is still
    /// queued: that turn ends without a Stop, and the urgent message goes in
    /// once the screen reads idle. A stale or finished interrupt no longer
    /// counts (a later turn's hooks hold its idle screen again).
    pub(crate) fn hook_live_until(&self, terminal_id: &str) -> Option<Instant> {
        let now = Instant::now();
        if self
            .message_queue
            .interrupted_at
            .get(terminal_id)
            .is_some_and(|at| now < *at + INTERRUPT_WAIT)
        {
            return None;
        }
        self.state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == terminal_id)?
            .turn()
            .hook_turn_live_until(now)
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
        let params = AgentPromptParams {
            target: pane.to_string(),
            text: input.text,
            wait: None,
            guard_user_typing: true,
        };
        // The coordinator's turn marker is written only when the prompt can
        // go in now (typing guard, input box, OSC 7501 blocked): a dialog
        // held for a while must not write and clear it every SETTLE.
        if coordinator {
            self.validate_agent_prompt(request_id.clone(), &params)?;
        }
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
            params,
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
        from_coordinator: bool,
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
            herdr_notice: false,
            attempts: 0,
            typed_unix: None,
            from_coordinator,
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
        // Fork: a tab in do-not-disturb stores it quietly
        // (src/app/quiet_messages.rs).
        if let Some(reply) = self.dnd_agent_message_send(
            &id,
            &target,
            &check,
            &message,
            &params.envelope,
            &params.id,
            now_unix,
        ) {
            return reply;
        }
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
            // Never from the caller's word (`from_pane`).
            false,
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

    /// Fork: [`Self::agent_message_check`] for a message from the
    /// coordinator (`agents.send_message` by `Actor::Coordinator`).
    pub(crate) fn coordinator_message_check(
        &self,
        target: &super::terminal_targets::TerminalTarget,
    ) -> MessageCheck {
        MessageCheck(self.message_check_from(target, true, true))
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
        from_coordinator: bool,
        now_unix: u64,
    ) -> Result<MessageDelivery, MessageRefused> {
        let message_id = message.id.clone().unwrap_or_default();
        // Fork (autosuspend): a full queue refuses before a wake, never
        // after it (the woken agent would get nothing).
        if self.message_queue.entries.len() >= MAX_QUEUED
            && self.auto_suspended(&target.terminal_id)
        {
            return Err(MessageRefused::new(
                "queue_full",
                "too many queued agent messages",
            ));
        }
        // Fork (autosuspend): a message wakes an agent herdr parked for
        // idling; it waits in the queue until the agent is up.
        let wake = self.wake_auto_suspended(target, Instant::now());
        let reason = match check.0 {
            Check::Missing => {
                return Err(MessageRefused::new(
                    "offline",
                    format!("{} is not running", message.to_pane),
                ))
            }
            _ if wake.is_some() => wake.clone().unwrap_or_default(),
            // Fork (lost-message fix): held messages do not block it.
            _ if self.message_queue.queued_for(&target.terminal_id)
                && !self.message_queue.submit_held(&target.terminal_id) =>
            {
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
                // Fork (lost-message fix): into Claude Code and Codex the
                // text goes in now and the queue presses Enter once it
                // shows; the sender hears `queued` (submitting).
                let submit = self.submit_agent(&target.terminal_id);
                if submit.is_some() && self.message_queue.entries.len() >= MAX_QUEUED {
                    return Err(MessageRefused::new(
                        "queue_full",
                        "too many queued agent messages",
                    ));
                }
                let typed = match submit {
                    Some(agent) => self.start_submit(
                        request_id.to_string(),
                        &target.terminal_id,
                        &pane,
                        &items,
                        coordinator,
                        from.as_ref(),
                        agent,
                        Instant::now(),
                    ),
                    None => self.type_message(
                        request_id.to_string(),
                        &target.terminal_id,
                        &pane,
                        &items,
                        coordinator,
                        from.as_ref(),
                    ),
                };
                match typed {
                    Ok(()) => {
                        let mut entry = self.queued_message(
                            target,
                            message,
                            envelope,
                            Some(pointer),
                            false,
                            from,
                            from_coordinator,
                            now_unix,
                        );
                        if submit.is_some() {
                            entry.typed_unix = Some(now_unix);
                            self.message_queue.push(entry);
                            return Ok(MessageDelivery::Queued {
                                reason: SUBMITTING_REASON.to_string(),
                            });
                        }
                        let paste = !self.reads_messages(&target.terminal_id);
                        self.watch_confirmation(
                            &target.terminal_id,
                            vec![entry],
                            paste,
                            Instant::now(),
                        );
                        return Ok(MessageDelivery::Sent);
                    }
                    Err(response) => {
                        let code = error_code(&response);
                        if !retryable(&code) && code != "busy" {
                            return Err(MessageRefused::from_response(&response));
                        }
                        match code.as_str() {
                            "user_typing" => "its user is typing in it".to_string(),
                            "busy" => "a coordinator turn is live".to_string(),
                            code => user_blocked_reason(code)
                                .map_or_else(|| "not ready".to_string(), str::to_string),
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
            from_coordinator,
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
                let first = self
                    .message_queue
                    .entries
                    .iter()
                    .find(|other| other.terminal_id == entry.terminal_id)
                    .filter(|first| first.id() != entry.id());
                let reason = match self.message_target(&entry.terminal_id, entry.session.as_deref())
                {
                    _ if first.is_some_and(|first| {
                        first.pointer.as_ref().is_some_and(|pointer| pointer.urgent)
                    }) =>
                    {
                        "an urgent message goes in ahead of it".to_string()
                    }
                    _ if first
                        .is_some_and(|first| first.attempts >= submit::MAX_SUBMIT_ATTEMPTS) =>
                    {
                        format!("an earlier message is {}", submit::SUBMIT_HELD_REASON)
                    }
                    _ if first.is_some() => "earlier messages are waiting for it".to_string(),
                    _ if entry.attempts >= submit::MAX_SUBMIT_ATTEMPTS => {
                        submit::SUBMIT_HELD_REASON.to_string()
                    }
                    Some(target) => {
                        // Its last typing attempt found no input box.
                        let blocked = self
                            .message_queue
                            .user_blocked
                            .get(&target.terminal_id)
                            .map(|blocked| blocked.reason.clone())
                            .or_else(|| {
                                self.message_queue
                                    .delivery
                                    .typing_wait
                                    .get(&target.terminal_id)
                                    .cloned()
                            });
                        match (self.message_check(&target), blocked) {
                            (
                                Check::Ready {
                                    hook_ended: true, ..
                                },
                                _,
                            ) => "its turn just ended".to_string(),
                            (Check::Ready { .. }, Some(reason)) => reason,
                            (check, _) => MessageCheck(check).status(),
                        }
                    }
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

    /// `agents.inbox`: the messages to the caller still queued (by the
    /// caller's terminal, as `agent.message_claim` matches them), oldest
    /// first. Read-only: nothing is claimed or typed in.
    pub(crate) fn handle_agents_inbox(
        &mut self,
        id: String,
        params: crate::api::schema::agents_model::AgentsInboxParams,
    ) -> String {
        let caller = match self.required_caller(&params.caller_pane) {
            Ok(caller) => caller,
            Err(error) => return Self::model_reply(id, Err(error)),
        };
        let now_unix = crate::coordinator::now_unix();
        let mut messages: Vec<crate::api::schema::agents_model::AgentsInboxMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == caller.terminal_id.as_str())
            .map(
                |entry| crate::api::schema::agents_model::AgentsInboxMessage {
                    id: entry.id().to_string(),
                    from_pane: entry.message.from_pane.clone(),
                    from_name: entry.message.from_name.clone(),
                    reply_to: entry.message.reply_to.clone(),
                    queued_unix: entry.message.unix,
                    age_s: now_unix.saturating_sub(entry.message.unix),
                    chars: entry.message.text.len() as u64,
                    legacy: entry.legacy,
                },
            )
            .collect();
        // Fork: and the quiet messages stored for it (src/app/quiet_messages.rs).
        messages.extend(self.quiet_inbox_messages(caller.terminal_id.as_str(), now_unix));
        // An urgent message sits at the front of its target's queue; the
        // inbox reads in sending order (stable for equal times).
        messages.sort_by_key(|message| message.queued_unix);
        encode_success(id, ResponseResult::AgentsInbox { messages })
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
        from_coordinator: bool,
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
                    request_id,
                    target,
                    check,
                    message,
                    envelope,
                    pointer,
                    from,
                    from_coordinator,
                    now_unix,
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
        // A target that is free (or whose hooks ended its turn) but has
        // messages queued gets no interrupt: its turn is not live.
        let ready = matches!(check.0, Check::Ready { .. });
        let blocked = if ready {
            None
        } else {
            self.urgent_interrupt_block(target)
        };
        let interrupted = match blocked {
            None if ready => false,
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
                (Check::Ready { .. }, _) => "queued ahead of its other messages".to_string(),
                (Check::Wait(reason), Some(why)) if reason == "working" => {
                    format!("{reason}; {why}; not interrupted")
                }
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
            from_coordinator,
            now_unix,
        );
        self.message_queue.push_at(entry, true);
        // Fork (autosuspend): an urgent message wakes an agent herdr parked
        // for idling like any other message (never an interrupt).
        let reason = self
            .wake_auto_suspended(target, Instant::now())
            .unwrap_or(reason);
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
        // Fork (OSC 7501): its own status report says it waits on its user.
        if self
            .lookup_runtime_sender(target.ws_idx, target.pane_id)
            .is_some_and(crate::terminal::TerminalRuntime::program_status_blocked)
        {
            return Some("blocked on its user");
        }
        if terminal.turn().hook_turn_ended().is_some() {
            return Some("its turn already ended");
        }
        // A turn herdr just started by typing in (a delivered message, a
        // wake-up, an earlier urgent) is about to read what it was given;
        // the urgent message goes in after that turn instead.
        if terminal
            .turn()
            .programmatic_within(INTERRUPT_AFTER_TYPING, Instant::now())
        {
            return Some("a herdr message just started its turn");
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
        match super::typing_guard::runtime_typing_block(Some(agent), runtime, Instant::now()) {
            Some(super::typing_guard::TypingBlock::InputBoxAbsent) => {
                return Some("its input box is not on screen")
            }
            Some(_) => return Some("its user is typing"),
            None => {}
        }
        // The status above is the detector's last report, which lags the
        // screen: read the screen once more, so an Esc never answers a
        // permission dialog or a question drawn since.
        if self.screen_shows_a_blocker(agent, runtime) {
            return Some("blocked on its user");
        }
        None
    }

    /// Whether `runtime`'s screen, classified now with the agent's
    /// detection rules, shows something waiting on its user. Once per
    /// urgent send, never per frame.
    fn screen_shows_a_blocker(
        &self,
        agent: crate::detect::Agent,
        runtime: &crate::terminal::TerminalRuntime,
    ) -> bool {
        #[cfg(test)]
        if let Some(blocked) = self.message_queue.screen_blocker_for_test {
            // Tests may not classify invented CLI screens with the bundled
            // rules (CLAUDE.md); they set the classifier's answer instead.
            return blocked;
        }
        let detection = crate::detect::detect_agent_with_osc(
            Some(agent),
            &runtime.detection_text(),
            &runtime.agent_osc_title(),
            &runtime.agent_osc_progress(),
        );
        detection_blocks_interrupt(&detection)
    }

    /// Send one Esc into `target` (an urgent message's interrupt), recorded
    /// as the sender's programmatic write for the turn origin.
    fn interrupt_turn(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        message_id: &str,
        from: Option<&crate::terminal::TerminalId>,
    ) -> bool {
        let origin = crate::agents_model::InputSource::Programmatic(match from {
            Some(from) => crate::agents_model::Programmatic::AgentMessage {
                id: message_id.to_string(),
                from: from.clone(),
            },
            None => crate::agents_model::Programmatic::Api,
        });
        if !self.send_interrupt_esc(target, origin) {
            return false;
        }
        tracing::info!(
            message = %message_id,
            target = %target.terminal_id,
            "message queue: an urgent message interrupted the target's turn"
        );
        true
    }

    /// One Esc into `target`, recorded as `origin`'s write and as an
    /// interrupt (no urgent Esc follows within [`INTERRUPT_WAIT`]). Shared by
    /// the urgent interrupt and a forced suspend (src/app/agent_suspend.rs).
    pub(super) fn send_interrupt_esc(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        origin: crate::agents_model::InputSource,
    ) -> bool {
        let Some(runtime) = self.lookup_runtime_sender(target.ws_idx, target.pane_id) else {
            return false;
        };
        let bytes: Vec<u8> = match super::api_helpers::encode_api_keys(runtime, &["esc".into()]) {
            Ok(encoded) => encoded.into_iter().flatten().collect(),
            Err(_) => return false,
        };
        if let Err(err) = runtime.try_send_bytes(bytes::Bytes::from(bytes)) {
            tracing::warn!(error = %err, "the interrupt (Esc) failed");
            return false;
        }
        self.note_pane_input(target.ws_idx, target.pane_id, origin);
        self.message_queue
            .interrupted_at
            .insert(target.terminal_id.clone(), Instant::now());
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
        // Fork (lost-message fix): a message whose text is in and whose
        // Enter waits is not taken (the waiter skips it; it goes in).
        let quiet_claimer = claimer.clone();
        let claimed = claimer.is_some_and(|terminal_id| {
            self.message_queue
                .entries
                .iter()
                .any(|entry| entry.id() == params.id && entry.terminal_id == terminal_id)
        }) && !self.message_queue.delivery.submitting_id(&params.id);
        if claimed {
            self.message_queue.finish(
                std::slice::from_ref(&params.id),
                OUTCOME_DELIVERED,
                crate::coordinator::now_unix(),
            );
        }
        // Fork: or a quiet message (src/app/quiet_messages.rs).
        let claimed = claimed
            || quiet_claimer
                .is_some_and(|terminal_id| self.claim_quiet_message(&terminal_id, &params.id));
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
        let entry = self.queued_message(
            &target,
            message,
            params.text,
            None,
            true,
            None,
            false,
            now_unix,
        );
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
        // Fork (missing directories): an agent coming up may take its notice.
        if matches!(
            event,
            EventKind::PaneAgentDetected | EventKind::PaneAgentStatusChanged
        ) {
            self.message_queue.missing_cwd.mark_due(Instant::now());
        }
        // Fork (missing directories): a moved pane's pending notice saves
        // its new public id (O(pending notices)).
        if matches!(event, EventKind::PaneMoved) {
            self.follow_missing_cwd_panes();
        }
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
        let queued = self.deliver_missing_cwd_notices(now, now_unix);
        self.message_queue_pass(now, now_unix) || queued
    }

    /// Fork (session chains): messages queued for Claude session `from`
    /// now wait for `to`, the newest session of its chain
    /// (src/agent_session_chain.rs), so a restart still finds their target.
    pub(crate) fn follow_queued_session(&mut self, from: &str, to: &str) {
        let mut moved = false;
        for entry in &mut self.message_queue.entries {
            if entry.session.as_deref() == Some(from) {
                entry.session = Some(to.to_string());
                moved = true;
            }
        }
        if moved {
            self.message_queue.save();
        }
    }

    /// Fork (session chains): after a restore, the queued messages of each
    /// pane whose saved session was advanced follow it (once).
    pub(crate) fn follow_restored_sessions(&mut self) {
        let moves: Vec<(String, String)> = self
            .state
            .terminals
            .values_mut()
            .filter_map(|terminal| {
                let from = terminal.session_advanced_from.take()?;
                Some((from, super::coordinator::terminal_session(terminal)?))
            })
            .collect();
        for (from, to) in moves {
            if from != to {
                self.follow_queued_session(&from, &to);
            }
        }
    }

    /// Queue a notice from herdr itself (no sender) for `target`: it goes in
    /// through the queue (SETTLE, the typing guard, the blocked and
    /// suspended vetoes) like any other message, never typed in directly.
    /// `about` names what it is about in the envelope. Returns its id.
    pub(super) fn queue_herdr_notice(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        text: String,
        about: &str,
        now_unix: u64,
    ) -> String {
        let id = messages::new_id();
        let envelope = format!(
            "[herdr+ notice {id} from herdr {} \u{2014} about {about}, not your user]\n{text}\n[no reply needed]",
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
        let to_name = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == target.terminal_id)
            .and_then(|terminal| terminal.agent_name.clone());
        let message = AgentMessage {
            unix: now_unix,
            from_pane: None,
            from_name: Some("herdr".into()),
            to_pane: self
                .public_pane_id(target.ws_idx, target.pane_id)
                .unwrap_or_default(),
            to_name,
            text,
            outcome: OUTCOME_QUEUED.into(),
            id: Some(id.clone()),
            reply_to: None,
            from_role: None,
            kind: None,
            team: None,
        };
        let mut entry = self.queued_message(
            target,
            message,
            envelope,
            Some(pointer),
            false,
            None,
            false,
            now_unix,
        );
        // It waits for the agent as long as a message to a user-blocked
        // agent may (a resume can take minutes to be detected).
        entry.expires_unix = now_unix.saturating_add(MESSAGE_TTL_USER_BLOCKED_S);
        entry.herdr_notice = true;
        self.message_queue.push(entry);
        id
    }

    /// Fork (autosuspend): wake the agent of `target` when herdr suspended it
    /// for idling (`agent.activate`); a message for it then waits until it
    /// is up. `None` when `target` is not auto-suspended, else the sender's
    /// reason. A failed wake is remembered (the queue pass retries it every
    /// [`WAKE_RETRY`]); the record keeps its mark.
    pub(crate) fn wake_auto_suspended(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        now: Instant,
    ) -> Option<String> {
        if !self.auto_suspended(&target.terminal_id) {
            // An earlier episode's failure (activated meanwhile) is over.
            self.message_queue.wake_failed.remove(&target.terminal_id);
            return None;
        }
        let pane = self.public_pane_id(target.ws_idx, target.pane_id)?;
        // The relaunch types into the pane's shell: like a restart, it waits
        // while the user types there (the typing guard).
        let typing = self
            .lookup_runtime_sender(target.ws_idx, target.pane_id)
            .is_some_and(|runtime| {
                super::typing_guard::runtime_typing_block(None, runtime, now).is_some()
            });
        let outcome = if typing {
            Err((false, false, USER_TYPING_WAKE_REASON.to_string()))
        } else {
            self.activate_agent(&pane).map_err(|err| {
                let exiting = matches!(
                    err,
                    super::agent_suspend::AgentActivateError::ExitPending(_)
                );
                let no_shell = matches!(
                    err,
                    super::agent_suspend::AgentActivateError::PaneNotAvailable(_)
                );
                (
                    exiting,
                    no_shell,
                    self.agent_activate_error_body(err).message,
                )
            })
        };
        match outcome {
            Ok(_) => {
                self.message_queue.wake_failed.remove(&target.terminal_id);
                self.message_queue.mark_due();
                tracing::info!(
                    event = "agent.autosuspend.wake",
                    pane = %pane,
                    "a message woke an agent herdr had suspended while idle"
                );
                Some(WAKING_REASON.to_string())
            }
            Err((exiting, no_shell, reason)) => {
                // Fork (restart screen): a wake still waiting on the exit,
                // the shell or the user's typing shows the pane waking (the
                // queue pass retries it); any other failure gave up.
                let retrying = typing || exiting || no_shell;
                if let Some(terminal) = self.state.terminals.get_mut(target.terminal_id.as_str()) {
                    let changed = if retrying {
                        terminal.mark_suspended_agent_waking(now, now + WAKE_MARK_TTL)
                    } else {
                        terminal.fail_suspended_agent_resume(
                            crate::terminal::AgentResumeKind::Wake,
                            now + super::agent_suspend::RESUME_FAILED_MARK_TTL,
                        )
                    };
                    if changed {
                        self.note_agent_resume_marks_changed();
                    }
                }
                let first = !self
                    .message_queue
                    .wake_failed
                    .contains_key(&target.terminal_id);
                if first {
                    tracing::info!(
                        event = "agent.autosuspend.wake_failed",
                        pane = %pane,
                        reason = %reason,
                        "could not wake an auto-suspended agent for a message yet; it stays queued"
                    );
                } else {
                    tracing::debug!(
                        event = "agent.autosuspend.wake_failed",
                        pane = %pane,
                        reason = %reason,
                        "could not wake an auto-suspended agent for a message yet; it stays queued"
                    );
                }
                let failure = self
                    .message_queue
                    .wake_failed
                    .entry(target.terminal_id.clone())
                    .or_insert_with(|| WakeFailure {
                        since: now,
                        last_attempt: now,
                        reason: reason.clone(),
                        notified: false,
                    });
                failure.last_attempt = now;
                failure.reason.clone_from(&reason);
                Some(if exiting {
                    // Right after the suspend its process is still exiting:
                    // the normal wait, not a failure.
                    WAKE_PENDING_REASON.to_string()
                } else if no_shell {
                    WAKE_NO_SHELL_REASON.to_string()
                } else {
                    format!(
                        "herdr had suspended it while idle and could not wake it ({reason}); it stays queued and herdr tries again"
                    )
                })
            }
        }
    }

    /// Fork (autosuspend): once a wake has failed for [`WAKE_NOTICE_AFTER`],
    /// tell the senders of the messages waiting for `terminal_id`, once.
    fn notify_failed_wake(&mut self, terminal_id: &str, now: Instant, now_unix: u64) -> bool {
        let Some(failure) = self
            .message_queue
            .wake_failed
            .get_mut(terminal_id)
            .filter(|failure| !failure.notified && now >= failure.since + WAKE_NOTICE_AFTER)
        else {
            return false;
        };
        failure.notified = true;
        let reason = failure.reason.clone();
        // Fork (restart screen): a wake that failed this long gave up.
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            if terminal.fail_suspended_agent_resume(
                crate::terminal::AgentResumeKind::Wake,
                now + super::agent_suspend::RESUME_FAILED_MARK_TTL,
            ) {
                self.note_agent_resume_marks_changed();
            }
        }
        let waiting: Vec<QueuedMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == terminal_id && !entry.herdr_notice)
            .cloned()
            .collect();
        for entry in waiting {
            if entry.legacy || (entry.from_terminal.is_none() && entry.message.from_pane.is_none())
            {
                continue;
            }
            let text = format!(
                "herdr: your message {} to {} ({}) is waiting: herdr had suspended it while idle and could not wake it ({reason}). It stays queued until the agent is activated or the message expires.",
                entry.id(),
                notice_target_name(&entry),
                entry.message.to_pane,
            );
            self.send_stuck_notice(&entry, text, now_unix);
        }
        true
    }

    pub(crate) fn message_queue_pass(&mut self, now: Instant, now_unix: u64) -> bool {
        if self.message_queue.is_empty() && !self.message_queue.delivery.is_active() {
            return false;
        }
        let due = self.message_queue.due
            || self.message_queue.retry_at.is_some_and(|at| now >= at)
            || self
                .message_queue
                .delivery
                .next_deadline()
                .is_some_and(|at| now >= at)
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
        let mut next: Vec<Instant> = Vec::new();
        // Fork (lost-message fix): batches waiting for their Enter first.
        changed |= self.drive_delivery_watch(now, now_unix, &mut next);

        let expired: Vec<QueuedMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.expires_unix <= now_unix)
            .cloned()
            .collect();
        if !expired.is_empty() {
            // Why each waited, read before they leave the queue.
            let reasons: Vec<String> = expired
                .iter()
                .map(|entry| self.queued_wait_reason(entry))
                .collect();
            let ids: Vec<String> = expired.iter().map(|entry| entry.id().to_string()).collect();
            self.message_queue.finish(&ids, OUTCOME_EXPIRED, now_unix);
            for (entry, reason) in expired.iter().zip(reasons) {
                self.send_expiry_notice(entry, &reason, now_unix);
            }
            changed = true;
        }

        // Targets in the order their oldest message was queued.
        let mut targets: Vec<(String, Option<String>)> = Vec::new();
        for entry in &self.message_queue.entries {
            if !targets.iter().any(|(id, _)| *id == entry.terminal_id) {
                targets.push((entry.terminal_id.clone(), entry.session.clone()));
            }
        }
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
        self.message_queue
            .settle_screens
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue
            .user_blocked
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue
            .wake_failed
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue
            .delivery
            .typing_wait
            .retain(|terminal, _| live.contains(terminal));
        self.message_queue
            .interrupted_at
            .retain(|terminal, at| live.contains(terminal) && now < *at + INTERRUPT_WAIT);
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
                    // Fork (lost-message fix): a new agent process, a new
                    // input box: earlier typing attempts no longer count.
                    entry.attempts = 0;
                    entry.typed_unix = None;
                }
            }
            self.message_queue.save();
        }
        let terminal_id = target.terminal_id.clone();
        let mut check = self.message_check(&target);
        // Fork: a do-not-disturb an agent turned on holds the rest, not the
        // coordinator's messages (src/app/tab_dnd.rs): those go alone.
        let coordinator_only = matches!(&check, Check::Wait(reason) if reason == super::tab_dnd::DND_WAIT_REASON)
            && self.message_queue.entries.iter().any(|entry| {
                entry.terminal_id == terminal_id
                    && entry.attempts < submit::MAX_SUBMIT_ATTEMPTS
                    && entry.from_coordinator
            });
        if coordinator_only {
            check = self.message_check_from(&target, true, true);
        }
        let (pane, coordinator, hook_ended) = match check {
            Check::Missing => return self.target_missing(&terminal_id, now, now_unix, next),
            Check::Wait(reason) => {
                self.message_queue.missing_since.remove(&terminal_id);
                self.message_queue.ready_since.remove(&terminal_id);
                self.message_queue.settle_screens.remove(&terminal_id);
                // Fork (autosuspend): wake an agent herdr parked for idling
                // (again, after a failed wake).
                if reason == SUSPENDED_REASON && self.auto_suspended(&terminal_id) {
                    self.message_queue.user_blocked.remove(&terminal_id);
                    self.message_queue.screens.remove(&terminal_id);
                    let retry_at = self
                        .message_queue
                        .wake_failed
                        .get(&terminal_id)
                        .map(|failure| failure.last_attempt + WAKE_RETRY);
                    let mut changed = false;
                    if retry_at.is_none_or(|at| now >= at) {
                        self.wake_auto_suspended(&target, now);
                        changed = !self.auto_suspended(&terminal_id);
                    }
                    changed |= self.notify_failed_wake(&terminal_id, now, now_unix);
                    if let Some(failure) = self.message_queue.wake_failed.get(&terminal_id) {
                        next.push(failure.last_attempt + WAKE_RETRY);
                    }
                    return changed;
                }
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
                if is_user_blocked(&reason) {
                    self.message_queue.screens.remove(&terminal_id);
                    // The screen reads blocked: its change is an event. The
                    // status report alone may clear without one: look again.
                    next.push(
                        now + if reason == PROGRAM_STATUS_BLOCKED_REASON {
                            RETRY
                        } else {
                            STUCK_SAMPLE
                        },
                    );
                    return self.watch_user_blocked_target(&target, &reason, now, now_unix, next);
                }
                self.message_queue.user_blocked.remove(&terminal_id);
                if reason == "working" {
                    return self.watch_stuck_target(&target, &reason, now, now_unix, next);
                }
                if reason == HOOK_LIVE_REASON {
                    // The hold runs out without an event when no Stop comes.
                    if let Some(until) = self.hook_live_until(&terminal_id) {
                        next.push(now + until.saturating_duration_since(Instant::now()));
                    }
                }
                self.message_queue.screens.remove(&terminal_id);
                return false;
            }
            Check::Ready {
                pane,
                coordinator,
                hook_ended,
            } => (pane, coordinator, hook_ended),
        };
        self.message_queue.screens.remove(&terminal_id);
        self.message_queue.missing_since.remove(&terminal_id);
        if hook_ended {
            self.settle_on_a_still_screen(&target, now);
        } else {
            self.message_queue.settle_screens.remove(&terminal_id);
        }
        let since = *self
            .message_queue
            .ready_since
            .entry(terminal_id.clone())
            .or_insert(now);
        if now < since + SETTLE {
            next.push(since + SETTLE);
            return false;
        }
        // Fork (lost-message fix): typed in twice into a box that stayed
        // empty; it waits for its expiry or a claim.
        if self.message_queue.submit_held(&terminal_id) {
            return false;
        }
        // Fork (lost-message fix): its text went in before a restart or a
        // live handoff and no Enter is known to have followed: look at the
        // input box before typing it again (submit.rs).
        match self.recover_typed_batch(&terminal_id, now, now_unix) {
            submit::Recovery::Proceed => {}
            submit::Recovery::Done(changed) => return changed,
            submit::Recovery::Busy => {
                next.push(now + RETRY);
                return false;
            }
            submit::Recovery::Wait => {
                // No input box (a dialog, a menu): look again, its text may
                // still be in the box behind it.
                next.push(now + RETRY);
                return self.watch_user_blocked_target(
                    &target,
                    super::typing_guard::TypingBlock::InputBoxAbsent.reason(),
                    now,
                    now_unix,
                    next,
                );
            }
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
            // Fork (lost-message fix): held ones wait; typed ones are the
            // recovery's (submit.rs).
            .filter(|entry| {
                entry.typed_unix.is_none() && entry.attempts < submit::MAX_SUBMIT_ATTEMPTS
            })
            .filter(|entry| !coordinator_only || entry.from_coordinator)
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
        // Fork (lost-message fix): Claude Code and Codex get the text now
        // and the Enter once it shows (submit.rs).
        let submit = self.submit_agent(&terminal_id);
        let typed = match submit {
            Some(agent) => self.start_submit(
                format!("agent-message:{first}"),
                &terminal_id,
                &pane,
                &items,
                coordinator,
                from.as_ref(),
                agent,
                now,
            ),
            None => self.type_message(
                format!("agent-message:{first}"),
                &terminal_id,
                &pane,
                &items,
                coordinator,
                from.as_ref(),
            ),
        };
        let paste = submit.is_none() && !self.reads_messages(&terminal_id);
        match typed {
            Ok(()) if submit.is_some() => {
                self.message_queue.delivery.typing_wait.remove(&terminal_id);
                self.message_queue.ready_since.remove(&terminal_id);
                self.message_queue.settle_screens.remove(&terminal_id);
                self.message_queue.user_blocked.remove(&terminal_id);
                true
            }
            Ok(()) => {
                self.message_queue.delivery.typing_wait.remove(&terminal_id);
                if hook_ended {
                    // Typed into a screen that read working, on the Stop
                    // hook's word and a screen that stood still for SETTLE
                    // (a lost UserPromptSubmit report or a blocking Stop
                    // hook would make this mid-turn; its turn moves the
                    // screen).
                    tracing::info!(
                        target = %terminal_id,
                        messages = ids.len(),
                        "message queue: delivered on the target's Stop hook while its screen read working"
                    );
                }
                self.message_queue.ready_since.remove(&terminal_id);
                self.message_queue.settle_screens.remove(&terminal_id);
                self.message_queue.interrupted_at.remove(&terminal_id);
                self.message_queue.user_blocked.remove(&terminal_id);
                let delivered = self.message_queue.entries_with(&ids);
                self.message_queue.finish(&ids, OUTCOME_DELIVERED, now_unix);
                self.watch_confirmation(&terminal_id, delivered, paste, now);
                if self.message_queue.queued_for(&terminal_id) {
                    // The rest goes in once the target settled again.
                    self.message_queue.due = true;
                }
                true
            }
            Err(response) => {
                let code = error_code(&response);
                if let Some(reason) = user_blocked_reason(&code) {
                    // No input box (a dialog, a menu) or its own report
                    // reads blocked: nothing was typed. Look again after a
                    // fresh SETTLE.
                    self.message_queue
                        .ready_since
                        .insert(terminal_id.clone(), now);
                    next.push(now + SETTLE);
                    return self.watch_user_blocked_target(&target, reason, now, now_unix, next);
                }
                self.message_queue.user_blocked.remove(&terminal_id);
                // Fork (lost-message fix): the user's typing or draft is
                // why it waits (agents.queued), logged once per wait.
                if code == "user_typing" {
                    self.note_typing_wait(&terminal_id, &response);
                } else {
                    self.message_queue.delivery.typing_wait.remove(&terminal_id);
                }
                if !retryable(&code) && code != "busy" {
                    tracing::warn!(code, "message queue: delivery failed, retrying");
                }
                next.push(now + RETRY);
                false
            }
        }
    }

    /// Why `entry` waits now (the reason `agents.queued` shows), for its
    /// expiry notice.
    fn queued_wait_reason(&self, entry: &QueuedMessage) -> String {
        let Some(target) = self.message_target(&entry.terminal_id, entry.session.as_deref()) else {
            return "offline".to_string();
        };
        match self.message_check(&target) {
            Check::Ready { .. } => self
                .message_queue
                .user_blocked
                .get(&target.terminal_id)
                .map(|blocked| blocked.reason.clone())
                .or_else(|| {
                    self.message_queue
                        .delivery
                        .typing_wait
                        .get(&target.terminal_id)
                        .cloned()
                })
                .unwrap_or_else(|| "not typed in yet".to_string()),
            check => MessageCheck(check).status(),
        }
    }

    /// A target waits on its user (`reason`: its screen or its own status
    /// report reads blocked, or its input box is not on screen). Keeps its
    /// queued messages alive (up to [`MESSAGE_TTL_USER_BLOCKED_S`] after
    /// each was sent); once the wait has lasted [`STUCK_WAIT`] tells its
    /// user (one agent card per wait) and every sender of a message that
    /// has waited [`STUCK_WAIT`] (one notice per sender: at once the first
    /// time, again in rounds per target after each
    /// [`USER_BLOCKED_NOTICE_BACKOFF`] step, never more often than
    /// [`STUCK_NOTICE_EVERY`]). Returns whether a notice went out.
    fn watch_user_blocked_target(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        reason: &str,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let terminal_id = target.terminal_id.clone();
        let blocked = self
            .message_queue
            .user_blocked
            .entry(terminal_id.clone())
            .or_insert_with(|| UserBlocked {
                since: now,
                since_unix: now_unix,
                reason: reason.to_string(),
                notices: 0,
                carded: false,
            });
        blocked.reason = reason.to_string();
        let (since, notices, carded) = (blocked.since, blocked.notices, blocked.carded);

        // Its messages stay queued while it waits (each pass that sees the
        // wait moves the expiry; at most once an hour per message).
        let mut extended = false;
        for entry in &mut self.message_queue.entries {
            if entry.terminal_id != terminal_id {
                continue;
            }
            let cap = entry
                .message
                .unix
                .saturating_add(MESSAGE_TTL_USER_BLOCKED_S);
            let wanted = now_unix.saturating_add(MESSAGE_TTL_S).min(cap);
            if entry.expires_unix.saturating_sub(now_unix) <= MESSAGE_TTL_S / 2
                && wanted > entry.expires_unix
            {
                entry.expires_unix = wanted;
                extended = true;
            }
        }
        if extended {
            self.message_queue.save();
        }

        let blocked_for = now.saturating_duration_since(since);
        if blocked_for < STUCK_WAIT {
            next.push(since + STUCK_WAIT);
            return false;
        }
        if !carded {
            self.user_blocked_card(target, reason, blocked_for);
            if let Some(blocked) = self.message_queue.user_blocked.get_mut(&terminal_id) {
                blocked.carded = true;
            }
        }
        let last = self
            .message_queue
            .stuck_notice_at
            .get(&terminal_id)
            .copied();
        let due_at = match (notices, last) {
            (0, last) => last.map(|at| at + STUCK_NOTICE_EVERY),
            (sent, Some(at)) => {
                let step = USER_BLOCKED_NOTICE_BACKOFF
                    [(sent - 1).min(USER_BLOCKED_NOTICE_BACKOFF.len() - 1)];
                Some(at + step.max(STUCK_NOTICE_EVERY))
            }
            (_, None) => None,
        };
        // A round (every sender with a message held STUCK_WAIT hears again)
        // follows the per-target backoff; a sender that has not heard about
        // this target yet hears as soon as its message has waited STUCK_WAIT.
        let round_due = match due_at {
            Some(at) if now < at => {
                next.push(at);
                false
            }
            _ => true,
        };
        let candidates: Vec<QueuedMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| {
                entry.terminal_id == terminal_id
                    && !entry.legacy
                    && (entry.from_terminal.is_some() || entry.message.from_pane.is_some())
            })
            .cloned()
            .collect();
        let waited_enough = |entry: &QueuedMessage| {
            now_unix.saturating_sub(entry.message.unix) >= STUCK_WAIT.as_secs()
        };
        if let Some(youngest) = candidates
            .iter()
            .filter(|entry| !waited_enough(entry))
            .map(|entry| entry.message.unix)
            .min()
        {
            // A later message reaches STUCK_WAIT at this time.
            let wait = youngest
                .saturating_add(STUCK_WAIT.as_secs())
                .saturating_sub(now_unix);
            next.push(now + Duration::from_secs(wait));
        }
        // One notice per distinct reachable sender: its oldest message that
        // waited STUCK_WAIT, one it was not told about yet first.
        let sender_key = |entry: &QueuedMessage| match &entry.from_terminal {
            Some(terminal) => format!("t:{terminal}"),
            None => format!("p:{}", entry.message.from_pane.as_deref().unwrap_or("")),
        };
        let mut picks: Vec<QueuedMessage> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        for entry in &candidates {
            let key = sender_key(entry);
            if keys.contains(&key) || !waited_enough(entry) {
                continue;
            }
            keys.push(key.clone());
            let mine = candidates
                .iter()
                .filter(|other| sender_key(other) == key && waited_enough(other));
            let heard = mine.clone().any(|other| other.stuck_notified);
            if !round_due && heard {
                continue;
            }
            let Some(pick) = mine
                .clone()
                .find(|other| !other.stuck_notified)
                .or_else(|| mine.clone().next())
            else {
                continue;
            };
            if self.stuck_notice_sender(pick).is_some() {
                picks.push(pick.clone());
            }
        }
        let mut sent = false;
        for entry in picks {
            let target_name = notice_target_name(&entry);
            let text = format!(
                "herdr: your message {} to {target_name} ({}) has waited {}; {target_name} is waiting on its user ({reason}) for {}. It stays queued (up to {}h after you sent it) and goes in once its user is done; check it with agents_messages.",
                entry.id(),
                entry.message.to_pane,
                minutes(Duration::from_secs(
                    now_unix.saturating_sub(entry.message.unix)
                )),
                minutes(blocked_for),
                MESSAGE_TTL_USER_BLOCKED_S / 3600,
            );
            tracing::info!(
                message = %entry.id(),
                target = %entry.message.to_pane,
                reason,
                blocked_s = blocked_for.as_secs(),
                notices,
                "message queue: a queued message waits on its target's user; telling its sender"
            );
            if !self.send_stuck_notice(&entry, text, now_unix) {
                continue;
            }
            sent = true;
            if let Some(queued) = self
                .message_queue
                .entries
                .iter_mut()
                .find(|queued| queued.id() == entry.id())
            {
                queued.stuck_notified = true;
            }
        }
        if !sent {
            return false;
        }
        if round_due {
            if let Some(blocked) = self.message_queue.user_blocked.get_mut(&terminal_id) {
                blocked.notices += 1;
            }
            self.message_queue.stuck_notice_at.insert(terminal_id, now);
        }
        self.message_queue.save();
        true
    }

    /// One agent card from `target`'s pane telling its user that agent
    /// messages wait on them (once per wait; [`App::watch_user_blocked_target`]).
    fn user_blocked_card(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        reason: &str,
        blocked_for: Duration,
    ) {
        let Some(pane) = self.public_pane_id(target.ws_idx, target.pane_id) else {
            return;
        };
        let Some(sender) = self.notice_sender(&pane) else {
            return;
        };
        let held = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == target.terminal_id)
            .count();
        let title = super::agent_notices::sanitize_title(&format!(
            "{held} agent message{} wait{} for you",
            if held == 1 { "" } else { "s" },
            if held == 1 { "s" } else { "" },
        ))
        .unwrap_or_default();
        let body = super::agent_notices::sanitize_body(&format!(
            "It has waited on you for {} ({reason}); they go in once you are done.",
            minutes(blocked_for)
        ));
        tracing::info!(
            target = %target.terminal_id,
            held,
            reason,
            "message queue: messages wait on the target's user; showing an agent card"
        );
        match self.agent_notices.notify(
            sender,
            crate::api::schema::AgentNoticeKind::Warning,
            title,
            body,
            crate::coordinator::now_unix(),
        ) {
            Ok((_, outcome)) => {
                if outcome == crate::api::schema::AgentNotifyOutcome::Shown {
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                }
            }
            Err(refused) => {
                tracing::debug!(code = %refused.code, "message queue: the wait card was not shown");
            }
        }
    }

    /// Tell an expired message's sender that it was never typed in
    /// (`reason`: why it waited). A sender that is gone, or herdr (a notice
    /// expiring), gets nothing.
    fn send_expiry_notice(&mut self, entry: &QueuedMessage, reason: &str, now_unix: u64) {
        if entry.legacy || (entry.from_terminal.is_none() && entry.message.from_pane.is_none()) {
            return;
        }
        let target_name = notice_target_name(entry);
        let text = format!(
            "herdr: your message {} to {target_name} ({}) expired after {} and was never typed in (it waited: {reason}). Send it again if it still matters.",
            entry.id(),
            entry.message.to_pane,
            minutes(Duration::from_secs(
                now_unix.saturating_sub(entry.message.unix)
            )),
        );
        tracing::info!(
            message = %entry.id(),
            target = %entry.message.to_pane,
            reason,
            "message queue: a queued message expired; telling its sender"
        );
        self.send_stuck_notice(entry, text, now_unix);
    }

    /// A target freed by its Stop hook alone (its screen reads working): the
    /// screen above its prompt box must stay still for the whole [`SETTLE`].
    /// A user's blocking Stop hook continues the turn without a new
    /// UserPromptSubmit, and a start report can be lost; that live turn's
    /// spinner timer or streamed text changes the region, while Claude's
    /// "Waiting for N background agents" line does not (the ticking agent
    /// panel sits under the prompt box). Sampled once per queue pass while
    /// such a target is pending (its first Ready pass, events, the SETTLE
    /// deadline), never per frame. The first sample, or a change, restarts
    /// the settle from `now`. Without a runtime nothing is sampled.
    fn settle_on_a_still_screen(
        &mut self,
        target: &super::terminal_targets::TerminalTarget,
        now: Instant,
    ) {
        let Some(hash) = self
            .lookup_runtime_sender(target.ws_idx, target.pane_id)
            .map(|runtime| screen_hash(&runtime.detection_text()))
        else {
            return;
        };
        let previous = self
            .message_queue
            .settle_screens
            .insert(target.terminal_id.clone(), hash);
        if previous != Some(hash) {
            if previous.is_some() {
                tracing::debug!(
                    target = %target.terminal_id,
                    "message queue: a hook-ended target's screen changed while settling; settle restarted"
                );
            }
            self.message_queue
                .ready_since
                .insert(target.terminal_id.clone(), now);
        }
    }

    /// A target waits on `working`: sample the screen above its prompt box
    /// every [`STUCK_SAMPLE`], and once it has not changed for
    /// [`STUCK_SCREEN`] while a message waited [`STUCK_WAIT`], tell that
    /// message's sender (once per message, once per target per
    /// [`STUCK_NOTICE_EVERY`]). A live turn's spinner and streamed text
    /// change the region; a panel ticking under the prompt box does not.
    /// Fork: a target with subagents running skips the frozen-screen test
    /// (their progress keeps it moving) and its notice says it is running
    /// a subagent; still once per message and per [`STUCK_NOTICE_EVERY`].
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
        // Fork: a working agent with subagents running waits on them (a
        // foreground subagent holds its turn); its screen moves with their
        // progress, so the sender hears why without the frozen-screen test.
        let subagents = self.target_subagents(target, now);
        if (subagents.is_none() && unchanged < STUCK_SCREEN) || !cooled {
            return false;
        }
        // The oldest such message whose sender can still be reached (a
        // herdr notice has none).
        let candidates: Vec<QueuedMessage> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| {
                entry.terminal_id == terminal_id
                    && !entry.stuck_notified
                    && !entry.legacy
                    && (entry.from_terminal.is_some() || entry.message.from_pane.is_some())
                    && now_unix.saturating_sub(entry.message.unix) >= STUCK_WAIT.as_secs()
            })
            .cloned()
            .collect();
        let Some(entry) = candidates
            .into_iter()
            .find(|entry| self.stuck_notice_sender(entry).is_some())
        else {
            return false;
        };
        let status = self
            .message_status_name(target)
            .unwrap_or_else(|| reason.to_string());
        let target_name = notice_target_name(&entry);
        let waited = minutes(Duration::from_secs(
            now_unix.saturating_sub(entry.message.unix),
        ));
        let text = match subagents {
            Some((count, running)) => format!(
                "herdr: your message {} to {target_name} ({}) has waited {waited}; {target_name} is running {} ({}); it will read your message when that turn ends. It stays queued; check it with agents_messages.",
                entry.id(),
                entry.message.to_pane,
                if count == 1 {
                    "a subagent".to_string()
                } else {
                    format!("{count} subagents")
                },
                minutes(running),
            ),
            None => format!(
                "herdr: your message {} to {target_name} ({}) has waited {waited}; {target_name} reads {status} but its screen has not changed for {} (queued: {reason}). It stays queued; check it with agents_messages.",
                entry.id(),
                entry.message.to_pane,
                minutes(unchanged),
            ),
        };
        tracing::info!(
            message = %entry.id(),
            target = %entry.message.to_pane,
            unchanged_s = unchanged.as_secs(),
            subagents = subagents.map_or(0, |(count, _)| count),
            subagents_running_s = subagents.map_or(0, |(_, running)| running.as_secs()),
            "message queue: a queued message seems stuck; telling its sender"
        );
        if !self.send_stuck_notice(&entry, text, now_unix) {
            // Neither typed in nor queued: the message's one notice and the
            // target's slot stay unused.
            return false;
        }
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

    /// Fork: the target's running subagents and how long they have run
    /// (`None` without any).
    fn target_subagents(
        &self,
        target: &super::terminal_targets::TerminalTarget,
        now: Instant,
    ) -> Option<(u32, Duration)> {
        let pane = self
            .state
            .workspaces
            .get(target.ws_idx)?
            .pane_state(target.pane_id)?;
        let (count, since) = self
            .state
            .terminals
            .get(&pane.attached_terminal_id)?
            .active_subagents_since()?;
        Some((count, now.saturating_duration_since(since)))
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

    /// The live sender of a queued message, for its stuck notice.
    fn stuck_notice_sender(
        &self,
        entry: &QueuedMessage,
    ) -> Option<super::terminal_targets::TerminalTarget> {
        entry
            .from_terminal
            .as_ref()
            .and_then(|terminal| self.public_pane_of_terminal(terminal))
            .or_else(|| entry.message.from_pane.clone())
            .and_then(|pane| self.resolve_agent_target(&pane).ok())
    }

    /// Tell `entry`'s sender `text` about its message (stuck, waiting on
    /// its target's user, expired), through the same delivery path. Logged
    /// in the message log. Returns whether the notice was typed in or
    /// queued; a sender that is gone gets nothing.
    fn send_stuck_notice(&mut self, entry: &QueuedMessage, text: String, now_unix: u64) -> bool {
        let message_id = entry.id().to_string();
        let Some(sender_target) = self.stuck_notice_sender(entry) else {
            return false;
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
            false,
            now_unix,
        ) {
            Ok(MessageDelivery::Sent) => {
                let mut sent = line;
                sent.outcome = messages::OUTCOME_SENT.into();
                self.message_queue.log(sent);
                true
            }
            Ok(MessageDelivery::Queued { .. }) => true,
            Err(refused) => {
                tracing::warn!(
                    code = %refused.code,
                    "message queue: the stuck notice could not reach its sender"
                );
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
        self.message_queue.settle_screens.remove(terminal_id);
        let since = *self
            .message_queue
            .missing_since
            .entry(terminal_id.to_string())
            .or_insert(now);
        if now < since + GONE_GRACE {
            next.push(since + GONE_GRACE);
            return false;
        }
        // herdr's own notices about a pane (a recreated directory) wait
        // while the pane is there, still records the session they were
        // queued for, and its agent is not detected yet (a slow resume), up
        // to the user-blocked hold; delivered once when it is. An agent
        // that exited (its session cleared) drops them after the grace.
        let pane_session = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.to_string() == terminal_id)
            .map(super::coordinator::terminal_session);
        let held = |entry: &QueuedMessage| {
            entry.herdr_notice
                && entry.session.is_some()
                && pane_session.as_ref() == Some(&entry.session)
                && now_unix
                    < entry
                        .message
                        .unix
                        .saturating_add(MESSAGE_TTL_USER_BLOCKED_S)
        };
        let mut holding = false;
        let ids: Vec<String> = self
            .message_queue
            .entries
            .iter()
            .filter(|entry| entry.terminal_id == terminal_id)
            .filter(|entry| {
                let keep = held(entry);
                holding |= keep;
                !keep
            })
            .map(|entry| entry.id().to_string())
            .collect();
        if holding {
            next.push(now + GONE_GRACE);
        } else {
            self.message_queue.missing_since.remove(terminal_id);
        }
        if ids.is_empty() {
            return false;
        }
        self.message_queue.finish(&ids, OUTCOME_DROPPED, now_unix);
        true
    }
}

#[cfg(test)]
mod tests;
