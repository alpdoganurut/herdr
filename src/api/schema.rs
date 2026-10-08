use serde::{Deserialize, Serialize};

pub mod agent_messages;
pub mod agent_notices;
pub mod agent_wrap;
pub mod agents;
pub mod agents_model;
pub mod browser;
pub mod closed_sessions;
pub mod commands;
pub mod common;
pub mod coordinator;
pub mod events;
pub mod integrations;
pub mod news;
pub mod notes;
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
    AgentsCheckInfo, AgentsCheckState, AgentsFixParams, AgentsSettingsInfo,
    AgentsSettingsSetParams, AgentsWrapSource,
};
pub use agents::*;
pub use browser::*;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params")]
// Request enums are short-lived wire values; keeping variants direct preserves
// the simple serde shape and avoids boxing churn across every caller.
#[allow(clippy::large_enum_variant)]
pub enum Method {
    #[serde(rename = "ping")]
    Ping(PingParams),
    #[serde(rename = "server.stop")]
    ServerStop(EmptyParams),
    #[serde(rename = "server.live_handoff")]
    ServerLiveHandoff(ServerLiveHandoffParams),
    #[serde(rename = "server.reload_config")]
    ServerReloadConfig(EmptyParams),
    #[serde(rename = "server.ssh_agent.register")]
    ServerSshAgentRegister(ServerSshAgentRegisterParams),
    #[serde(rename = "server.agent_manifests")]
    ServerAgentManifests(EmptyParams),
    #[serde(rename = "server.reload_agent_manifests")]
    ServerReloadAgentManifests(EmptyParams),
    #[serde(rename = "notification.show")]
    NotificationShow(NotificationShowParams),
    #[serde(rename = "product_announcement.dismiss")]
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams),
    #[serde(rename = "release_notes.dismiss")]
    ReleaseNotesDismiss(ReleaseNotesDismissParams),
    #[serde(rename = "command.invoke")]
    CommandInvoke(CommandInvokeParams),
    #[serde(rename = "client.window_title.set")]
    ClientWindowTitleSet(ClientWindowTitleSetParams),
    #[serde(rename = "client.window_title.clear")]
    ClientWindowTitleClear(EmptyParams),
    #[serde(rename = "client_shell.surface.set")]
    ClientShellSurfaceSet(ClientShellSurfaceSetParams),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(EmptyParams),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreateParams),
    #[serde(rename = "workspace.list")]
    WorkspaceList(EmptyParams),
    #[serde(rename = "workspace.get")]
    WorkspaceGet(WorkspaceTarget),
    #[serde(rename = "workspace.focus")]
    WorkspaceFocus(WorkspaceTarget),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRenameParams),
    #[serde(rename = "workspace.move")]
    WorkspaceMove(WorkspaceMoveParams),
    #[serde(rename = "workspace.move_block")]
    WorkspaceMoveBlock(WorkspaceMoveBlockParams),
    #[serde(rename = "workspace.report_metadata")]
    WorkspaceReportMetadata(WorkspaceReportMetadataParams),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceCloseParams),
    #[serde(rename = "worktree.list")]
    WorktreeList(WorktreeListParams),
    #[serde(rename = "worktree.create")]
    WorktreeCreate(WorktreeCreateParams),
    #[serde(rename = "worktree.open")]
    WorktreeOpen(WorktreeOpenParams),
    #[serde(rename = "worktree.remove")]
    WorktreeRemove(WorktreeRemoveParams),
    #[serde(rename = "tab.create")]
    TabCreate(TabCreateParams),
    #[serde(rename = "tab.list")]
    TabList(TabListParams),
    #[serde(rename = "tab.get")]
    TabGet(TabTarget),
    #[serde(rename = "tab.focus")]
    TabFocus(TabTarget),
    #[serde(rename = "tab.rename")]
    TabRename(TabRenameParams),
    #[serde(rename = "tab.move")]
    TabMove(TabMoveParams),
    #[serde(rename = "tab.close")]
    TabClose(TabTarget),
    #[serde(rename = "agent.list")]
    AgentList(EmptyParams),
    #[serde(rename = "agent.get")]
    AgentGet(AgentTarget),
    #[serde(rename = "agent.read")]
    AgentRead(AgentReadParams),
    #[serde(rename = "agent.explain")]
    AgentExplain(AgentTarget),
    #[serde(rename = "agent.send_keys")]
    AgentSendKeys(AgentSendKeysParams),
    #[serde(rename = "agent.rename")]
    AgentRename(AgentRenameParams),
    #[serde(rename = "agent.view.set")]
    AgentViewSet(AgentViewSetParams),
    #[serde(rename = "agent.view.clear")]
    AgentViewClear(AgentViewClearParams),
    #[serde(rename = "agent.focus")]
    AgentFocus(AgentTarget),
    #[serde(rename = "agent.start")]
    AgentStart(AgentStartParams),
    #[serde(rename = "agent.suspend")]
    AgentSuspend(AgentSuspendParams),
    #[serde(rename = "agent.activate")]
    AgentActivate(AgentActivateParams),
    #[serde(rename = "agent.restart")]
    AgentRestart(AgentRestartParams),
    #[serde(rename = "agent.transcripts")]
    AgentTranscripts(EmptyParams),
    #[serde(rename = "agent.notify")]
    AgentNotify(agent_notices::AgentNotifyParams),
    #[serde(rename = "agent.notices")]
    AgentNotices(EmptyParams),
    #[serde(rename = "agent.notice_dismiss")]
    AgentNoticeDismiss(agent_notices::AgentNoticeDismissParams),
    #[serde(rename = "agent.message_send")]
    AgentMessageSend(agent_messages::AgentMessageSendParams),
    #[serde(rename = "agent.message_claim")]
    AgentMessageClaim(agent_messages::AgentMessageClaimParams),
    #[serde(rename = "agents.settings")]
    AgentsSettings(EmptyParams),
    #[serde(rename = "agents.settings.set")]
    AgentsSettingsSet(agent_wrap::AgentsSettingsSetParams),
    #[serde(rename = "agents.fix")]
    AgentsFix(agent_wrap::AgentsFixParams),
    #[serde(rename = "team.list")]
    TeamList(EmptyParams),
    #[serde(rename = "team.get")]
    TeamGet(team::TeamGetParams),
    #[serde(rename = "team.make")]
    TeamMake(team::TeamMakeParams),
    #[serde(rename = "team.disband")]
    TeamDisband(team::TeamWorkspaceParams),
    #[serde(rename = "team.set_purpose")]
    TeamSetPurpose(team::TeamSetPurposeParams),
    #[serde(rename = "team.set_role")]
    TeamSetRole(team::TeamSetRoleParams),
    #[serde(rename = "team.join")]
    TeamJoin(team::TeamJoinParams),
    #[serde(rename = "team.leave")]
    TeamLeave(team::TeamPaneParams),
    #[serde(rename = "team.context")]
    TeamContext(team::TeamContextParams),
    #[serde(rename = "tab.set_color")]
    TabSetColor(TabSetColorParams),
    #[serde(rename = "tab.set_remind")]
    TabSetRemind(TabSetRemindParams),
    #[serde(rename = "tab.set_reminder")]
    TabSetReminder(TabSetReminderParams),
    /// Fork (sidebar v3): pin or unpin a tab.
    #[serde(rename = "tab.set_pinned")]
    TabSetPinned(TabSetPinnedParams),
    /// Fork: mute or unmute a tab's notifications.
    #[serde(rename = "tab.set_muted")]
    TabSetMuted(TabSetMutedParams),
    #[serde(rename = "pane.report_subagent")]
    PaneReportSubagent(PaneReportSubagentParams),
    #[serde(rename = "session.closed_list")]
    SessionClosedList(EmptyParams),
    #[serde(rename = "session.closed_reopen")]
    SessionClosedReopen(ClosedSessionTarget),
    #[serde(rename = "session.closed_remove")]
    SessionClosedRemove(ClosedSessionTarget),
    #[serde(rename = "news.run")]
    NewsRun(EmptyParams),
    #[serde(rename = "news.status")]
    NewsStatus(EmptyParams),
    #[serde(rename = "news.get")]
    NewsGet(EmptyParams),
    #[serde(rename = "news.history")]
    NewsHistory(NewsHistoryParams),
    #[serde(rename = "news.open")]
    NewsOpen(NewsOpenParams),
    #[serde(rename = "news.set_enabled")]
    NewsSetEnabled(NewsSetEnabledParams),
    #[serde(rename = "news.set_times")]
    NewsSetTimes(NewsSetTimesParams),
    #[serde(rename = "news.set_quiet_hours")]
    NewsSetQuietHours(NewsSetQuietHoursParams),
    #[serde(rename = "coordinator.get")]
    CoordinatorGet(EmptyParams),
    #[serde(rename = "coordinator.open")]
    CoordinatorOpen(EmptyParams),
    #[serde(rename = "coordinator.open_dashboard")]
    CoordinatorOpenDashboard(coordinator::CoordinatorOpenDashboardParams),
    #[serde(rename = "coordinator.wake")]
    CoordinatorWake(coordinator::CoordinatorWakeParams),
    #[serde(rename = "coordinator.start")]
    CoordinatorStart(coordinator::CoordinatorStartParams),
    #[serde(rename = "coordinator.set_enabled")]
    CoordinatorSetEnabled(coordinator::CoordinatorSetEnabledParams),
    #[serde(rename = "coordinator.set_wake_caps")]
    CoordinatorSetWakeCaps(coordinator::CoordinatorSetWakeCapsParams),
    #[serde(rename = "coordinator.set_model")]
    CoordinatorSetModel(coordinator::CoordinatorSetModelParams),
    #[serde(rename = "coordinator.set_notify")]
    CoordinatorSetNotify(coordinator::CoordinatorSetNotifyParams),
    #[serde(rename = "browser.run")]
    BrowserRun(BrowserRunParams),
    #[serde(rename = "browser.get")]
    BrowserGet(BrowserGetParams),
    #[serde(rename = "browser.status")]
    BrowserStatus(EmptyParams),
    #[serde(rename = "browser.focus")]
    BrowserFocus(BrowserTabTarget),
    #[serde(rename = "browser.start")]
    BrowserStart(BrowserProfileTarget),
    #[serde(rename = "browser.stop")]
    BrowserStop(BrowserStopParams),
    #[serde(rename = "browser.log")]
    BrowserLog(BrowserLogParams),
    #[serde(rename = "browser.resolve_caller")]
    BrowserResolveCaller(BrowserCaller),
    #[serde(rename = "browser.profiles")]
    BrowserProfiles(EmptyParams),
    #[serde(rename = "browser.profile_create")]
    BrowserProfileCreate(BrowserProfileCreateParams),
    #[serde(rename = "browser.profile_delete")]
    BrowserProfileDelete(BrowserProfileName),
    #[serde(rename = "browser.settings")]
    BrowserSettings(EmptyParams),
    #[serde(rename = "browser.settings.set")]
    BrowserSettingsSet(BrowserSettingsSetParams),
    #[serde(rename = "browser.fix")]
    BrowserFix(BrowserFixParams),
    #[serde(rename = "notes.get")]
    NotesGet(notes::NotesGetParams),
    #[serde(rename = "notes.set")]
    NotesSet(notes::NotesSetParams),
    #[serde(rename = "notes.append")]
    NotesAppend(notes::NotesAppendParams),
    #[serde(rename = "checkpoints.list")]
    CheckpointsList(notes::CheckpointsListParams),
    #[serde(rename = "checkpoints.add")]
    CheckpointsAdd(notes::CheckpointsAddParams),
    #[serde(rename = "checkpoints.update")]
    CheckpointsUpdate(notes::CheckpointsUpdateParams),
    #[serde(rename = "checkpoints.remove")]
    CheckpointsRemove(notes::CheckpointTarget),
    #[serde(rename = "checkpoints.context")]
    CheckpointsContext(notes::CheckpointsContextParams),
    // fork: the agents model (agents v2).
    #[serde(rename = "agents.actor")]
    AgentsActor(agents_model::AgentsActorParams),
    #[serde(rename = "agents.directory")]
    AgentsDirectory(agents_model::AgentsDirectoryParams),
    #[serde(rename = "agents.read")]
    AgentsRead(agents_model::AgentsReadParams),
    #[serde(rename = "agents.open_tab")]
    AgentsOpenTab(agents_model::AgentsOpenTabParams),
    #[serde(rename = "agents.send_message")]
    AgentsSendMessage(agents_model::AgentsSendMessageParams),
    #[serde(rename = "agents.rename_tab")]
    AgentsRenameTab(agents_model::AgentsRenameTabParams),
    #[serde(rename = "agents.move_tab")]
    AgentsMoveTab(agents_model::AgentsMoveTabParams),
    #[serde(rename = "agents.set_meta")]
    AgentsSetMeta(agents_model::AgentsSetMetaParams),
    #[serde(rename = "agents.close_tab")]
    AgentsCloseTab(agents_model::AgentsCloseTabParams),
    #[serde(rename = "agents.reopen_tab")]
    AgentsReopenTab(agents_model::AgentsReopenTabParams),
    #[serde(rename = "agents.notes_append")]
    AgentsNotesAppend(agents_model::AgentsNotesAppendParams),
    #[serde(rename = "agents.checkpoint")]
    AgentsCheckpoint(agents_model::AgentsCheckpointParams),
    #[serde(rename = "agents.actions")]
    AgentsActions(agents_model::AgentsActionsParams),
    #[serde(rename = "agents.check")]
    AgentsCheck(agents_model::AgentsCheckParams),
    #[serde(rename = "agents.suspend")]
    AgentsSuspend(agents_model::AgentsLifecycleParams),
    #[serde(rename = "agents.activate")]
    AgentsActivate(agents_model::AgentsLifecycleParams),
    #[serde(rename = "agents.restart")]
    AgentsRestart(agents_model::AgentsLifecycleParams),
    #[serde(rename = "agents.read_messages")]
    AgentsReadMessages(agents_model::AgentsReadMessagesParams),
    #[serde(rename = "agents.reorder_group")]
    AgentsReorderGroup(agents_model::AgentsReorderGroupParams),
    #[serde(rename = "agents.reorder_tab")]
    AgentsReorderTab(agents_model::AgentsReorderTabParams),
    /// Fork: a turn of the pane's agent started or ended (its hooks).
    #[serde(rename = "pane.report_turn")]
    PaneReportTurn(PaneReportTurnParams),
    #[serde(rename = "agent.prompt")]
    AgentPrompt(AgentPromptParams),
    #[serde(rename = "agent.wait")]
    AgentWait(AgentWaitParams),
    #[serde(rename = "pane.split")]
    PaneSplit(PaneSplitParams),
    #[serde(rename = "pane.swap")]
    PaneSwap(PaneSwapParams),
    #[serde(rename = "pane.move")]
    PaneMove(PaneMoveParams),
    #[serde(rename = "pane.zoom")]
    PaneZoom(PaneZoomParams),
    #[serde(rename = "pane.layout")]
    PaneLayout(PaneLayoutParams),
    #[serde(rename = "pane.process_info")]
    PaneProcessInfo(PaneProcessInfoParams),
    #[serde(rename = "layout.export")]
    LayoutExport(LayoutExportParams),
    #[serde(rename = "layout.apply")]
    LayoutApply(LayoutApplyParams),
    #[serde(rename = "layout.set_split_ratio")]
    LayoutSetSplitRatio(LayoutSetSplitRatioParams),
    #[serde(rename = "pane.neighbor")]
    PaneNeighbor(PaneNeighborParams),
    #[serde(rename = "pane.edges")]
    PaneEdges(PaneEdgesParams),
    #[serde(rename = "pane.focus_direction")]
    PaneFocusDirection(PaneFocusDirectionParams),
    #[serde(rename = "pane.resize")]
    PaneResize(PaneResizeParams),
    #[serde(rename = "pane.scroll")]
    PaneScroll(PaneScrollParams),
    #[serde(rename = "pane.clear")]
    PaneClear(PaneTarget),
    #[serde(rename = "pane.edit_scrollback")]
    PaneEditScrollback(PaneTarget),
    #[serde(rename = "pane.selection.read")]
    PaneSelectionRead(PaneSelectionReadParams),
    #[serde(rename = "pane.copy_motion")]
    PaneCopyMotion(PaneCopyMotionParams),
    #[serde(rename = "pane.copy_search")]
    PaneCopySearch(PaneCopySearchParams),
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),
    #[serde(rename = "pane.current")]
    PaneCurrent(PaneCurrentParams),
    #[serde(rename = "pane.get")]
    PaneGet(PaneTarget),
    #[serde(rename = "pane.focus")]
    PaneFocus(PaneTarget),
    #[serde(rename = "pane.input.set")]
    PaneInputSet(PaneInputSetParams),
    #[serde(rename = "pane.link.activate")]
    PaneLinkActivate(PaneLinkActivateParams),
    #[serde(rename = "pane.link.resolve")]
    PaneLinkResolve(PaneLinkActivateParams),
    #[serde(rename = "pane.rename")]
    PaneRename(PaneRenameParams),
    #[serde(rename = "pane.send_text")]
    PaneSendText(PaneSendTextParams),
    #[serde(rename = "pane.send_keys")]
    PaneSendKeys(PaneSendKeysParams),
    #[serde(rename = "pane.send_input")]
    PaneSendInput(PaneSendInputParams),
    #[serde(rename = "pane.read")]
    PaneRead(PaneReadParams),
    #[serde(rename = "pane.graphics.set")]
    PaneGraphicsSet(PaneGraphicsSetParams),
    #[serde(rename = "pane.graphics.clear")]
    PaneGraphicsClear(PaneGraphicsClearParams),
    #[serde(rename = "pane.graphics.info")]
    PaneGraphicsInfo(PaneTarget),
    #[serde(rename = "pane.graphics.stream")]
    #[schemars(skip)]
    PaneGraphicsStream(PaneGraphicsStreamParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamSet(PaneGraphicsSetParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamDirect(PaneGraphicsDirectParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamOpen(PaneGraphicsStreamParams),
    #[serde(skip)]
    #[schemars(skip)]
    PaneGraphicsStreamClose(PaneGraphicsStreamParams),
    #[serde(rename = "pane.report_agent")]
    PaneReportAgent(PaneReportAgentParams),
    #[serde(rename = "pane.report_agent_session")]
    PaneReportAgentSession(PaneReportAgentSessionParams),
    #[serde(rename = "pane.report_metadata")]
    PaneReportMetadata(PaneReportMetadataParams),
    #[serde(rename = "pane.clear_agent_authority")]
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams),
    #[serde(rename = "pane.release_agent")]
    PaneReleaseAgent(PaneReleaseAgentParams),
    #[serde(rename = "pane.close")]
    PaneClose(PaneTarget),
    #[serde(rename = "popup.close")]
    PopupClose(EmptyParams),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EventsSubscribeParams),
    #[serde(rename = "events.wait")]
    EventsWait(EventsWaitParams),
    #[serde(rename = "pane.wait_for_output")]
    PaneWaitForOutput(PaneWaitForOutputParams),
    #[serde(rename = "integration.list")]
    IntegrationList(EmptyParams),
    #[serde(rename = "integration.install")]
    IntegrationInstall(IntegrationInstallParams),
    #[serde(rename = "integration.uninstall")]
    IntegrationUninstall(IntegrationUninstallParams),
    #[serde(rename = "plugin.link")]
    PluginLink(PluginLinkParams),
    #[serde(rename = "plugin.list")]
    PluginList(PluginListParams),
    #[serde(rename = "plugin.unlink")]
    PluginUnlink(PluginUnlinkParams),
    #[serde(rename = "plugin.enable")]
    PluginEnable(PluginSetEnabledParams),
    #[serde(rename = "plugin.disable")]
    PluginDisable(PluginSetEnabledParams),
    #[serde(rename = "plugin.action.list")]
    PluginActionList(PluginActionListParams),
    #[serde(rename = "plugin.action.invoke")]
    PluginActionInvoke(PluginActionInvokeParams),
    #[serde(rename = "plugin.log.list")]
    PluginLogList(PluginLogListParams),
    #[serde(rename = "plugin.pane.open")]
    PluginPaneOpen(PluginPaneOpenParams),
    #[serde(rename = "plugin.pane.focus")]
    PluginPaneFocus(PluginPaneFocusParams),
    #[serde(rename = "plugin.pane.close")]
    PluginPaneClose(PluginPaneCloseParams),
}

#[cfg(test)]
mod tests;
