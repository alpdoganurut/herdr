//! The agents model (fork, agents v2): every tab is part of herdr+, and one
//! server check decides what an agent may do to another tab (`agents.*`).
//!
//! The caller is always a pane (`caller_pane`, resolved by the MCP or the
//! CLI route); the server derives the actor (user, coordinator, agent) from
//! its own records, never from parameters.
//!
//! Digest hygiene: the params structs reference only the types defined in
//! this module, strings, numbers and booleans, never an existing schema type,
//! so no other schema change can move a request digest (`agents.set_meta` is
//! advertised to client shells and frozen). Results may reference existing
//! result types. Every enum carries an `Unknown` fallback, and every result
//! field past the required core is optional or defaulted, so an older client
//! can read a newer server's reply.

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

/// The `agents.*` method names of the agents model on the wire.
pub mod method {
    pub const ACTOR: &str = "agents.actor";
    pub const DIRECTORY: &str = "agents.directory";
    pub const READ: &str = "agents.read";
    pub const OPEN_TAB: &str = "agents.open_tab";
    pub const SEND_MESSAGE: &str = "agents.send_message";
    pub const RENAME_TAB: &str = "agents.rename_tab";
    pub const MOVE_TAB: &str = "agents.move_tab";
    pub const SET_META: &str = "agents.set_meta";
    pub const CLOSE_TAB: &str = "agents.close_tab";
    pub const REOPEN_TAB: &str = "agents.reopen_tab";
    pub const NOTES_APPEND: &str = "agents.notes_append";
    pub const CHECKPOINT: &str = "agents.checkpoint";
    pub const ACTIONS: &str = "agents.actions";
    pub const CHECK: &str = "agents.check";

    /// The one method advertised to client shells (the TUI's "Set role…").
    #[cfg(test)]
    pub const CLIENT_SHELL: [&str; 1] = [SET_META];

    /// Every method.
    #[cfg(test)]
    pub const ALL: [&str; 14] = [
        ACTOR,
        DIRECTORY,
        READ,
        OPEN_TAB,
        SEND_MESSAGE,
        RENAME_TAB,
        MOVE_TAB,
        SET_META,
        CLOSE_TAB,
        REOPEN_TAB,
        NOTES_APPEND,
        CHECKPOINT,
        ACTIONS,
        CHECK,
    ];
}

/// The error codes the `agents.*` methods of the agents model answer with.
pub mod error_code {
    /// A malformed parameter.
    pub const INVALID_PARAMS: &str = "invalid_params";
    /// The target (pane, tab, agent, closed entry) does not exist.
    pub const NOT_FOUND: &str = "not_found";
    /// `caller_pane` does not resolve to a live pane, or no longer to the
    /// caller's own pane.
    pub const CALLER_UNRESOLVED: &str = "caller_unresolved";
    /// The target is outside the caller's team: read and message only.
    pub const OUTSIDE_TEAM: &str = "outside_team";
    /// The caller's current turn did not start from its user's input.
    pub const NON_USER_TURN: &str = "non_user_turn";
    /// A shell screen outside the caller's team (U6).
    pub const SHELL_PRIVATE: &str = "shell_private";
    /// The coordinator's tab: no agent renames, moves or closes it.
    pub const PROTECTED_TAB: &str = "protected_tab";
    /// Only the owner (or the coordinator in a user turn) may do this.
    pub const OWN_ONLY: &str = "own_only";
    /// The target is the caller itself where that makes no sense.
    pub const INVALID_TARGET: &str = "invalid_target";
    /// A message to a shell pane.
    pub const NOT_AN_AGENT: &str = "not_an_agent";
    /// The target is working; not typed in.
    pub const BUSY: &str = "busy";
    /// The user is typing in the target.
    pub const USER_TYPING: &str = "user_typing";
    /// The target is suspended, launch-pending or gone.
    pub const OFFLINE: &str = "offline";
    /// The target is blocked on its user.
    pub const BLOCKED: &str = "blocked";
    /// A message rate limit.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// Two agents answering each other forever.
    pub const LOOP_GUARD: &str = "loop_guard";
    /// Too many agent-opened tabs.
    pub const SPAWN_LIMIT: &str = "spawn_limit";
    /// A close target is working, starting or between steps.
    pub const TARGET_BUSY: &str = "target_busy";
    /// A shell pane runs a job other than its shell (`force` closes anyway).
    pub const SHELL_BUSY: &str = "shell_busy";
    /// An agent in the tab could not be reopened after the close.
    pub const NOT_RESUMABLE: &str = "not_resumable";
    /// An agent in the tab has no graceful exit command.
    pub const NO_GRACEFUL_EXIT: &str = "no_graceful_exit";
    /// The CLI verb is not available to agents.
    pub const AGENT_REFUSED: &str = "agent_refused";
    /// The underlying operation failed.
    pub const FAILED: &str = "failed";
}

