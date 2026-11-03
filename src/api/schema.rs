use serde::{Deserialize, Serialize};

pub mod agent_messages;
pub mod agent_notices;
pub mod agent_wrap;
pub mod agents;
pub mod agents_model;
pub mod browser;
pub mod browser_placement;
pub mod closed_sessions;
pub mod commands;
pub mod common;
pub mod coordinator;
pub mod events;
pub mod integrations;
pub mod news;
pub mod notes;
pub(crate) mod operations;
pub mod panes;
pub mod plugins;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod team;
pub mod workspaces;
pub mod worktrees;

// fork: named re-exports (each module also has `method` / `error_code`).
pub use agent_messages::{AgentMessageClaimParams, AgentMessageOutcome, AgentMessageSendParams};
pub use agent_notices::{
    AgentNoticeDismissParams, AgentNoticeInfo, AgentNoticeKind, AgentNotifyOutcome,
    AgentNotifyParams,
};
pub use agent_wrap::{
    AgentsCheckInfo, AgentsCheckState, AgentsFixParams, AgentsHandoffInfo, AgentsSettingsInfo,
    AgentsSettingsSetParams, AgentsWrapSource,
};
pub use agents::*;
pub use browser::*;
pub use browser_placement::{
    BrowserCellRect, BrowserGridSize, BrowserPlacementInfo, BrowserPlacementKind,
    BrowserPlacementReportParams, BrowserPlacementSetParams, BrowserPlacementSpec,
    BrowserPlacementStatus, BrowserPlacementsParams, BrowserPxSize,
};
pub use closed_sessions::*;
pub use commands::*;
pub use common::*;
pub use events::*;
pub use integrations::*;
pub use news::*;
pub use panes::*;
pub use plugins::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use tabs::*;
pub use team::{
    TeamActor, TeamContextParams, TeamGetParams, TeamInfo, TeamJoinParams, TeamMakeParams,
    TeamMemberInfo, TeamPaneParams, TeamSetPurposeParams, TeamSetRoleParams, TeamWorkspaceParams,
};
pub use workspaces::*;
pub use worktrees::*;

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

