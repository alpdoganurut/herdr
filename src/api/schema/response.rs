use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, AgentTranscriptBackupPass};
use super::browser::{
    BrowserActivity, BrowserActor, BrowserGetInfo, BrowserProfileRecord, BrowserRunResult,
    BrowserSettingsInfo, BrowserStatusInfo,
};
use super::closed_sessions::ClosedSessionInfo;
use super::common::{ClientWindowTitleReason, NotificationShowReason};
use super::events::EventEnvelope;
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::news::{NewsEditionInfo, NewsGetInfo, NewsStatusInfo};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSwapResult, PaneTextPoint, PaneTextRange, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::server::ServerCapabilities;
use super::session::SessionSnapshot;
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;
use super::worktrees::{WorktreeInfo, WorktreeSourceInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuccessResponse {
    pub id: String,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong {
        version: String,
        protocol: u32,
        #[serde(default)]
        capabilities: Option<ServerCapabilities>,
    },
    SessionSnapshot {
        snapshot: Box<SessionSnapshot>,
    },
    WorkspaceInfo {
        workspace: WorkspaceInfo,
    },
    WorkspaceCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    WorkspaceList {
        workspaces: Vec<WorkspaceInfo>,
    },
    WorktreeList {
        source: WorktreeSourceInfo,
        worktrees: Vec<WorktreeInfo>,
    },
    WorktreeCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
    },
    WorktreeOpened {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
        already_open: bool,
    },
    WorktreeRemoved {
        workspace_id: String,
        path: String,
        forced: bool,
    },
    TabInfo {
        tab: TabInfo,
    },
    TabCreated {
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    TabList {
        tabs: Vec<TabInfo>,
    },
    AgentInfo {
        agent: AgentInfo,
    },
    AgentStarted {
        agent: AgentInfo,
        argv: Vec<String>,
    },
    AgentSuspended {
        pane_id: String,
    },
    AgentActivated {
        pane_id: String,
    },
    AgentRestarted {
        pane_id: String,
    },
    AgentTranscripts {
        store_dir: String,
        /// Whether backup passes run (the config flag and session
        /// persistence together).
        enabled: bool,
        sessions: u64,
        native_missing: u64,
        transcript_bytes: u64,
        disk_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_pass: Option<AgentTranscriptBackupPass>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_pass_in_ms: Option<u64>,
    },
    SessionClosedList {
        sessions: Vec<ClosedSessionInfo>,
    },
    NewsStatus {
        status: NewsStatusInfo,
    },
    NewsGet {
        news: NewsGetInfo,
    },
    CoordinatorGet {
        info: super::coordinator::CoordinatorGetInfo,
    },
    NewsHistory {
        editions: Vec<NewsEditionInfo>,
    },
    BrowserRun {
        result: BrowserRunResult,
    },
    BrowserGet {
        browser: BrowserGetInfo,
    },
    BrowserStatus {
        status: BrowserStatusInfo,
    },
    BrowserLog {
        entries: Vec<BrowserActivity>,
    },
    BrowserActor {
        actor: BrowserActor,
    },
    BrowserProfiles {
        profiles: Vec<BrowserProfileRecord>,
    },
    BrowserSettings {
        settings: BrowserSettingsInfo,
    },
    /// `agent.notify` (fork).
    AgentNotify {
        id: String,
        outcome: super::agent_notices::AgentNotifyOutcome,
    },
    /// `agent.notices` and `agent.notice_dismiss` (fork): every card, oldest first.
    AgentNotices {
        notices: Vec<super::agent_notices::AgentNoticeInfo>,
    },
    /// `agent.message_send` (fork): sent now or queued; `status` is the
    /// target's status when it was decided, `reason` why it was queued.
    AgentMessageSend {
        id: String,
        outcome: super::agent_messages::AgentMessageOutcome,
        #[serde(default)]
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// `agent.message_claim` (fork): whether a queued message was taken off
    /// the queue (false: not queued, or not addressed to the claimer).
    AgentMessageClaim {
        claimed: bool,
    },
    /// `agents.settings` and `agents.settings.set` (fork).
    AgentsSettings {
        info: super::agent_wrap::AgentsSettingsInfo,
    },
    /// `agents.fix` (fork): the checks after the fixes ran.
    AgentsFix {
        results: Vec<super::agent_wrap::AgentsCheckInfo>,
    },
    /// `team.list` (fork): every team, in sidebar order.
    TeamList {
        revision: u64,
        #[serde(default)]
        teams: Vec<super::team::TeamInfo>,
    },
    /// `team.get`, `team.make`, `team.disband`, `team.set_purpose`,
    /// `team.set_role`, `team.join`, `team.leave` (fork). `renamed` is set by
    /// `team.set_role` (and a `team.join` with a role): whether the member's
    /// name now follows the role.
    TeamReply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team: Option<super::team::TeamInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        renamed: Option<bool>,
    },
    /// `team.context` (fork): the caller's team and the text to tell it.
    TeamContext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member: Option<super::team::TeamMemberInfo>,
        /// The caller's pane is in a team group, not removed from it, and
        /// not the coordinator's (it joins once its agent is detected).
        #[serde(default)]
        eligible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        team: Option<super::team::TeamInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default)]
        revision: u64,
        /// What `text` comes from (the pane's team, or its pending "no
        /// longer in a team" line), opaque; pass it back as `ack_key`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ack_key: Option<String>,
    },
    NotesGet {
        notes: super::notes::NotesInfo,
    },
    NotesWrite {
        write: super::notes::NotesWriteInfo,
    },
    CheckpointsList {
        checkpoints: super::notes::CheckpointsListInfo,
    },
    CheckpointWrite {
        checkpoint: super::notes::CheckpointWriteInfo,
    },
    CheckpointContext {
        context: super::notes::CheckpointContextInfo,
    },
    // fork: the agents model (agents v2). `agents.notes_append` answers
    // `NotesWrite` and `agents.checkpoint` answers `CheckpointWrite`.
    /// `agents.actor`.
    AgentsActor {
        actor: super::agents_model::AgentsActorInfo,
    },
    /// `agents.directory`.
    AgentsDirectory {
        directory: super::agents_model::AgentsDirectory,
    },
    /// `agents.read`.
    AgentsRead {
        read: super::agents_model::AgentsReadResult,
    },
    /// `agents.open_tab`.
    AgentsOpenTab {
        open: super::agents_model::AgentsOpenResult,
    },
    /// `agents.send_message`.
    AgentsMessage {
        message: super::agents_model::AgentsMessageResult,
    },
    /// `agents.rename_tab`.
    AgentsRenameTab {
        rename: super::agents_model::AgentsRenameResult,
    },
    /// `agents.move_tab`.
    AgentsMoveTab {
        moved: super::agents_model::AgentsMoveResult,
    },
    /// `agents.set_meta`.
    AgentsSetMeta {
        meta: super::agents_model::AgentsSetMetaResult,
    },
    /// `agents.close_tab`.
    AgentsCloseTab {
        close: super::agents_model::AgentsCloseResult,
    },
    /// `agents.reopen_tab`.
    AgentsReopenTab {
        reopen: super::agents_model::AgentsReopenResult,
    },
    /// `agents.actions`: newest first.
    AgentsActions {
        #[serde(default)]
        entries: Vec<super::agents_model::AgentActionEntry>,
    },
    /// `agents.check`: an allowed action (a denial is an error).
    AgentsCheck {
        allowed: bool,
    },
    /// `agents.read_messages`, in the order asked.
    AgentsReadMessages {
        #[serde(default)]
        messages: Vec<super::agents_model::AgentsDeliveredMessage>,
    },
    /// `agents.reorder_group`, `agents.reorder_tab`.
    AgentsReorder {
        reorder: super::agents_model::AgentsReorderResult,
    },
    /// Fork: `agents.queued`, oldest first.
    AgentsQueued {
        #[serde(default)]
        messages: Vec<super::agents_model::AgentsQueuedMessage>,
    },
    AgentPrompted {
        agent: AgentInfo,
    },
    AgentList {
        agents: Vec<AgentInfo>,
    },
    AgentView {
        active: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    PaneInfo {
        pane: PaneInfo,
    },
    PaneList {
        panes: Vec<PaneInfo>,
    },
    PaneCurrent {
        pane: PaneInfo,
    },
    PaneSwap {
        swap: PaneSwapResult,
    },
    PaneMove {
        move_result: PaneMoveResult,
    },
    PaneZoom {
        zoom: PaneZoomResult,
    },
    PaneLayout {
        layout: PaneLayoutSnapshot,
    },
    PaneProcessInfo {
        process_info: PaneProcessInfo,
    },
    LayoutExport {
        layout: LayoutDescription,
    },
    LayoutApply {
        layout: LayoutDescription,
    },
    LayoutSplitRatioSet {
        layout: LayoutDescription,
    },
    PaneNeighbor {
        neighbor: PaneNeighborResult,
    },
    PaneEdges {
        edges: PaneEdgesResult,
    },
    PaneFocusDirection {
        focus: PaneFocusDirectionResult,
    },
    PaneResize {
        resize: PaneResizeResult,
    },
    PaneRead {
        read: PaneReadResult,
    },
    PaneSelection {
        pane_id: String,
        text: String,
    },
    PaneCopyMotion {
        pane_id: String,
        cursor: PaneTextPoint,
        content_revision: u64,
    },
    PaneCopySearch {
        pane_id: String,
        content_revision: u64,
        matches: Vec<PaneTextRange>,
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_global: Option<u64>,
    },
    PaneGraphicsFrameAck {
        sequence: u64,
        revision: u64,
    },
    PaneGraphicsInfo {
        cell_width_px: u32,
        cell_height_px: u32,
        /// True only when this pane is on the currently rendered terminal surface.
        pane_visible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_directory: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        file_frame_formats: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_max_bytes: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_direct_max_bytes: Option<usize>,
        /// Accepts damage metadata while still consuming a complete canonical file.
        #[serde(default)]
        file_frame_damage: bool,
        #[serde(default)]
        max_layers_per_pane: usize,
        #[serde(default)]
        pixel_mouse: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_frame_transport: Option<String>,
    },
    AgentExplain {
        explain: serde_json::Value,
    },
    SubscriptionStarted {},
    WaitMatched {
        event: EventEnvelope,
    },
    OutputMatched {
        pane_id: String,
        revision: u64,
        matched_line: Option<String>,
        read: PaneReadResult,
    },
    NotificationShow {
        shown: bool,
        reason: NotificationShowReason,
    },
    ClientWindowTitle {
        changed: bool,
        reason: ClientWindowTitleReason,
    },
    IntegrationList {
        integrations: Vec<super::integrations::IntegrationInfo>,
    },
    IntegrationInstall {
        target: IntegrationTarget,
        details: IntegrationInstallResult,
    },
    IntegrationUninstall {
        target: IntegrationTarget,
        details: IntegrationUninstallResult,
    },
    AgentManifestReload {
        manifests: Vec<AgentManifestInfo>,
    },
    AgentManifestStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_check_unix: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_result: Option<String>,
        manifests: Vec<AgentManifestInfo>,
    },
    PluginLinked {
        plugin: InstalledPluginInfo,
    },
    PluginList {
        plugins: Vec<InstalledPluginInfo>,
    },
    PluginUnlinked {
        plugin_id: String,
        removed: bool,
    },
    PluginEnabled {
        plugin: InstalledPluginInfo,
    },
    PluginDisabled {
        plugin: InstalledPluginInfo,
    },
    PluginActionList {
        actions: Vec<PluginActionInfo>,
    },
    PluginActionInvoked {
        action: PluginActionInfo,
        context: PluginInvocationContext,
        log: PluginCommandLogInfo,
    },
    PaneLinkResolved {
        regions: Vec<super::panes::PaneLinkRegion>,
    },
    PaneLinkActivated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        handled: bool,
    },
    PluginLogList {
        logs: Vec<PluginCommandLogInfo>,
    },
    PluginPaneOpened {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneFocused {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneClosed {
        pane_id: String,
    },
    ConfigReload {
        status: crate::config::ConfigReloadStatus,
        diagnostics: Vec<String>,
    },
    /// Acknowledgement for the client-shell surface interest lease. This method is new on the
    /// endpoint protocol, so its revision-bearing result can establish an activation floor.
    ClientShellSurfaceSet {
        active: bool,
        projection_revision: u64,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentManifestInfo {
    pub agent: String,
    pub source: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_remote_version: Option<String>,
    pub local_override_shadowing_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_last_checked_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