// ----- enums ----------------------------------------------------------------

/// What a pane hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentPaneKind {
    /// An agent pane: live, suspended or launch-pending.
    Agent,
    Shell,
    /// A kind this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// Who acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentActorKind {
    User,
    Coordinator,
    Agent,
    /// A pane with no live agent (its user types there).
    Shell,
    /// An actor this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// What a caller may do to a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsAccess {
    /// The caller's own tab.
    #[serde(rename = "self")]
    SelfTab,
    /// A tab in the caller's team (or any tab, for the coordinator).
    Edit,
    /// Read and message only.
    ReadMessage,
    /// An access this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// Whether a caller may read a pane's screen (U6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsScreenAccess {
    Full,
    /// Only that the pane exists (a shell outside the caller's team).
    Exists,
    /// An access this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// Where a pane's current turn came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsTurnOrigin {
    /// The pane's user submitted input.
    User,
    /// Another agent's message.
    AgentMessage,
    /// A herdr wake-up (the coordinator).
    HerdrWake,
    /// A scripted prompt (`agent.prompt`, `pane.send_*`).
    Programmatic,
    /// The agent started it on its own (a background completion, a queued
    /// prompt).
    SelfStarted,
    /// Not known: after a restart, a handoff, a reopen, or an origin this
    /// side does not know.
    #[serde(other)]
    Unknown,
}

/// A detail on a turn origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsOriginDetail {
    /// The turn carried over a short screen-detected idle flap.
    Bridged,
    /// The user input came through a terminal-attach session.
    ClientAttach,
    /// A detail this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// What an action did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsActionOutcome {
    Ok,
    Denied,
    Deferred,
    Closing,
    Aborted,
    Failed,
    /// An outcome this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// What `agents.close_tab` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsCloseOutcome {
    /// The tab is closed.
    Closed,
    /// The agents are exiting; the tab closes once they have.
    Closing,
    /// The caller's own tab: it closes once the caller is idle.
    Deferred,
    /// An outcome this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// What `agents.send_message` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsMessageOutcome {
    /// Typed into the target.
    Sent,
    /// Only logged (a reply to an asker that is busy now).
    Logged,
    /// An outcome this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

/// The action `agents.check` decides, for CLI verbs without a dedicated
/// method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsCheckAction {
    /// Tab color, important, reminders.
    Cosmetic,
    /// Suspend or restart an agent.
    SuspendRestart,
    /// Activate a suspended agent.
    Activate,
    /// Type into a shell pane.
    ShellInput,
    /// Close a pane that is not its tab's last.
    ClosePane,
    /// Make a team, set its purpose, rename or move a group.
    TeamStructure,
    /// An action this side does not know (a newer peer); always denied.
    #[serde(other)]
    Unknown,
}

/// The screen `agents.read` reads.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentsReadSource {
    Visible,
    #[default]
    Recent,
    RecentUnwrapped,
    Detection,
    /// A source this side does not know (a newer peer): read `recent`.
    #[serde(other)]
    Unknown,
}

/// How `agents.read` returns the screen.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentsReadFormat {
    #[default]
    Text,
    Ansi,
    /// A format this side does not know (a newer peer): plain text.
    #[serde(other)]
    Unknown,
}

/// Who did something (opened a tab, closed it, suspended an agent): set by
/// the server, never from parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentsWho {
    User,
    Coordinator,
    /// An agent, named as it was at the time.
    Agent {
        name: String,
    },
    /// An actor this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

impl AgentsWho {
    /// Whether an agent (not the user, not the coordinator) did it.
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }

    /// "the user", "the coordinator", the agent's name.
    pub fn describe(&self) -> String {
        match self {
            Self::User | Self::Unknown => "the user".into(),
            Self::Coordinator => "the coordinator".into(),
            Self::Agent { name } => name.clone(),
        }
    }
}