// Variant order and parameter types preserve the existing wire contract.
// The two flags are independent: presentation impact, then client-shell admission.
operations::define_operations! {
    Ping(PingParams) => "ping", false, false;
    ServerStop(EmptyParams) => "server.stop", false, false;
    ServerLiveHandoff(ServerLiveHandoffParams) => "server.live_handoff", false, false;
    ServerReloadConfig(EmptyParams) => "server.reload_config", true, true;
    ServerSshAgentRegister(ServerSshAgentRegisterParams) => "server.ssh_agent.register", false, false;
    ServerAgentManifests(EmptyParams) => "server.agent_manifests", false, false;
    ServerReloadAgentManifests(EmptyParams) => "server.reload_agent_manifests", true, false;
    NotificationShow(NotificationShowParams) => "notification.show", true, false;
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams) => "product_announcement.dismiss", true, true;
    ReleaseNotesDismiss(ReleaseNotesDismissParams) => "release_notes.dismiss", true, true;
    CommandInvoke(CommandInvokeParams) => "command.invoke", true, true;
    ClientWindowTitleSet(ClientWindowTitleSetParams) => "client.window_title.set", false, false;
    ClientWindowTitleClear(EmptyParams) => "client.window_title.clear", false, false;
    ClientShellSurfaceSet(ClientShellSurfaceSetParams) => "client_shell.surface.set", false, true;
    SessionSnapshot(EmptyParams) => "session.snapshot", false, false;
    WorkspaceCreate(WorkspaceCreateParams) => "workspace.create", true, true;
    WorkspaceList(EmptyParams) => "workspace.list", false, false;
    WorkspaceGet(WorkspaceTarget) => "workspace.get", false, false;
    WorkspaceFocus(WorkspaceTarget) => "workspace.focus", true, true;
    WorkspaceRename(WorkspaceRenameParams) => "workspace.rename", true, true;
    WorkspaceMove(WorkspaceMoveParams) => "workspace.move", true, true;
    WorkspaceMoveBlock(WorkspaceMoveBlockParams) => "workspace.move_block", true, true;
    WorkspaceReportMetadata(WorkspaceReportMetadataParams) => "workspace.report_metadata", true, false;
    WorkspaceClose(WorkspaceCloseParams) => "workspace.close", true, true;
    WorktreeList(WorktreeListParams) => "worktree.list", false, true;
    WorktreeCreate(WorktreeCreateParams) => "worktree.create", true, true;
    WorktreeOpen(WorktreeOpenParams) => "worktree.open", true, true;
    WorktreeRemove(WorktreeRemoveParams) => "worktree.remove", true, true;
    TabCreate(TabCreateParams) => "tab.create", true, true;
    TabList(TabListParams) => "tab.list", false, false;
    TabGet(TabTarget) => "tab.get", false, false;
    TabFocus(TabTarget) => "tab.focus", true, true;
    TabRename(TabRenameParams) => "tab.rename", true, true;
    TabMove(TabMoveParams) => "tab.move", true, true;
    TabClose(TabTarget) => "tab.close", true, true;
    AgentList(EmptyParams) => "agent.list", false, false;
    AgentGet(AgentTarget) => "agent.get", false, false;
    AgentRead(AgentReadParams) => "agent.read", false, false;
    AgentExplain(AgentTarget) => "agent.explain", false, false;
    AgentSendKeys(AgentSendKeysParams) => "agent.send_keys", true, false;
    AgentRename(AgentRenameParams) => "agent.rename", true, false;
    AgentViewSet(AgentViewSetParams) => "agent.view.set", true, false;
    AgentViewClear(AgentViewClearParams) => "agent.view.clear", true, false;
    AgentFocus(AgentTarget) => "agent.focus", true, false;
    AgentStart(AgentStartParams) => "agent.start", true, false;
    AgentSuspend(AgentSuspendParams) => "agent.suspend", true, true;
    AgentActivate(AgentActivateParams) => "agent.activate", true, true;
    AgentRestart(AgentRestartParams) => "agent.restart", true, true;
    /// Fork (force suspend): `agent.suspend` past the blocked, working,
    /// subagents and background-task refusals; the user's own, from the
    /// client shell only.
    AgentSuspendForce(AgentSuspendParams) => "agent.suspend_force", true, true;
    /// Fork (force suspend): `agent.restart` as a forced suspend; the user's
    /// own, from the client shell only.
    AgentRestartForce(AgentRestartParams) => "agent.restart_force", true, true;
    AgentTranscripts(EmptyParams) => "agent.transcripts", false, true;
    AgentNotify(agent_notices::AgentNotifyParams) => "agent.notify", false, false;
    AgentNotices(EmptyParams) => "agent.notices", false, false;
    AgentNoticeDismiss(agent_notices::AgentNoticeDismissParams) => "agent.notice_dismiss", true, true;
    AgentMessageSend(agent_messages::AgentMessageSendParams) => "agent.message_send", true, false;
    AgentMessageClaim(agent_messages::AgentMessageClaimParams) => "agent.message_claim", true, false;
    AgentsSettings(EmptyParams) => "agents.settings", false, true;
    AgentsSettingsSet(agent_wrap::AgentsSettingsSetParams) => "agents.settings.set", false, true;
    AgentsFix(agent_wrap::AgentsFixParams) => "agents.fix", false, true;
    TeamList(EmptyParams) => "team.list", false, false;
    TeamGet(team::TeamGetParams) => "team.get", false, true;
    TeamMake(team::TeamMakeParams) => "team.make", true, true;
    TeamDisband(team::TeamWorkspaceParams) => "team.disband", true, true;
    TeamSetPurpose(team::TeamSetPurposeParams) => "team.set_purpose", true, true;
    TeamSetRole(team::TeamSetRoleParams) => "team.set_role", true, true;
    TeamJoin(team::TeamJoinParams) => "team.join", true, true;
    TeamLeave(team::TeamPaneParams) => "team.leave", true, true;
    TeamContext(team::TeamContextParams) => "team.context", false, false;
    TabSetColor(TabSetColorParams) => "tab.set_color", true, true;
    TabSetRemind(TabSetRemindParams) => "tab.set_remind", true, true;
    TabSetReminder(TabSetReminderParams) => "tab.set_reminder", true, true;
    /// Fork (sidebar v3): pin or unpin a tab.
    TabSetPinned(TabSetPinnedParams) => "tab.set_pinned", true, true;
    /// Fork: mute or unmute a tab's notifications.
    TabSetMuted(TabSetMutedParams) => "tab.set_muted", true, true;
    /// Fork: turn a tab's do-not-disturb on or off.
    TabSetDnd(TabSetDndParams) => "tab.set_dnd", true, true;
    PaneReportSubagent(PaneReportSubagentParams) => "pane.report_subagent", true, false;
    SessionClosedList(EmptyParams) => "session.closed_list", false, true;
    SessionClosedReopen(ClosedSessionTarget) => "session.closed_reopen", true, true;
    SessionClosedRemove(ClosedSessionTarget) => "session.closed_remove", false, true;
    NewsRun(EmptyParams) => "news.run", true, true;
    NewsStatus(EmptyParams) => "news.status", false, false;
    NewsGet(EmptyParams) => "news.get", false, true;
    NewsHistory(NewsHistoryParams) => "news.history", false, false;
    NewsOpen(NewsOpenParams) => "news.open", true, true;
    NewsSetEnabled(NewsSetEnabledParams) => "news.set_enabled", false, true;
    NewsSetTimes(NewsSetTimesParams) => "news.set_times", false, true;
    NewsSetQuietHours(NewsSetQuietHoursParams) => "news.set_quiet_hours", false, true;
    CoordinatorGet(EmptyParams) => "coordinator.get", false, true;
    CoordinatorOpen(EmptyParams) => "coordinator.open", true, true;
    CoordinatorOpenDashboard(coordinator::CoordinatorOpenDashboardParams) => "coordinator.open_dashboard", false, true;
    CoordinatorWake(coordinator::CoordinatorWakeParams) => "coordinator.wake", true, true;
    CoordinatorStart(coordinator::CoordinatorStartParams) => "coordinator.start", true, true;
    CoordinatorSetEnabled(coordinator::CoordinatorSetEnabledParams) => "coordinator.set_enabled", false, true;
    CoordinatorSetWakeCaps(coordinator::CoordinatorSetWakeCapsParams) => "coordinator.set_wake_caps", false, true;
    CoordinatorSetModel(coordinator::CoordinatorSetModelParams) => "coordinator.set_model", false, true;
    CoordinatorSetNotify(coordinator::CoordinatorSetNotifyParams) => "coordinator.set_notify", false, true;
    BrowserRun(BrowserRunParams) => "browser.run", false, false;
    BrowserGet(BrowserGetParams) => "browser.get", false, true;
    BrowserStatus(EmptyParams) => "browser.status", false, false;
    BrowserFocus(BrowserTabTarget) => "browser.focus", false, true;
    BrowserStart(BrowserProfileTarget) => "browser.start", false, true;
    BrowserStop(BrowserStopParams) => "browser.stop", false, true;
    BrowserLog(BrowserLogParams) => "browser.log", false, false;
    BrowserResolveCaller(BrowserCaller) => "browser.resolve_caller", false, false;
    BrowserProfiles(EmptyParams) => "browser.profiles", false, false;
    BrowserProfileCreate(BrowserProfileCreateParams) => "browser.profile_create", false, false;
    BrowserProfileDelete(BrowserProfileName) => "browser.profile_delete", false, false;
    BrowserSettings(EmptyParams) => "browser.settings", false, true;
    BrowserSettingsSet(BrowserSettingsSetParams) => "browser.settings.set", false, true;
    BrowserFix(BrowserFixParams) => "browser.fix", false, true;
    NotesGet(notes::NotesGetParams) => "notes.get", false, true;
    NotesSet(notes::NotesSetParams) => "notes.set", false, true;
    NotesAppend(notes::NotesAppendParams) => "notes.append", false, true;
    CheckpointsList(notes::CheckpointsListParams) => "checkpoints.list", false, true;
    CheckpointsAdd(notes::CheckpointsAddParams) => "checkpoints.add", false, true;
    CheckpointsUpdate(notes::CheckpointsUpdateParams) => "checkpoints.update", false, true;
    CheckpointsRemove(notes::CheckpointTarget) => "checkpoints.remove", false, true;
    CheckpointsContext(notes::CheckpointsContextParams) => "checkpoints.context", false, true;
    /// Fork: show a tab's notes in the attached clients' info panes (an
    /// agent's `agents_notes_write reveal=true`).
    NotesReveal(notes::NotesRevealParams) => "notes.reveal", true, false;
    // fork: the agents model (agents v2).
    AgentsActor(agents_model::AgentsActorParams) => "agents.actor", false, false;
    AgentsDirectory(agents_model::AgentsDirectoryParams) => "agents.directory", false, false;
    AgentsRead(agents_model::AgentsReadParams) => "agents.read", false, false;
    AgentsOpenTab(agents_model::AgentsOpenTabParams) => "agents.open_tab", true, false;
    AgentsSendMessage(agents_model::AgentsSendMessageParams) => "agents.send_message", true, false;
    AgentsRenameTab(agents_model::AgentsRenameTabParams) => "agents.rename_tab", true, false;
    AgentsMoveTab(agents_model::AgentsMoveTabParams) => "agents.move_tab", true, false;
    AgentsSetMeta(agents_model::AgentsSetMetaParams) => "agents.set_meta", true, true;
    AgentsCloseTab(agents_model::AgentsCloseTabParams) => "agents.close_tab", true, false;
    AgentsReopenTab(agents_model::AgentsReopenTabParams) => "agents.reopen_tab", true, false;
    AgentsNotesAppend(agents_model::AgentsNotesAppendParams) => "agents.notes_append", false, false;
    AgentsCheckpoint(agents_model::AgentsCheckpointParams) => "agents.checkpoint", false, false;
    AgentsActions(agents_model::AgentsActionsParams) => "agents.actions", false, false;
    AgentsCheck(agents_model::AgentsCheckParams) => "agents.check", false, false;
    AgentsSuspend(agents_model::AgentsLifecycleParams) => "agents.suspend", true, false;
    AgentsActivate(agents_model::AgentsLifecycleParams) => "agents.activate", true, false;
    AgentsRestart(agents_model::AgentsLifecycleParams) => "agents.restart", true, false;
    AgentsReadMessages(agents_model::AgentsReadMessagesParams) => "agents.read_messages", false, false;
    AgentsReorderGroup(agents_model::AgentsReorderGroupParams) => "agents.reorder_group", true, false;
    AgentsReorderTab(agents_model::AgentsReorderTabParams) => "agents.reorder_tab", true, false;
    /// Fork: a turn of the pane's agent started or ended (its hooks).
    PaneReportTurn(PaneReportTurnParams) => "pane.report_turn", false, false;
    /// Fork: the caller's own messages still queued.
    AgentsQueued(agents_model::AgentsQueuedParams) => "agents.queued", false, false;
    /// Fork: the messages to the caller still queued.
    AgentsInbox(agents_model::AgentsInboxParams) => "agents.inbox", false, false;
    /// Fork: handoff, then compact, for a Claude agent.
    AgentsHandoff(agents_model::AgentsHandoffParams) => "agents.handoff", false, true;
    /// Fork: a compaction of the pane's agent starts or started over (its
    /// PreCompact / SessionStart(compact) hooks).
    PaneReportCompact(PaneReportCompactParams) => "pane.report_compact", false, false;
    /// Fork: a wait of the caller started or finished (the herdr_agents
    /// MCP), for its turn origin.
    AgentsReportWait(agents_model::AgentsReportWaitParams) => "agents.report_wait", false, false;
    /// Fork: take the caller's quiet messages (its Claude hook).
    AgentsTakeQuiet(agents_model::AgentsTakeQuietParams) => "agents.take_quiet", false, false;
    /// Fork: the caller's hook printed the quiet messages it took.
    AgentsAckQuiet(agents_model::AgentsAckQuietParams) => "agents.ack_quiet", false, false;
    /// Fork: mute and do-not-disturb on an agent's tab, checked as the
    /// caller (the herdr_agents agents_mute / agents_dnd tools, `herdr tab
    /// mute|unmute|dnd`).
    AgentsSetQuiet(agents_model::AgentsSetQuietParams) => "agents.set_quiet", true, false;
    /// Fork (companion browser): the browser host role. register /
    /// heartbeat / unregister run on the connection thread
    /// (src/browser/serve.rs); the rest on the App lane
    /// (src/app/browser_host.rs). None is client-shell.
    BrowserHostRegister(BrowserHostRegisterParams) => "browser.host_register", false, false;
    BrowserHostHeartbeat(BrowserHostLeaseTarget) => "browser.host_heartbeat", false, false;
    BrowserHostUnregister(BrowserHostLeaseTarget) => "browser.host_unregister", false, false;
    BrowserHostState(BrowserHostStateParams) => "browser.host_state", false, false;
    BrowserHostSync(BrowserHostSyncParams) => "browser.host_sync", false, false;
    BrowserHostReport(BrowserHostReportParams) => "browser.host_report", false, false;
    BrowserSendPage(BrowserSendPageParams) => "browser.send_page", false, false;
    /// Fork (companion launcher): the companion app herdr runs — status,
    /// start, stop, restart, focus (App lane; the Browser row and overlay).
    BrowserCompanion(BrowserCompanionParams) => "browser.companion", false, true;
    /// Fork (companion launcher): the browser placement of an attached
    /// client (client shell; the headless server's endpoint handler stores
    /// it, never the App), every client's placement for the browser host (a
    /// long poll) and the host's report on one (both on the connection
    /// thread, src/browser/serve.rs).
    BrowserPlacementSet(BrowserPlacementSetParams) => "browser.placement.set", false, true;
    BrowserPlacements(BrowserPlacementsParams) => "browser.placements", false, false;
    BrowserPlacementReport(BrowserPlacementReportParams) => "browser.placement_report", false, false;
    AgentPrompt(AgentPromptParams) => "agent.prompt", true, false;
    AgentWait(AgentWaitParams) => "agent.wait", false, false;
    PaneSplit(PaneSplitParams) => "pane.split", true, true;
    PaneSwap(PaneSwapParams) => "pane.swap", true, true;
    PaneMove(PaneMoveParams) => "pane.move", true, true;
    PaneZoom(PaneZoomParams) => "pane.zoom", true, true;
    PaneLayout(PaneLayoutParams) => "pane.layout", false, false;
    PaneProcessInfo(PaneProcessInfoParams) => "pane.process_info", false, false;
    LayoutExport(LayoutExportParams) => "layout.export", false, false;
    LayoutApply(LayoutApplyParams) => "layout.apply", true, false;
    LayoutSetSplitRatio(LayoutSetSplitRatioParams) => "layout.set_split_ratio", true, true;
    PaneNeighbor(PaneNeighborParams) => "pane.neighbor", false, false;
    PaneEdges(PaneEdgesParams) => "pane.edges", false, false;
    PaneFocusDirection(PaneFocusDirectionParams) => "pane.focus_direction", true, true;
    PaneResize(PaneResizeParams) => "pane.resize", true, true;
    PaneScroll(PaneScrollParams) => "pane.scroll", true, true;
    PaneClear(PaneTarget) => "pane.clear", true, true;
    PaneEditScrollback(PaneTarget) => "pane.edit_scrollback", true, true;
    PaneSelectionRead(PaneSelectionReadParams) => "pane.selection.read", false, true;
    PaneCopyMotion(PaneCopyMotionParams) => "pane.copy_motion", false, true;
    PaneCopySearch(PaneCopySearchParams) => "pane.copy_search", false, true;
    PaneList(PaneListParams) => "pane.list", false, false;
    PaneCurrent(PaneCurrentParams) => "pane.current", false, false;
    PaneGet(PaneTarget) => "pane.get", false, true;
    PaneFocus(PaneTarget) => "pane.focus", true, true;
    PaneInputSet(PaneInputSetParams) => "pane.input.set", true, true;
    PaneLinkActivate(PaneLinkActivateParams) => "pane.link.activate", false, true;
    PaneLinkResolve(PaneLinkActivateParams) => "pane.link.resolve", false, true;
    PaneRename(PaneRenameParams) => "pane.rename", true, true;
    PaneSendText(PaneSendTextParams) => "pane.send_text", false, false;
    PaneSendKeys(PaneSendKeysParams) => "pane.send_keys", false, false;
    PaneSendInput(PaneSendInputParams) => "pane.send_input", false, false;
    PaneRead(PaneReadParams) => "pane.read", false, false;
    PaneReportAgent(PaneReportAgentParams) => "pane.report_agent", true, false;
    PaneReportAgentSession(PaneReportAgentSessionParams) => "pane.report_agent_session", true, false;
    PaneReportMetadata(PaneReportMetadataParams) => "pane.report_metadata", true, false;
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams) => "pane.clear_agent_authority", true, false;
    PaneReleaseAgent(PaneReleaseAgentParams) => "pane.release_agent", true, false;
    PaneClose(PaneTarget) => "pane.close", true, true;
    PopupClose(EmptyParams) => "popup.close", true, false;
    EventsSubscribe(EventsSubscribeParams) => "events.subscribe", false, false;
    EventsWait(EventsWaitParams) => "events.wait", false, false;
    PaneWaitForOutput(PaneWaitForOutputParams) => "pane.wait_for_output", false, false;
    IntegrationList(EmptyParams) => "integration.list", false, true;
    IntegrationInstall(IntegrationInstallParams) => "integration.install", false, true;
    IntegrationUninstall(IntegrationUninstallParams) => "integration.uninstall", false, false;
    PluginLink(PluginLinkParams) => "plugin.link", false, false;
    PluginList(PluginListParams) => "plugin.list", false, false;
    PluginUnlink(PluginUnlinkParams) => "plugin.unlink", true, false;
    PluginEnable(PluginSetEnabledParams) => "plugin.enable", false, false;
    PluginDisable(PluginSetEnabledParams) => "plugin.disable", true, false;
    PluginActionList(PluginActionListParams) => "plugin.action.list", false, false;
    PluginActionInvoke(PluginActionInvokeParams) => "plugin.action.invoke", true, false;
    PluginLogList(PluginLogListParams) => "plugin.log.list", false, false;
    PluginPaneOpen(PluginPaneOpenParams) => "plugin.pane.open", true, false;
    PluginPaneFocus(PluginPaneFocusParams) => "plugin.pane.focus", true, false;
    PluginPaneClose(PluginPaneCloseParams) => "plugin.pane.close", true, false;
}

#[cfg(test)]
mod tests;