// ----- params -----------------------------------------------------------------

/// `agents.actor`: who the caller is, its team, its turn and its rights, with
/// the `team.context` change it has not been told yet (one call per MCP tool
/// call).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsActorParams {
    pub caller_pane: String,
    /// Mark the team change as told (as `team.context`'s `ack`).
    #[serde(default)]
    pub ack: bool,
    /// The whole roster, not just the changes since the last ack.
    #[serde(default)]
    pub full: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_key: Option<String>,
}

/// `agents.directory`: every group and tab, scoped. Without a filter and
/// with a caller: the caller's group in full plus every group's counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsDirectoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
    /// A group id or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A team's group id or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Every group's tabs.
    #[serde(default)]
    pub all: bool,
    /// Leave out shell-only tabs.
    #[serde(default)]
    pub agents_only: bool,
    /// The most tab rows (the rest is counted in `truncated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Also the recently closed tabs (reads the closed-sessions store).
    #[serde(default)]
    pub include_closed: bool,
}

/// `agents.read`: a pane's screen, as `pane.read` reads it; shell screens only
/// for the pane's team and the coordinator (U6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsReadParams {
    pub caller_pane: String,
    /// A pane id, agent name, tab id or unique tab label.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentsReadSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<AgentsReadFormat>,
}

/// `agents.open_tab`: open a tab (a shell without `agent`), set its meta,
/// join its team and type the kickoff, in one step.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsOpenTabParams {
    pub caller_pane: String,
    /// An existing group id or label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A new group with this label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_group: Option<String>,
    /// The top space (ungrouped).
    #[serde(default)]
    pub priority: bool,
    /// `claude` or `codex`; absent opens a shell tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The agent name (outside a team; in a team the role names it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The first instruction for the new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kickoff: Option<String>,
    /// The tab label (default: the agent name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `agents.send_message`: type a message into an idle agent. Never waits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsSendMessageParams {
    pub caller_pane: String,
    /// An agent pane id, agent name or unique tab label.
    pub to: String,
    pub text: String,
    /// The id of the message this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// `agents.rename_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsRenameTabParams {
    pub caller_pane: String,
    pub target: String,
    pub name: String,
}

/// `agents.move_tab`: into a group, a new group, or the top space.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsMoveTabParams {
    pub caller_pane: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_group: Option<String>,
    #[serde(default)]
    pub priority: bool,
}

/// `agents.set_meta` (client-shell): a pane's role and note. An absent field
/// stays; an empty one clears it. Absent `caller_pane` means the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsSetMetaParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `agents.close_tab`: exit the tab's agents gracefully, then close it, with
/// every agent resumable from the closed sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsCloseTabParams {
    pub caller_pane: String,
    pub target: String,
    /// Close even when an agent cannot be reopened.
    #[serde(default)]
    pub allow_unresumable: bool,
    /// Close a shell running a job, or an agent without a graceful exit.
    #[serde(default)]
    pub force: bool,
}

/// `agents.reopen_tab`: a closed tab from the closed sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsReopenTabParams {
    pub caller_pane: String,
    pub closed_id: String,
}

/// `agents.notes_append`: append to another pane's notes (a teammate's).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsNotesAppendParams {
    pub caller_pane: String,
    pub target: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default)]
    pub stamp: bool,
}

/// `agents.checkpoint`: add a checkpoint to another pane's timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsCheckpointParams {
    pub caller_pane: String,
    pub target: String,
    /// `decision`, `milestone`, `failure`, `bookmark` or `note`.
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// `agents.actions`: the newest action-log entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsActionsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only entries about this pane or tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Only entries by this actor (pane id or name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

/// `agents.check`: decide an action for the CLI route and log it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsCheckParams {
    pub caller_pane: String,
    pub action: AgentsCheckAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

// ----- results --------------------------------------------------------------

/// A caller's or a directory row's team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsTeamRef {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Who set the purpose, as shown (`the user`, `the coordinator`, a name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_by: Option<String>,
    #[serde(default)]
    pub member_count: u32,
    /// The members, in join order (only in `agents.actor`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<super::team::TeamMemberInfo>,
}

/// A pane's current turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsTurnInfo {
    /// The effective origin: a scripted write into a user turn makes it that
    /// write's origin.
    pub origin: AgentsTurnOrigin,
    /// The origin is the user's and nothing programmatic landed since.
    #[serde(default)]
    pub user_turn: bool,
    #[serde(default)]
    pub poisoned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<AgentsOriginDetail>,
    /// The message that started the turn (`agent_message`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Its sender's pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pane: Option<String>,
    /// The wake that started the turn (`herdr_wake`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix: Option<u64>,
}

impl Default for AgentsTurnInfo {
    fn default() -> Self {
        Self {
            origin: AgentsTurnOrigin::Unknown,
            user_turn: false,
            poisoned: false,
            detail: None,
            message_id: None,
            from_pane: None,
            wake_seq: None,
            started_unix: None,
        }
    }
}

/// `agents.actor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsActorInfo {
    /// The caller's canonical public pane id.
    pub pane_id: String,
    pub kind: AgentActorKind,
    /// A live agent runs in the pane (not suspended, not launch-pending).
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// The agent name, else the agent kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The caller's team (absent outside a team, or when it left it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<AgentsTeamRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub turn: AgentsTurnInfo,
    /// One line: what the caller may do.
    #[serde(default)]
    pub rights: String,
    /// The team change the caller has not been told yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_key: Option<String>,
}

/// A group's team, in the directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsGroupTeam {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default)]
    pub member_count: u32,
}

/// One sidebar group (space), in sidebar order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsGroupInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    /// 1-based sidebar position (1 is the top space).
    #[serde(default)]
    pub number: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<AgentsGroupTeam>,
    #[serde(default)]
    pub tab_count: u32,
    #[serde(default)]
    pub agent_count: u32,
}

/// One pane of a directory tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsPaneInfo {
    pub pane_id: String,
    pub kind: AgentPaneKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(default)]
    pub suspended: bool,
    #[serde(default)]
    pub launch_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// A member of its group's team.
    #[serde(default)]
    pub member: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by: Option<AgentsWho>,
    #[serde(default)]
    pub subagents: u32,
    /// Omitted for a shell outside the caller's team (U6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub screen: AgentsScreenAccess,
}

/// One tab of the directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsTabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    /// The coordinator's tab: no agent renames, moves or closes it.
    #[serde(default)]
    pub protected: bool,
    /// What the caller may do here (only with a caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AgentsAccess>,
    #[serde(default)]
    pub panes: Vec<AgentsPaneInfo>,
}

/// A recently closed tab, for `agents_reopen_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsClosedInfo {
    pub closed_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub workspace_id: String,
    /// The agents' names.
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<AgentsWho>,
    #[serde(default)]
    pub closed_at: u64,
}

/// `agents.directory`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsDirectory {
    pub generated_unix: u64,
    /// The caller's canonical pane id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(default)]
    pub groups: Vec<AgentsGroupInfo>,
    #[serde(default)]
    pub tabs: Vec<AgentsTabInfo>,
    /// Tabs left out by `limit`.
    #[serde(default)]
    pub truncated: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recently_closed: Vec<AgentsClosedInfo>,
}

/// `agents.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsReadResult {
    pub pane_id: String,
    pub kind: AgentPaneKind,
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
}

/// `agents.open_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsOpenResult {
    pub tab_id: String,
    pub pane_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The new agent joined its group's team.
    #[serde(default)]
    pub member: bool,
}

/// `agents.send_message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsMessageResult {
    pub id: String,
    pub outcome: AgentsMessageOutcome,
    pub to_pane: String,
    #[serde(default)]
    pub to_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    /// The target's team (group id), when it is a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The target is not in the sender's team.
    #[serde(default)]
    pub cross_team: bool,
}

/// `agents.rename_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsRenameResult {
    pub tab_id: String,
    pub name: String,
}

/// `agents.move_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsMoveResult {
    pub previous_pane_id: String,
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
}

/// `agents.set_meta`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsSetMetaResult {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A team member's name now follows the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed: Option<bool>,
}

/// An agent `agents.close_tab` keeps resumable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsResumableInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub session: String,
}

/// An agent `agents.close_tab` cannot keep resumable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsUnresumableInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub reason: String,
}

/// `agents.close_tab`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsCloseResult {
    pub tab_id: String,
    pub outcome: AgentsCloseOutcome,
    #[serde(default)]
    pub resumable: Vec<AgentsResumableInfo>,
    #[serde(default)]
    pub unresumable: Vec<AgentsUnresumableInfo>,
    /// The closed-session ids (`closed` only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_ids: Vec<String>,
}

/// `agents.reopen_tab`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentsReopenResult {
    pub tab_id: String,
    #[serde(default)]
    pub pane_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// One line of `actions.jsonl`: an agent action, a denial, a completion, or
/// a user action on tabs and teams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentActionEntry {
    pub unix: u64,
    pub id: String,
    pub actor: AgentActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_name: Option<String>,
    /// `close_tab`, `rename_tab`, `send_message`, ...
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_tab: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    /// The actor's team (group id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_origin: Option<AgentsTurnOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_detail: Option<AgentsOriginDetail>,
    pub outcome: AgentsActionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_ids: Vec<String>,
}

/// The type names the params structs may reference (digest hygiene).
#[cfg(test)]
pub const PARAM_TYPES: [&str; 17] = [
    "AgentsActorParams",
    "AgentsDirectoryParams",
    "AgentsReadParams",
    "AgentsReadSource",
    "AgentsReadFormat",
    "AgentsOpenTabParams",
    "AgentsSendMessageParams",
    "AgentsRenameTabParams",
    "AgentsMoveTabParams",
    "AgentsSetMetaParams",
    "AgentsCloseTabParams",
    "AgentsReopenTabParams",
    "AgentsNotesAppendParams",
    "AgentsCheckpointParams",
    "AgentsActionsParams",
    "AgentsCheckParams",
    "AgentsCheckAction",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_falls_back_to_unknown() {
        assert_eq!(
            serde_json::from_str::<AgentPaneKind>(r#""robot""#).unwrap(),
            AgentPaneKind::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentActorKind>(r#""robot""#).unwrap(),
            AgentActorKind::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsAccess>(r#""robot""#).unwrap(),
            AgentsAccess::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsScreenAccess>(r#""robot""#).unwrap(),
            AgentsScreenAccess::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsTurnOrigin>(r#""robot""#).unwrap(),
            AgentsTurnOrigin::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsOriginDetail>(r#""robot""#).unwrap(),
            AgentsOriginDetail::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsActionOutcome>(r#""robot""#).unwrap(),
            AgentsActionOutcome::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsCloseOutcome>(r#""robot""#).unwrap(),
            AgentsCloseOutcome::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsMessageOutcome>(r#""robot""#).unwrap(),
            AgentsMessageOutcome::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsCheckAction>(r#""robot""#).unwrap(),
            AgentsCheckAction::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsReadSource>(r#""robot""#).unwrap(),
            AgentsReadSource::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsReadFormat>(r#""robot""#).unwrap(),
            AgentsReadFormat::Unknown
        );
        assert_eq!(
            serde_json::from_str::<AgentsWho>(r#"{"kind":"robot","id":1}"#).unwrap(),
            AgentsWho::Unknown
        );
    }

    #[test]
    fn the_self_access_and_the_who_tag_have_their_wire_names() {
        assert_eq!(
            serde_json::to_value(AgentsAccess::SelfTab).unwrap(),
            serde_json::json!("self")
        );
        assert_eq!(
            serde_json::to_value(AgentsWho::Agent { name: "a".into() }).unwrap(),
            serde_json::json!({"kind": "agent", "name": "a"})
        );
    }

    #[test]
    fn results_default_their_optional_fields() {
        let actor: AgentsActorInfo =
            serde_json::from_str(r#"{"pane_id":"w1:p1","kind":"agent"}"#).unwrap();
        assert_eq!(actor.turn.origin, AgentsTurnOrigin::Unknown);
        assert!(!actor.turn.user_turn && actor.team.is_none());
        let directory: AgentsDirectory = serde_json::from_str(r#"{"generated_unix":1}"#).unwrap();
        assert!(directory.tabs.is_empty() && directory.recently_closed.is_empty());
        let entry: AgentActionEntry = serde_json::from_str(
            r#"{"unix":1,"id":"a1","actor":"user","action":"close_tab","outcome":"ok"}"#,
        )
        .unwrap();
        assert!(entry.closed_ids.is_empty() && entry.code.is_none());
        let params: AgentsSetMetaParams = serde_json::from_str(r#"{"target":"w1:p1"}"#).unwrap();
        assert_eq!(params.caller_pane, None);
    }
}
