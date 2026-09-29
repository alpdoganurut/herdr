# FORK.md — herdr fork manifest

Section 1 drift check: `git diff --diff-filter=A --name-only $(git merge-base origin/master fork) fork -- . ':(exclude)docs/next/api'` must print exactly the section 1 list.

Base: upstream master 621e6b73 (the merge base moves with each sync). Branch: fork (pushed to mine/fork). Sync tool: scripts/fork_sync.sh via /fork-sync.
The sync script parses the sections below by their `## N.` headings; keep the headings and the line formats stable. Machine-read sections (1, 2, 7, 8, 9, 10) hold no prose: one entry per line, blank lines ignored.
Formats: 1 one path per line; 2 a pipe table `| struct | field | default |` whose first row is the header; 7 and 8 `<path><two or more spaces><keyword>: <detail>`, where `*` in section 7 is the default rule; 9 one fixed-string, case-sensitive identifier per line, matched against the added lines of `git diff <base> <upstream> -- . ':(exclude)vendor'` (vendored libghostty already contains `Suspended`); 10 one full nextest test name per line.
Section 2 also covers E0027 (an exhaustive struct pattern missing a fork field): add `<field>: _`.

## 1. Owned files (added by the fork; upstream R/D/T on any = deny)
.claude/skills/fork-sync/SKILL.md
FORK.md
scripts/fork_sync.sh
scripts/fork_sync_lib.py
scripts/test_fork_sync.py
src/api/schema/closed_sessions.rs
src/api/schema/news.rs
src/app/agent_suspend.rs
src/app/agent_transcripts.rs
src/app/closed_sessions.rs
src/app/news.rs
src/app/subagents.rs
src/app/tab_bar_status/output.rs
src/app/tab_color.rs
src/app/tab_remind.rs
src/cli/news.rs
src/cli/tab_closed.rs
src/client/shell/idle_reminders.rs
src/client/shell/news.rs
src/client/shell/notification_format.rs
src/client/shell/settings_closed.rs
src/client/shell/settings_daily_time.rs
src/client/shell/settings_news.rs
src/client/shell/settings_sounds.rs
src/client/shell/suspended_pane.rs
src/client/shell/tab_color.rs
src/client/shell/tab_remind_menu.rs
src/client/shell/tab_sidebar.rs
src/client/shell/tests/idle_reminders.rs
src/client/shell/tests/news.rs
src/client/shell/tests/notification_format.rs
src/client/shell/tests/settings_backups.rs
src/client/shell/tests/settings_closed.rs
src/client/shell/tests/sticky_notifications.rs
src/client/shell/tests/tab_sidebar.rs
src/config/news.rs
src/integration/assets/news/anchors.py
src/integration/assets/news/news_run.py
src/integration/assets/news/sources.json
src/integration/assets/news/system.md
src/integration/assets/news/topic.md
src/integration/assets/news/viewer.py
src/integration/claude_subagent_hooks.rs
src/integration/news_assets.rs
src/persist/agent_transcripts.rs
src/persist/closed_sessions.rs
src/persist/news.rs
src/server/headless/tests/fork_smoke.rs

## 2. Owned fields on upstream structs (E0063 in upstream-authored literals: insert the default)
| struct | field | default |
| PersistedAgentSession | transcript_path | None |
| PaneAgentSessionSnapshot | transcript_path | None |
| PaneSnapshot | suspended_agent | None |
| AppEvent::HookStateReported | transcript_path | None |
| AppEvent::AgentSessionReported | transcript_path | None |
| PaneStateUpdate | previous_suspended | false |
| PaneStateUpdate | suspended | false |
| HandoffRuntimeState | suspended_exit_pending | false |
| PaneDetail | suspended | false |
| AgentPanelEntry | suspended | false |
| TerminalState | suspended_agent | None |
| TerminalState | agent_transcript_paths | std::collections::HashMap::new() |
| TerminalState | active_subagents | std::collections::HashSet::new() |
| TerminalState | subagent_snapshot_seen | false |
| PaneReportSubagentParams | subagent_ids | Vec::new() |
| AgentInfo | subagents | 0 |
| ClientShellAgent | subagents | 0 |
| App | backup_agent_transcripts | config.session.backup_agent_transcripts |
| App | agent_transcript_backup_deadline | None |
| App | agent_transcript_backup_thread | None |
| App | agent_transcript_backup_last | None |
| App | agent_transcript_backup_pending | std::collections::BTreeMap::new() |
| App | news | news::NewsState::new(&config.news, policy.persist_session, Instant::now()) |
| Config | news | crate::config::NewsConfig::default() |
| SessionConfig | backup_agent_transcripts | true |
| UiConfig | sidebar_layout | crate::config::SidebarLayoutConfig::Spaces |
| UiConfig | tab_agent_glyphs | std::collections::BTreeMap::new() |
| UiConfig | tab_agent_glyph_colors | std::collections::BTreeMap::new() |
| UiConfig | idle_reminder_minutes | 10 |
| UiConfig | daily_reminder_time | "09:30".into() |
| SoundConfig | reminder_path | None |
| KeysConfig | toggle_agent_suspend | crate::config::BindingConfig::default() |
| KeysConfig | move_tab_to_group | crate::config::BindingConfig::default() |
| KeysConfig | toggle_groups_folded | crate::config::BindingConfig::default() |
| KeysConfig | restart_agent | crate::config::BindingConfig::default() |
| KeysConfig | cycle_tab_color | crate::config::BindingConfig::default() |
| KeysConfig | toggle_tab_important | crate::config::BindingConfig::default() |
| KeysConfig | open_news | crate::config::BindingConfig::default() |
| KeysConfigOverlay | toggle_agent_suspend | None |
| KeysConfigOverlay | move_tab_to_group | None |
| KeysConfigOverlay | toggle_groups_folded | None |
| KeysConfigOverlay | restart_agent | None |
| KeysConfigOverlay | cycle_tab_color | None |
| KeysConfigOverlay | toggle_tab_important | None |
| KeysConfigOverlay | open_news | None |
| Keybinds | toggle_agent_suspend | crate::config::ActionKeybinds::default() |
| Keybinds | move_tab_to_group | crate::config::ActionKeybinds::default() |
| Keybinds | toggle_groups_folded | crate::config::ActionKeybinds::default() |
| Keybinds | restart_agent | crate::config::ActionKeybinds::default() |
| Keybinds | cycle_tab_color | crate::config::ActionKeybinds::default() |
| Keybinds | toggle_tab_important | crate::config::ActionKeybinds::default() |
| Keybinds | open_news | crate::config::ActionKeybinds::default() |
| ClientShellConfig | sidebar_layout | crate::config::SidebarLayoutConfig::Spaces |
| ClientShellConfig | tab_agent_glyphs | std::collections::BTreeMap::new() |
| ClientShellConfig | tab_agent_glyph_colors | std::collections::BTreeMap::new() |
| ClientShellConfig | toast_sticky | false |
| ClientShellConfig | toast_max_stack | 6 |
| ClientShellConfig | idle_reminder_minutes | 10 |
| ClientShellConfig | daily_reminder_minutes | 570 |
| ClientShellConfig | sound_files | [None, None, None] |
| ClientShellConfig | system_sounds_dir | std::path::PathBuf::from("/System/Library/Sounds") |
| HerdrToastConfig | sticky | false |
| HerdrToastConfig | max_stack | 6 |
| ClientShellState | suspended_pane_ids | std::collections::HashSet::new() |
| ClientShellState | idle_reminders | std::collections::HashMap::new() |
| ClientShellState | scheduled_reminders | std::collections::HashMap::new() |
| ClientShellState | reminder_epochs | std::collections::HashMap::new() |
| ClientShellState | reminder_local_time | None |
| ClientShellState | reminder_daily_minutes | None |
| ClientShellState | news | Default::default() |
| ClientPendingNotification | reminder | None |
| ClientVisibleNotification | reminder | None |
| ShellHitMap | sidebar_tabs | Vec::new() |
| ShellHitMap | sidebar_groups | Vec::new() |
| ShellHitMap | group_toggle_all | Rect::default() |
| ShellHitMap | group_new | Rect::default() |
| ShellHitMap | notification_toasts | Vec::new() |
| ShellHitMap | context_menu_swatches | Vec::new() |
| ShellHitMap | context_menu_remind_options | Vec::new() |
| ShellHitMap | news_row | Rect::default() |
| OverlayRender | menu_swatches | Vec::new() |
| OverlayRender | menu_remind_options | Vec::new() |
| ShellRenderState | sidebar_tab_drop_row | None |
| ShellRenderState | idle_reminders | &self.idle_reminders |
| ShellRenderState | scheduled_reminders | &self.scheduled_reminders |
| ShellRenderState | news_row | None |
| ClientSettingsOverlay | transcripts | None |
| ClientSettingsOverlay | loading_transcripts | false |
| ClientSettingsOverlay | closed | Box::default() |
| ClientSettingsOverlay | idle_reminder_minutes | 10 |
| ClientSettingsOverlay | sound_picker | None |
| ClientSettingsOverlay | daily_time_picker | None |
| ClientSettingsOverlay | news | Box::default() |
| ClientConfirmCloseOverlay | close_group | true |
| ClientContextMenuTarget::Tab | agent | None |
| ClientContextMenuTarget::Tab | color | Default::default() |
| ClientContextMenuTarget::Tab | important | false |
| ClientContextMenuTarget::Tab | remind | Default::default() |
| Tab | color | None |
| TabSnapshot | color | None |
| TabInfo | color | None |
| ClientShellTab | color | None |
| Tab | important | false |
| Tab | remind_every | None |
| TabSnapshot | remind | false |
| TabSnapshot | important | false |
| TabSnapshot | remind_every | None |
| TabInfo | important | false |
| TabInfo | remind_every | None |
| ClientShellTab | important | false |
| ClientShellTab | remind_every | None |
| PaneMoveRecoveryContext | previous_tab_color | None |
| PaneMoveRecoveryContext | previous_tab_important | false |
| PaneMoveRecoveryContext | previous_tab_remind_every | None |
| TabBarRightEntryConfig::Command | lines | 1 |
| TabBarRightEntryConfig::Command | ansi | false |

## 3. Removed or re-signatured upstream symbols (E0425/E0061 at a new upstream call site = deny)
app::api_helpers::pane_agent_status(state, seen) -> removed; app::api_helpers::agent_status(state, seen, suspended) or app::api_helpers::terminal_agent_status(terminal, seen)
app::api_helpers::tab_attention_priority(state, seen) -> tab_attention_priority(state, seen, suspended)
workspace::Workspace::aggregate_state(terminals) -> (AgentState, bool) -> returns workspace::aggregate::PaneAttention { state, seen, suspended } (E0308 at an upstream tuple destructure)
workspace::aggregate::pane_attention_priority(state, seen) -> pane_attention_priority(PaneAttention)
app::agent_view::status_name(state, seen) -> status_name(state, seen, suspended)
client::shell::ClientShellState::activate_tab_context_action(tab_id, workspace_id, action, outcome) -> (tab_id, workspace_id, agent, action, outcome)
client::shell::ClientShellState.visible_notification: Option<ClientVisibleNotification> -> removed; visible_notifications: VecDeque<ClientVisibleNotification> (timed mode holds at most one card: Some(x) -> push_back(x), .as_ref() -> .front(), .is_none() -> .is_empty(); queued_notifications is unchanged)
client::shell::ClientShellState::focus_visible_notification(outcome) -> kept as a wrapper over focus_notification_at(index, outcome); the timed toast is index 0, the newest sticky card the last index
client::shell::overlays::render_client_overlay(b, o, s, endpoints, active_endpoint_id, k, p) -> (b, o, s, endpoints, active_endpoint_id, k, c: &ClientShellConfig, p) (the settings sound section reads the config)
client::shell::settings::save_settings_edit -> pub(super) (settings_sounds.rs calls it)
app::tab_bar_status::spawn_status_command(7 args) -> #[cfg(test)] wrapper passing StatusOutputFormat::UPSTREAM; production code calls spawn_status_command_with_format(.., format: StatusOutputFormat) (a new upstream non-test call site = E0425: switch it to the _with_format form)
client::shell::ClientShellState::receive_notification -> kept; its replace-by-pane block moved into replace_pane_notifications(endpoint_id, pane_id, now) -> bool (cleared a visible card), shared with the idle reminder engine
Visibility widened by the fork (upstream renaming or narrowing one breaks fork code): app::agents::DEFAULT_AGENT_START_TIMEOUT, app::agents::available_shell_name, app::api::agents::AGENT_PROMPT_SUBMIT_DELAY, app::terminal_targets::{terminal_targets, terminal_target_candidate}, integration::home_dir, client::shell::notification_policy::{notification_target_is_active, COMPLETION_EVIDENCE_GRACE} (pub(super)), app::agent_resume::shell_command_from_argv (pub(super); the closed-session reopen types it)

## 4. Owned enum variants (append last; E0004 in upstream match = deny)
AgentStatus::Suspended   [src/api/schema/common.rs, last after Unknown; wire: JSON "suspended" in the socket API, events and the client shell snapshot, any non-human-readable serde codec encodes it as variant index 5; append-closed, deny on conflict]
Method::AgentSuspend   [src/api/schema.rs, after AgentStart, not last; wire by serde name "agent.suspend", order irrelevant]
Method::AgentActivate   [src/api/schema.rs, after AgentSuspend; wire "agent.activate"]
Method::AgentRestart   [src/api/schema.rs, after AgentActivate; wire "agent.restart"]
Method::AgentTranscripts   [src/api/schema.rs, after AgentRestart; wire "agent.transcripts"]
Method::TabSetColor   [src/api/schema.rs, after AgentTranscripts; wire "tab.set_color"]
Method::TabSetRemind   [src/api/schema.rs, after TabSetColor; wire "tab.set_remind"]
Method::TabSetReminder   [src/api/schema.rs, after TabSetRemind; wire "tab.set_reminder"]
Method::PaneReportSubagent   [src/api/schema.rs, after TabSetReminder; wire "pane.report_subagent"]
Method::SessionClosedList   [src/api/schema.rs, after PaneReportSubagent; wire "session.closed_list"]
Method::SessionClosedReopen   [src/api/schema.rs, after SessionClosedList; wire "session.closed_reopen"]
Method::SessionClosedRemove   [src/api/schema.rs, after SessionClosedReopen; wire "session.closed_remove"]
Method::NewsRun   [src/api/schema.rs, after SessionClosedRemove; wire "news.run"]
Method::NewsStatus   [src/api/schema.rs, after NewsRun; wire "news.status"]
Method::NewsGet   [src/api/schema.rs, after NewsStatus; wire "news.get"]
Method::NewsHistory   [src/api/schema.rs, after NewsGet; wire "news.history"]
Method::NewsOpen   [src/api/schema.rs, after NewsHistory; wire "news.open"]
Method::NewsSetEnabled   [src/api/schema.rs, after NewsOpen, last of the fork block; wire "news.set_enabled"]
TabRemindInterval   [src/api/schema/tabs.rs, fork-owned enum after TabSetRemindParams; wire "5m" "10m" "30m" "1h" "6h" "daily", Unknown the serde(other) fallback; JSON records, session.json and the bincode client snapshot (variant index), append-closed]
TabRemindEvery   [src/api/schema/tabs.rs, fork-owned closed enum, "off" plus the intervals; part of the tab.set_reminder digest, so a new interval needs a new method name]
crate::sound::Sound::Reminder   [src/sound.rs, last after Request, with ReminderBase { Done, Request }; client-only (never on the wire); the server's sound_notify_message and app/actions.rs client_notification_kind match it]
ClientShellAction::PreviewSound   [src/client/shell/state.rs, last; the settings sound picker's preview, handled in shell_runtime.rs; internal]
ResponseResult::AgentSuspended   [src/api/schema/response.rs, after AgentStarted, not last; wire by tag "agent_suspended"]
ResponseResult::AgentActivated   [src/api/schema/response.rs; wire "agent_activated"]
ResponseResult::AgentRestarted   [src/api/schema/response.rs, after AgentActivated; wire "agent_restarted"]
ResponseResult::AgentTranscripts   [src/api/schema/response.rs; wire "agent_transcripts"]
ResponseResult::SessionClosedList   [src/api/schema/response.rs, after AgentTranscripts; wire "session_closed_list"]
ResponseResult::NewsStatus   [src/api/schema/response.rs, after SessionClosedList; wire "news_status"]
ResponseResult::NewsGet   [src/api/schema/response.rs, after NewsStatus; wire "news_get"]
ResponseResult::NewsHistory   [src/api/schema/response.rs, after NewsGet, last of the fork block; wire "news_history"]
KeybindAction::ToggleAgentSuspend   [src/input/keybindings.rs, after ClearPane; internal]
KeybindAction::MoveTabToGroup   [src/input/keybindings.rs; internal]
KeybindAction::ToggleGroupsFolded   [src/input/keybindings.rs; internal]
KeybindAction::RestartAgent   [src/input/keybindings.rs, after ToggleGroupsFolded; internal]
KeybindAction::CycleTabColor   [src/input/keybindings.rs, after RestartAgent; internal]
KeybindAction::ToggleTabImportant   [src/input/keybindings.rs, after CycleTabColor; internal]
KeybindAction::OpenNews   [src/input/keybindings.rs, after ToggleTabImportant; internal]
ClientSettingsSection::Backups   [src/client/shell/state.rs, before Reminders; UI tab order; internal]
ClientSettingsSection::Reminders   [src/client/shell/state.rs, after Backups; UI tab order, ALL keeps Backups, Reminders, ClosedSessions then News last; internal]
ClientSettingsSection::ClosedSessions   [src/client/shell/state.rs, after Reminders; the `closed` tab; internal]
ClientSettingsSection::News   [src/client/shell/state.rs, last after ClosedSessions; the `news` tab; internal]
ClientContextMenuAction::SuspendAgent   [src/client/shell/state.rs, last five; internal]
ClientContextMenuAction::ActivateAgent   [internal]
ClientContextMenuAction::Ungroup   [internal]
ClientContextMenuAction::CloseGroup   [internal]
ClientContextMenuAction::RestartAgent   [src/client/shell/state.rs, after CloseGroup; internal]
ClientContextMenuAction::Color   [src/client/shell/state.rs, after RestartAgent; the tab menu's swatch row; internal]
ClientContextMenuAction::Important   [src/client/shell/state.rs, after Color; the tab menu's important toggle; internal]
ClientContextMenuAction::RemindTop   [src/client/shell/state.rs, after Important; the reminder selector's first row; internal]
ClientContextMenuAction::RemindBottom   [src/client/shell/state.rs, after RemindTop; the selector's second row; internal]
ClientContextMenuAction::NewsRun   [src/client/shell/state.rs, after RemindBottom; the News row menu's Run now; internal]
ClientContextMenuAction::NewsOpen   [src/client/shell/state.rs, after NewsRun; the News row menu's Open; internal]
ClientContextMenuAction::NewsToggleSchedule   [src/client/shell/state.rs, last after NewsOpen; the News row menu's Pause / Resume schedule; internal]
ConfigEdit::IdleReminderMinutes   [src/config/write.rs, after ToastDelivery; the settings overlay's reminders tab; internal]
ConfigEdit::DailyReminderTime   [src/config/write.rs, after SoundFile; the reminders tab's daily time picker, written as a quoted "HH:MM"; internal]
ConfigEdit::NewsEnabled   [src/config/write.rs, after DailyReminderTime; `news.enabled`, written by news.set_enabled on the server and by the settings news tab; internal]
ConfigEdit::NewsIntervalHours   [src/config/write.rs, after NewsEnabled; `news.interval_hours`, the news tab's interval picker; internal]
ConfigEdit::NewsQuietHours   [src/config/write.rs, last after NewsIntervalHours; `news.quiet_hours` as a quoted window or "" for off, the news tab's quiet-hours picker; internal]
ConfigEdit::SoundFile   [src/config/write.rs, after IdleReminderMinutes; the settings sound pickers ([ui.sound] done_path / request_path / reminder_path); internal]
ClientContextMenuTarget::Group   [src/client/shell/state.rs, between Tab and Pane; internal]
ClientContextMenuTarget::News   [src/client/shell/state.rs, last after Pane; the tabs layout's pinned News row, `{ enabled }`; internal]
ClientChromeDrag::SidebarTab   [src/client/shell/state.rs, before PaneSplit; internal]
ClientRenameTarget::MoveTabToGroup   [src/client/shell/state.rs, last; internal]
PendingEndpointKind::AgentTranscripts   [src/client/shell/state.rs, after IntegrationInstall; internal]
PendingEndpointKind::SessionClosedList   [src/client/shell/state.rs, after AgentTranscripts; internal]
PendingEndpointKind::SessionClosedReopen   [src/client/shell/state.rs, after SessionClosedList; internal]
PendingEndpointKind::SessionClosedRemove   [src/client/shell/state.rs, after SessionClosedReopen; internal]
PendingEndpointKind::NewsGet   [src/client/shell/state.rs, after SessionClosedRemove; internal]
PendingEndpointKind::NewsRun   [src/client/shell/state.rs, after NewsGet; internal]
PendingEndpointKind::NewsOpen   [src/client/shell/state.rs, after NewsRun; internal]
PendingEndpointKind::NewsSetEnabled   [src/client/shell/state.rs, after NewsOpen; internal]
AgentRenameError::Suspended   [src/app/agents.rs, after PendingLaunch; internal, surfaces as error code agent_suspended]
SubagentEvent::Snapshot   [src/api/schema/panes.rs, fork-owned enum, last after Stop; wire "snapshot"]
AgentSuspendError::SubagentsRunning   [src/app/agent_suspend.rs, fork-owned enum, after Working; surfaces as error code agent_subagents_running, restart inherits it through AgentRestartError::Suspend]

## 5. Owned API methods and digests
agent.suspend, agent.activate, agent.restart, agent.transcripts: fork-defined (Method variants, api_method_name arms, request_changes_ui for suspend/activate/restart, CLI `herdr agent suspend|activate|restart|transcripts`).
agent.restart digest: dc124dcfe9d7fc0fe3d9a85e00548a0263574d3de68ffd16370f2b6ba67ad062 (AgentRestartParams { target }).
tab.set_color: fork-defined (Method::TabSetColor, api_method_name arm, request_changes_ui, handler in src/app/tab_color.rs, CLI `herdr tab color`). Response: ResponseResult::TabInfo, as tab.rename.
tab.set_color digest: 47beb991c2a5f54fffc12c8bfc0a27c725487f94e3088474e80bc8526ced74c2 (TabSetColorParams { tab_id, color: Option<TabColor> }, TabColor snake_case with the Unknown fallback).
tab.set_remind: fork-defined (Method::TabSetRemind, api_method_name arm, request_changes_ui, handler in src/app/tab_remind.rs); the boolean form of tab.set_reminder's `important`. Response: ResponseResult::TabInfo, as tab.rename.
tab.set_remind digest: e9c63bb9f29eacf951cfee82469449b61eb3b72ae80d0c0c9a3cb6319aa4bb29 (TabSetRemindParams { tab_id, remind: bool }, remind required).
tab.set_reminder: fork-defined (Method::TabSetReminder, api_method_name arm, request_changes_ui, handler in src/app/tab_remind.rs, CLI `herdr tab important` / `herdr tab remind`). Response: ResponseResult::TabInfo.
tab.set_reminder digest: 597627419b2ba06f50d220dd1ea4218fbb2204ba3452bbc25897cdc417da5b1e (TabSetReminderParams { tab_id, important: Option<bool>, every: Option<TabRemindEvery> }).
pane.report_subagent: fork-defined (Method::PaneReportSubagent, api_method_name arm, request_changes_ui, handler in src/app/subagents.rs), sent by the Claude hook asset's `subagent` and `stop` actions; not advertised to the client shell, so no digest. Params PaneReportSubagentParams { pane_id, agent, event: SubagentEvent start|stop|snapshot, subagent_id (serde default), subagent_ids (serde default; the snapshot) }.
TabColor (src/api/schema/tabs.rs) is fork-owned and closed: its values are part of the tab.set_color digest, so a new color needs a new method name; Unknown stays the serde(other) fallback (JSON snapshot, TabInfo, session.json).
session.closed_list, session.closed_reopen, session.closed_remove: fork-defined (Method::SessionClosedList/Reopen/Remove, api_method_name arms, request_changes_ui for closed_reopen only, handlers in src/app/closed_sessions.rs, CLI `herdr tab closed|reopen` in src/cli/tab_closed.rs). Responses: closed_list ResponseResult::SessionClosedList { sessions: Vec<ClosedSessionInfo> } (a response type, so not digest-covered), closed_reopen upstream's ResponseResult::TabCreated as tab.create, closed_remove Ok. Errors closed_session_not_found, closed_session_not_resumable, closed_sessions_unavailable, closed_session_reopen_failed.
session.closed_list digest: 36061ddc74238ec4cbe0e9b01e685d417718a12f09ebac9334b7c1fc754c22cd (EmptyParams).
session.closed_reopen digest: f18fab7830a2e3c6e915def7f93b9afb48f406bc78001cecd0a17f188291306d (ClosedSessionTarget { id }).
session.closed_remove digest: 9583e60d3393fbbcec1a209e4ce7b227ce38d429b0d0f1e9dac1ac2bb3befbdb (ClosedSessionTarget { id }).
news.run, news.status: fork-defined (Method::NewsRun/NewsStatus, api_method_name arms, request_changes_ui for news.run only (it may open the News tab), handlers in src/app/news.rs, CLI `herdr news run|status|log [n]` in src/cli/news.rs). Both take EmptyParams and answer ResponseResult::NewsStatus { status: NewsStatusInfo } (a response type, so not digest-covered); news.run starts a run first and refuses with news_run_in_flight while one lasts, news_unavailable without a persisted session, news_run_failed when the runner install or the tab fails. news.status is not advertised to the client shell; news.run is (the News row menu's "Run now").
news.get, news.history, news.open, news.set_enabled: fork-defined (Method::NewsGet/NewsHistory/NewsOpen/NewsSetEnabled, api_method_name arms, request_changes_ui for news.open only (it focuses the News tab), handlers in src/app/news.rs, CLI `herdr news open [--edition N] | history [--days N] [--json] | enable | disable` in src/cli/news.rs). news.get (EmptyParams), news.open (NewsOpenParams { edition: Option<u32> }) and news.set_enabled (NewsSetEnabledParams { enabled }) answer ResponseResult::NewsGet { news: NewsGetInfo }; news.history (NewsHistoryParams { days: Option<u32> }, not advertised to the client shell) answers ResponseResult::NewsHistory { editions: Vec<NewsEditionInfo> } from `<home>/editions/index.json`. news.open focuses the tab (created with the page viewer when gone), with `edition` quits the pane with `q` and runs the viewer on that edition once the shell prompt is back (a pending pane command driven from handle_news_tasks), refusing news_run_in_flight while a run lasts and news_edition_not_found for an unknown number; news.set_enabled writes `news.enabled` through ConfigEdit::NewsEnabled and calls App::reload_config (news_config_write_failed on a write error). Focusing the News tab clears its important mark on the next scheduler pass (clear_news_unread_when_focused in handle_news_tasks).
news.get digest: aa473a7480d449faed99b6e7958a359b79c42352fa2baf37a080c17d17c8c855 (EmptyParams).
news.open digest: bb5614fbd62d35a07127482009b8f69808325fecf27588dc70da6a19b15b4fb7 (NewsOpenParams { edition: Option<u32> }).
news.run digest: 19594c58ed9598a02c2edd64f939dfd11e71b82267bdcaac244a3f5e840209ff (EmptyParams).
news.set_enabled digest: 1246ec256f7160ad56a4bc171faceb2e68649e592628021b0f821624c87b2d74 (NewsSetEnabledParams { enabled: bool }).
pane.move: upstream method, advertised to the client shell only by the fork (absent from base CLIENT_SHELL_METHODS).
CLIENT_SHELL_METHODS (src/server/client_commands.rs): union, sorted; the test advertised_client_shell_methods_are_sorted_unique_and_in_schema enforces it.
Digest asserts in advertised_client_shell_method_shapes_stay_at_the_v1_contract: the fork appends fifteen `actual.remove(..)` asserts (agent.suspend, agent.activate, pane.move, agent.transcripts, agent.restart, tab.set_color, tab.set_remind, tab.set_reminder, session.closed_list, session.closed_remove, session.closed_reopen, news.get, news.open, news.run, news.set_enabled) after upstream's pane.link.resolve assert. Resolve an assert-block conflict as the union of `actual.remove` blocks, upstream first, no method name twice.
Any digest value change = deny (contract change, never a fixture fix). pane.move's digest covers upstream-owned PaneMoveParams/PaneMoveDestination: an upstream reshape fails it after a clean merge, and that is a deny.
tests/fixtures/endpoint-*-v1.json and src/protocol/** frozen tests: never edited (the fork has no diff under tests/).
Upstream adding "pane.move" to CLIENT_SHELL_METHODS, or adding any section 9 identifier = deny (collision).

## 6. Config keys (cross-checked by scripts/config_reference_check.py)
ui.sidebar_layout, ui.tab_agent_glyphs, ui.tab_agent_glyph_colors, session.backup_agent_transcripts,
keys.toggle_agent_suspend, keys.move_tab_to_group, keys.toggle_groups_folded, keys.restart_agent, keys.cycle_tab_color, keys.toggle_tab_important, keys.open_news,
ui.toast.herdr.sticky, ui.toast.herdr.max_stack, ui.idle_reminder_minutes, ui.daily_reminder_time, ui.sound.reminder_path,
news.enabled, news.interval_hours, news.quiet_hours, news.model
ui.tab_bar_right command entry fields `lines` (u8, default 1, clamped to 1..=4 with a config warning) and `ansi` (bool, default false) are not separate keys: they are documented in the ui.tab_bar_right entry's description in config-reference.json (appended after upstream's sentences) and in configuration.mdx in the paragraph plus example directly after upstream's "Separators appear only between visible entries" paragraph.
Placement in docs/next/website/src/data/config-reference.json: keys.* directly after keys.clear_pane, ui.* directly after ui.sidebar_collapsed_mode (in the order ui.sidebar_layout, ui.tab_agent_glyphs, ui.tab_agent_glyph_colors, ui.idle_reminder_minutes, ui.daily_reminder_time), ui.sound.reminder_path directly after ui.sound.request_path, session.backup_agent_transcripts last in the session group, ui.toast.herdr.sticky and ui.toast.herdr.max_stack directly after ui.toast.herdr.position in the notifications group. The same keys appear as commented defaults in src/main.rs DEFAULT_CONFIG (after clear_pane, sidebar_collapsed_mode, startup_per_agent_delay_ms) and in docs/next/website/src/content/docs/configuration.mdx.
The news.* keys form their own group (id `news`, title News) directly after the session group in config-reference.json and a `[news]` block directly after the `[session]` block in DEFAULT_CONFIG; they are not in configuration.mdx (fork-only feature, no release docs).
After any merge touching config-reference.json: python3 -m json.tool on the file, then python3 scripts/config_reference_check.py.

## 7. Per-file merge rules
docs/next/CHANGELOG.md  take-theirs: fork entries live in section 11
docs/next/api/herdr-api.schema.json  take-theirs: after every .rs conflict is resolved, regenerate with HERDR_UPDATE_API_SCHEMA=1 cargo nextest run generated_protocol_schema_artifact_is_current, rerun it clean, require git diff --exit-code docs/next/api/
Cargo.lock  take-theirs: exact; the --locked gate verifies
Cargo.toml  take-theirs: the fork does not touch it
skills/herdr/SKILL.md  take-theirs+reapply: re-apply the fork's hunks (two at the time of writing) with git diff <base> <fork tip> -- skills/herdr/SKILL.md | git apply --3way; a hunk that does not apply = deny
docs/next/website/src/data/config-reference.json  additive: upstream entries first, fork entries after (placement in section 6), then json.tool and config_reference_check.py
docs/next/website/src/content/docs/configuration.mdx  additive: upstream first, fork after
src/server/client_commands.rs  section-5
src/api/schema/common.rs  deny: AgentStatus is append-closed
tests/api_ping.rs  deny: the fork's one line is the protocol literal in ping_over_socket_returns_version; it must equal PROTOCOL_VERSION in src/protocol/wire.rs after the sync
tests/support/mod.rs  deny: the fork's one line is CURRENT_PROTOCOL; it must equal PROTOCOL_VERSION in src/protocol/wire.rs after the sync
src/protocol/wire.rs  deny: PROTOCOL_VERSION is the fork's value (upstream + 3: ClientShellTab.color (23), ClientShellTab.remind with ClientShellAgent.subagents (24), ClientShellTab.important and remind_every replacing remind (25); when upstream bumps, resolve to upstream's new value + 3 and keep the fork comment), the fork's other lines are one inside deserialize_client_shell_agent_status, ClientShellTab.color, .important, .remind_every and ClientShellAgent.subagents (serde default, deliberately not skip_serializing_if: the bincode round-trip needs every field) and `color: None` / `important: false` / `remind_every: None` in the client_shell_snapshot_roundtrip literal (section 2)
src/api/schema.rs  additive: fork Method variants stay directly after AgentStart (NewsRun, NewsStatus, NewsGet, NewsHistory, NewsOpen and NewsSetEnabled close the block); the fork's is_zero stays directly after is_false; `pub mod closed_sessions;` and `pub use closed_sessions::*;` directly after the agents lines, `pub mod news;` and `pub use news::*;` directly after them
src/persist.rs  additive: `pub mod closed_sessions;` directly after `pub mod agent_transcripts;`, `pub mod news;` directly after it; the closed-sessions and news lines last in the module doc
src/app/mod.rs  additive: `mod closed_sessions;` directly after `mod agents;`, `mod news;` directly after it
src/cli.rs  additive: `mod tab_closed;` directly after `mod tab;`, `mod news;` directly after it; the `news` arm directly after the `session` arm in maybe_run
src/client/shell.rs  additive: `mod settings_closed;` directly after `mod settings;`, `mod news;` directly after `mod mouse;`, `mod settings_news;` directly after `mod settings_daily_time;`
src/api/schema/response.rs  additive: fork ResponseResult variants stay directly after AgentStarted (NewsStatus, NewsGet and NewsHistory close the block); `use super::news::{NewsEditionInfo, NewsGetInfo, NewsStatusInfo};` directly after the closed_sessions use
src/api/schema/agents.rs  additive: fork params types stay after AgentStartParams; AgentInfo.subagents stays directly after state_change_seq
src/api/schema/panes.rs  additive: PaneReportSubagentParams and SubagentEvent stay last in the file
src/integration/mod.rs  additive: `mod claude_subagent_hooks;` directly after `mod claude_settings;`, `pub(crate) mod news_assets;` directly after it
src/integration/targets.rs  additive: the claude_subagent_hooks install/uninstall lines stay directly after install_claude_settings / uninstall_claude_settings
src/integration/assets/claude/herdr-agent-state.sh  deny: the fork's hunks are `subagent|stop` in the action case, the send() helper, and the `subagent` (agent_type filter) and `stop` (background_tasks snapshot) branches before the SessionStart filter, plus HERDR_INTEGRATION_VERSION=11 (upstream 10 + 1; upstream bumping it = resolve to upstream + 1 in the .sh, the .ps1 and CLAUDE_INTEGRATION_VERSION); re-apply them on upstream's new asset by hand
src/integration/tests.rs  deny: the fork's lines are the two SubagentStop asserts after install_claude (one entry, the subagent hook; none on Windows), the three Stop asserts (one entry, the stop hook; none on Windows) and the expected Claude integration version 11 in the two status tests
src/api/schema/tabs.rs  additive: TabInfo.color, .important, .remind_every stay the last fields; TabColor, TabSetColorParams, TabSetRemindParams, TabRemindInterval, TabRemindEvery and TabSetReminderParams stay after TabInfo
src/cli/tab.rs  additive: the fork's `color`, `important`, `remind`, `closed` and `reopen` arms stay after `rename` (closed and reopen call src/cli/tab_closed.rs), tab_color, tab_important, tab_remind and send_reminder before tab_close, their help lines after the rename line; the fork's tests module stays last
src/cli/spec.rs  additive: the fork's `closed` and `reopen` subcommands directly before the tab `close` subcommand; `.subcommand(news_command())` directly after session_command() in command(), fn news_command directly before fn session_command; spec_models_tab_closed_and_reopen, spec_models_news_run_status_and_log then spec_models_news_open_history_enable_and_disable directly before spec_models_tab_remind_values
src/api/server.rs  additive: fork arms stay after the agent.start arm
src/api/mod.rs  additive: fork arms stay after Method::AgentStart (Method::SessionClosedReopen directly after PaneReportSubagent, Method::NewsRun then Method::NewsOpen directly after it)
src/config/model.rs  additive: upstream first, fork lines directly after each clear_pane line; Config.news the last field (NewsConfig in the super import); HerdrToastConfig sticky/max_stack directly after position (struct, Default, impl HerdrToastConfig after the Default impl), the *_TOAST_MAX_STACK consts directly after MAX_TOAST_DELAY_SECONDS, the *_IDLE_REMINDER_MINUTES consts after them; UiConfig.idle_reminder_minutes and daily_reminder_time directly after tab_agent_glyph_colors (struct and Default), effective_idle_reminder_minutes, effective_daily_reminder_minutes, daily_reminder_diagnostic and idle_reminder_diagnostic last in impl UiConfig, parse_time_of_day directly before impl Default for ToastConfig; KeysConfigOverlay.toggle_tab_important carries `alias = "toggle_tab_remind"`
src/config/tab_bar.rs  additive: MAX_TAB_BAR_COMMAND_LINES directly after MAX_TAB_BAR_RIGHT_ENTRIES, default_command_lines and effective_tab_bar_command_lines after default_command_timeout_seconds, Command.lines and .ansi the last fields of the variant, the lines clamp diagnostic first in tab_bar_right_diagnostics' Command arm (which binds `lines, ansi: _`), the fork test last in the tests module
src/app/tab_bar_status.rs  deny: the fork's hunks are `mod output; use output::StatusOutputFormat;` after the imports, TabBarCommandRuntime.format (last field), `lines, ansi` in configure_tab_bar_status' Command arm and `format: StatusOutputFormat::new(*lines, *ansi)`, spawn_status_command_with_format in handle_tab_bar_status_tasks, spawn_status_command turned into a #[cfg(test)] wrapper, run_status_command's `format` parameter and its two calls into output::, and `lines: 1, ansi: false` in three test literals; re-apply them on upstream's new file by hand
src/config/io.rs  additive: `"news"` in KNOWN_TOP_LEVEL_CONFIG_KEYS (sorted, after keys); the `news` load_live_section call last, directly after the remote one
src/config.rs  additive: `mod news;` directly after `mod model;`, `news::{NewsConfig, QuietHours},` in the pub use block directly before the sound line; `tab_bar::{effective_tab_bar_command_lines, MAX_TAB_BAR_COMMAND_LINES},` in the pub(crate) use block directly after the tab_bar group; the fork's `.chain(self.ui.toast.herdr.diagnostic())` then `.chain(self.ui.idle_reminder_diagnostic())` then `.chain(self.ui.daily_reminder_diagnostic())` then `.chain(self.news.diagnostics())` stay last in Config::collect_diagnostics
src/config/write.rs  additive: ConfigEdit::IdleReminderMinutes, SoundFile, DailyReminderTime, NewsEnabled, NewsIntervalHours, NewsQuietHours stay last in the enum and in each match (format_time_of_day directly after the enum, re-exported from src/config.rs); their tests first in the tests module
src/sound.rs  additive: Sound::Reminder, ReminderBase, Sound::base and preview directly after the Sound enum; play()'s built-in match goes through base()
src/config/sound.rs  additive: SoundConfig.reminder_path after request_path (struct, Default, path_for arm, diagnostics list); supported_sound_extension directly before impl AgentSoundOverrides; its test before missing_sound_file_produces_diagnostic
src/config/keybinds.rs  additive: upstream first, fork lines directly after each clear_pane line
src/input/keybindings.rs  additive: upstream first, fork lines directly after each ClearPane line
src/input/keybind_help.rs  additive: upstream first, fork entries directly after the clear pane entry
src/main.rs  additive: DEFAULT_CONFIG comment lines, upstream first; the `[news]` block directly after the `[session]` block
src/client/shell/state.rs  additive: upstream first, fork after; ClientSettingsSection::ALL keeps Backups, Reminders, ClosedSessions then News last; ClientSettingsOverlay.closed sits between transcripts and loading_transcripts, .news is the last field
src/client/shell/tests/mod.rs  additive: fork module lines
src/server/headless/tests/mod.rs  additive: the fork_smoke module line; test literals per section 2
*  deny: anything that is not a structural additive conflict (zdiff3 base empty, both sides pure insertions)

## 8. Extended surfaces (upstream touch forces human review in the report, even when green)
src/client/shell/notifications.rs  mid-logic: render_visible_notification and render_mobile_notification_banner swap the drawn `●` for notification_glyph (reminder marker, `✓` finished, `×` needs attention) via put_notification_glyph after the upstream render; render_notification_card and render_mobile_notice_banner are unchanged
src/terminal/state.rs  mid-logic: set_detected_state_with_screen_signals_at (suspend reconcile, hook-clear durable session, name kept on exit), clear_full_lifecycle_hook_suppression_for_detected_agent (replacement sessions), set_agent_session_ref_for_session_start (launch-session identity), release_agent_with_mutation, managed_agent_launch_pending, managed_agent_interactive_ready, managed_agent_kind, reconcile_managed_agent_at, clear_agent_name, clear_agent_runtime_identity_after_respawn; active_subagents (with subagent_snapshot_seen) is forgotten in recompute_effective_state on an agent label change (before the early return; before the first snapshot an Idle state also clears the set, the older Claude rule), release_agent_with_mutation, begin_agent_suspend and clear_agent_runtime_identity_after_respawn; a turn end and an Unknown with the same label keep it; replace_subagents takes the Stop snapshot; active_subagent_count is len() in any state, 0 while suspended
src/app/actions.rs  mid-logic: expire_agent_metadata_at, handle_app_event (transcript path before session routing), update_terminal_state_with_completion_policy (suspended in the captured tuple, dirty and completion suppression; clear_subagents when mutation.session_ref_changed)
src/app/agent_suspend.rs  mid-logic: suspend_resolved_agent refuses with SubagentsRunning after the Working check (no input written)
src/app/api.rs  mid-logic: emit_pane_state_update (status computed with suspended on both sides); handle_api_request dispatches Method::NewsRun / NewsStatus / NewsGet / NewsHistory / NewsOpen / NewsSetEnabled directly after the SessionClosedRemove arm
src/app/api/agents.rs  mid-logic: queue_agent_prompt and handle_agent_send_keys refuse suspended panes; any new upstream input method does not
src/app/api/panes.rs  mid-logic: handle_pane_report_agent and handle_pane_report_agent_session derive transcript_path
src/app/api_helpers.rs  mid-logic: status mapping moved to workspace::aggregate
src/app/creation.rs  mid-logic: tab_info (color, important, remind_every), pane_info, workspace_info, terminal_agent_session_info
src/app/mod.rs  mid-logic: App::new (news field from NewsState::new), apply_live_config (session section block restructured; a `news` section block directly before the graphics check applies NewsState::apply_config)
src/app/session.rs  mid-logic: save_session_on_shutdown (early return became if/else, backup pass appended)
src/app/agents.rs  mid-logic: rename refuses suspended; live_runtime_agent split into live_runtime_agent_job; agent_info sets subagents from active_subagent_count
src/app/agent_view.rs  mid-logic: apply_agent_view, validate_field_value, status_name
src/app/agent_resume.rs  mid-logic: start_pending_agent_resume restores the transcript backup before the resume command
src/app/runtime.rs  mid-logic: next_headless_loop_deadline_with_git_refresh gains four deadlines (suspend exit, restart resume, transcript backup, next_news_deadline directly after next_tab_bar_status_deadline)
src/workspace/aggregate.rs  mid-logic: pane_details, aggregate_state, agent_status, agent_status_priority
src/persist/snapshot.rs  mid-logic: capture_tab agent_session block rewritten to persistable_agent_session; capture_tab copies Tab.color, important (also as the legacy `remind`) and remind_every into TabSnapshot
src/persist/restore.rs  mid-logic: restore_tab (resume disabled for suspended panes, handoff exit wait, color restored with Unknown dropped to None, important from important or the legacy remind, remind_every with Unknown dropped), unavailable_restored_terminal, pane_restore_startup, persisted_agent_session_from_snapshot
src/server/headless.rs  mid-logic: handle_scheduled_tasks_headless (backup pass, suspend escalation, start_pending_agent_restarts, handle_news_tasks directly after handle_tab_bar_status_tasks)
src/server/headless/lifecycle.rs  mid-logic: perform_live_handoff sets suspended_exit_pending
src/pane.rs  mid-logic: handoff_runtime_state, from_handoff_fd
src/protocol/wire.rs  mid-logic: deserialize_client_shell_agent_status
src/client/shell.rs  mid-logic: status_priority renumbered (Unknown 0 -> 1, Suspended 0), status_icon, status_text, status_color
src/client/shell/input.rs  mid-logic: push_pane_key and push_focused_pane_event are the only key/text/paste lock points
src/config/sound.rs  mid-logic: SoundConfig::diagnostics accepts supported_sound_extension (mp3; on macOS also aiff, aif, caf, m4a) instead of mp3 only; path_for returns early for Sound::Reminder
src/server/headless.rs  mid-logic: sound_notify_message gains a Sound::Reminder arm (never sent)
src/app/actions.rs  mid-logic: the client notification kind match treats Sound::Reminder like Done
src/client/shell_runtime.rs  mid-logic: the action loop plays ClientShellAction::PreviewSound
src/client/shell/mouse.rs  mid-logic: handle_mouse (the News row: a left press on hits.news_row focuses its tab through tab.focus before the sidebar tab press is taken, and a right-click opens the News menu before the tab-row lookup; sidebar tab drag, group menu, locked-pane gestures, notification card hits: timed left-click keeps the upstream pane_id gate, sticky left focuses / right dismisses / the fold line swallows), push_pane_mouse_event; the ContextMenu block asks route_tab_color_swatch_mouse, then route_tab_remind_option_mouse first (swatch / reminder option hover and click); a settings choice click also applies at once when reminders_click_applies (the reminders tab's daily time row and picker) or news_click_applies (every row of the news tab)
src/client/shell/surface_patch.rs  mid-logic: fast_path_blocker else-if for suspended panes; the notification arm calls notification_blocks_patch (timed: any card blocks, as upstream; sticky: only patch rows over a drawn card)
src/client/shell/composition.rs  mid-logic: both ShellRenderState literals pass news_row (computed by news_row() before the mutable borrows); compose paints the suspended card and occludes graphics; compose draws the sticky notification stack (render_notification_stack), occludes every card rect, fills hits.notification_toasts, and hands the stack bounds to copy_feedback_offset_for_toast; the context menu branch copies rendered.menu_swatches and menu_remind_options into the hit map; both ShellRenderState literals pass idle_reminders and scheduled_reminders; render_client_overlay gets &self.config
src/client/shell/render.rs  mid-logic: render_shell else-if for the tabs layout
src/client/shell/config.rs  mid-logic: layout (show_tab_bar), from_config, apply_live_config (sound_files, daily_reminder_minutes), reload_client_config (rebalance_notification_cards after a sticky flip)
src/client/shell/notification_policy.rs  mid-logic: retire_endpoint_notifications, queue_visible_notification (sticky push), promote_queued_notification, focus_visible_notification (split into focus_notification_at), receive_notification (replace-by-pane moved into replace_pane_notifications), tick_notifications (starts with tick_idle_reminders, whose due reminders it delivers from the pending list; a pending reminder's sound becomes Sound::Reminder; a validated pending event is passed through format_agent_notification before the target/sound/delivery code, so every path gets the `tabs` layout text; expiry gated on !toast_sticky); notification_validation is reused by sticky_notification_is_stale; notification_target_is_active is the idle reminders' focus test
src/client/shell/endpoints.rs  mid-logic: cache_endpoint_snapshot_with_surface ends with prune_sticky_notifications
src/client/shell/machine_diagnostics.rs  mid-logic: handle_machine_badge_event also yields to hits.notification_toasts
src/client/shell/state.rs  mid-logic: ClientShellState::new initialises visible_notifications, the reminder maps and news; timer_delay chains next_idle_reminder_deadline then next_news_deadline (the running row's minute clock)
src/config.rs  mid-logic: Config::collect_diagnostics chains tab_agent_glyph_color_diagnostics and HerdrToastConfig::diagnostic
src/client/shell/tests/graphics.rs  depends: assert_graphics_cover is pub(super) for tests/sticky_notifications.rs; its ClientContextMenuTarget::Tab literal carries `color: Default::default()` and its ClientSettingsOverlay literal `news: Box::default()` (section 2)
src/client/shell/actions.rs  mid-logic: record_binding (topology lock, group keys), endpoint_method_for_action (SwitchTab/NextTab scope, CycleTabColor, ToggleTabImportant, OpenNews -> news.open)
src/client/shell/context_menu.rs  mid-logic: items (a News target lists Run now, Open, Pause / Resume schedule; after Close, so upstream item indices hold: the important toggle, the two reminder selector rows, then the swatch row `Color` last), activate_context_menu_item's target match sends News to activate_news_context_action, open_tab_context_menu (captures the tab color, important and remind_every into the Tab target), activate_context_menu_item (the swatch row, Important and the selector act before the target dispatch, so no tab focus; the Tab arm ignores `color`, `important` and `remind` with `..`)
src/client/shell/overlay_input.rs  mid-logic: save_rename_overlay, accept_close_confirmation (close_group now from the overlay); the ContextMenu key block asks route_tab_remind_menu_key, then route_tab_color_menu_key first (Up/Down move between and re-seat the selector and swatch cursors, Left/Right/h/l on either row)
src/client/shell/overlays.rs  mid-logic: render_context_menu widens a tab menu to the swatch row and the reminder selector, draws the swatches and the selector's options in place of their items' labels (only the cursor one highlighted, while its row is) and returns their rects as menu_swatches / menu_remind_options; render_client_overlay passes the config to the settings overlay
src/client/shell/tabs.rs  mid-logic: render_tab_bar tints unfocused tabs with their color tag (focused tab unchanged)
src/app/api/tabs.rs  mid-logic: handle_tab_close captures closed_session_entries_for_workspaces (last tab) or closed_session_entries_for_tab before the close and calls record_closed_sessions after it, in both branches; a new upstream close path records nothing
src/app/api/panes.rs  mid-logic: close_pane captures closed_session_entries_for_workspaces (the pane closes its space) or closed_session_entries_for_pane before ws.close_pane and records after remove_plugin_pane_records; handle_pane_move records nothing (a moved tab is not closed)
src/app/api/workspaces.rs  mid-logic: handle_workspace_close captures closed_session_entries_for_workspaces(close_indices) before close_selected_workspace and records after shutdown_detached_terminal_runtimes
src/app/api/panes.rs  mid-logic: handle_pane_move (PaneMoveRecoveryContext.previous_tab_color, previous_tab_important and previous_tab_remind_every; a whole-tab move applies them to the NewTab / NewWorkspace tab), recover_failed_pane_move restores them
src/client/shell/settings.rs  mid-logic: selected_index_for_settings_section (News 0), select_settings_section (refreshes idle_reminder_minutes, closes a sound picker and a daily time picker, requests session.closed_list on entering `closed`, enter_news_section on entering `news`), settings_choice_count (Sound counts sound_section_rows, Reminders counts reminders_section_rows, ClosedSessions the filtered rows, News news_section_rows), apply_settings_choice (Sound goes to apply_sound_choice, Reminders asks apply_daily_time_choice first, then writes ConfigEdit::IdleReminderMinutes, ClosedSessions reopens, News goes to apply_news_choice), route_settings_key (Esc closes an open sound, daily time or news picker first, then route_closed_sessions_key takes filter text, Backspace, Delete / `d` and Esc on a non-empty filter; Up/Down preview its sound), handle_settings_endpoint_result
src/client/shell/settings_overlay.rs  mid-logic: render_settings_overlay (show_primary match, ClosedSessions shows ` ↵ reopen ` while rows are listed; the section tab strip drops its one-cell gaps when it does not fit the 74-column inner width, and then drops each label's padding (settings_tab_label) and keeps the gaps, which with eight sections is always; Sound renders render_sound_section (rows or the open picker); Reminders arm: render_reminders draws the daily time row below the intervals, or the open daily time picker; ClosedSessions renders settings_closed::render_closed_sessions; News renders settings_news::render_news_section and shows the primary button only once a record is listed)
src/client/shell/actions.rs  mid-logic: push_endpoint_method_with_kind treats SessionClosedReopen and NewsOpen as focus-changing; handle_endpoint_result sends the SessionClosed* kinds to handle_closed_sessions_endpoint_result and the News* kinds to handle_news_endpoint_result
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists the SessionClosed* and News* kinds
src/client/shell/endpoint_navigation.rs  mid-logic: finish_endpoint_workspace_press early return in the tabs layout
src/client/mod.rs  mid-logic: the client loop's tick block calls shell.tick_news(now, &mut outcome) directly after tick_notifications (the only non-input producer of endpoint requests: news.get is pulled from there)
src/protocol/wire.rs  depends: ClientShellSnapshot.tab_bar_right and tab_bar_right_separator feed the tabs sidebar footer (status_footer_lines and render_tab_status_footer in src/client/shell/tab_sidebar.rs), which joins the segment texts with the separator, splits on `\n` (a command entry's `lines`), parses kept SGR (`ansi`) and truncates each row with `…`; ClientShellTabStatusSegment.text stays a plain String, so multi-line and SGR text needs no wire change
src/app/tab_bar_status.rs  mid-logic: configure_tab_bar_status (Command arm reads lines/ansi into TabBarCommandRuntime.format), handle_tab_bar_status_tasks (spawns with the format), spawn_status_command (test-only wrapper), run_status_command (reads through output::read_status_output_lines and builds the text with output::status_output_text, which hand off to upstream's read_last_output_line and command_output_text for the defaults); output.rs reuses command_output_text, read_last_output_line, is_unicode_format_control, MAX_COMMAND_LINE_BYTES and MAX_STATUS_TEXT_CHARS and mirrors strip_terminal_control_sequences (an upstream change to that state machine must be mirrored in strip_control_sequences_keeping_sgr)
src/config/tab_bar.rs  mid-logic: tab_bar_right_diagnostics (the lines clamp warning in the Command arm)
src/client/shell/tabs.rs  mid-logic: tab_bar_status_width and render_tab_bar_status measure and draw tab_sidebar::single_line_status_text(segment.text) (last line, escapes stripped) instead of the raw text
src/client/shell/state.rs  mid-logic: apply_active_snapshot's tab_layout_changed also compares tab_sidebar::status_footer_lines counts (a footer line count change resizes the tabs sidebar list)
src/server/client_shell.rs  depends: copies agent_status from app.session_snapshot() into the snapshot agents, tabs and workspaces; ClientShellTab.color, .important and .remind_every come from the zipped Tab state; ClientShellAgent.subagents from AgentInfo.subagents
src/integration/assets/claude/herdr-agent-state.sh  depends: forwards Claude's transcript_path as agent_session_path (transcript backup store); the fork's `subagent` and `stop` actions send pane.report_subagent
src/integration/assets/claude/herdr-agent-state.ps1  depends: the transcript path as the .sh asset, on Windows; no `subagent` or `stop` action (install skips those hooks on Windows); its version marker follows the .sh (11)
src/integration/claude_settings.rs  depends: install/uninstall run before the fork's claude_subagent_hooks, which re-parses their output; HOOK_REMOVALS must not remove the `subagent` or `stop` actions (upstream's Stop removal is the `idle` action)
src/api/schema/panes.rs  depends: PaneMoveParams/PaneMoveDestination behind the fork's pane.move digest; PaneReportAgentSessionParams.agent_session_path
src/persist/io.rs  depends: session.json load path for suspended_agent and transcript_path
src/handoff_runtime.rs  depends: HandoffRuntimeState serde carries suspended_exit_pending

## 9. Identifier watch-list (any hit in the incoming upstream diff = deny "upstream collision")
agent.suspend
agent.activate
agent.transcripts
AgentSuspend
AgentActivate
AgentTranscripts
AgentTranscriptBackupPass
Suspended
"suspended"
suspended_agent
suspended_exit_pending
SuspendedAgent
agent_suspended
agent_not_suspendable
graceful_exit_input
transcript_path:
transcript_path_from_report
agent-transcripts
backup_agent_transcripts
SidebarLayoutConfig
tab_agent_glyph
toggle_agent_suspend
move_tab_to_group
toggle_groups_folded
ToggleAgentSuspend
MoveTabToGroup
ToggleGroupsFolded
agent.restart
AgentRestart
RestartAgent
restart_agent
resume_pending
"agent_working"
agent_status_priority
terminal_agent_status
PaneAttention
max_stack
notification_toasts
visible_notifications
toast_sticky
render_notification_stack
prune_sticky_notifications
sticky_notification_is_stale
focus_notification_at
dismiss_notification_at
rebalance_notification_cards
primary_notification_index
notification_blocks_patch
tab.set_color
TabSetColor
TabColor
cycle_tab_color
CycleTabColor
ClientTabMenuColor
context_menu_swatches
menu_swatches
route_tab_color_menu_key
route_tab_color_swatch_mouse
previous_tab_color
invalid_tab_color
tab.set_remind
TabSetRemind
tab.set_reminder
TabSetReminder
TabRemindInterval
TabRemindEvery
remind_every
toggle_tab_important
ToggleTabImportant
toggle_tab_remind
idle_reminder
IdleReminder
scheduled_reminders
daily_reminder_time
reminder_path
ReminderBase
PreviewSound
ClientReminderKind
notification_glyph
put_notification_glyph
DailyReminderTime
daily_time_picker
settings_daily_time
reminder_daily_minutes
format_time_of_day
SoundFile
settings_sounds
tab_remind_menu
previous_tab_remind
previous_tab_important
replace_pane_notifications
pane.report_subagent
PaneReportSubagent
SubagentEvent
subagents:
active_subagents
active_subagent_count
record_subagent
SUBAGENT_LIMIT
claude_subagent_hooks
format_agent_notification
agent_notification_body
render_tab_status_footer
effective_tab_bar_command_lines
MAX_TAB_BAR_COMMAND_LINES
StatusOutputFormat
spawn_status_command_with_format
read_status_output_lines
strip_control_sequences_keeping_sgr
status_footer_lines
single_line_status_text
session.closed_list
session.closed_reopen
session.closed_remove
SessionClosedList
SessionClosedReopen
SessionClosedRemove
ClosedSessionInfo
ClosedSessionTarget
ClosedSessions
ClientClosedSessions
closed_sessions
closed-sessions.json
closed_session_not_found
closed_session_not_resumable
compact_closed_age
settings_closed
tab_closed::
subagent_snapshot_seen
replace_subagents
forget_subagents
subagent_ids
agent_subagents_running
SubagentsRunning
shows_subagents
SUBAGENT_HOOKS
remind_marker
TAB_REMIND_HOURS_MARKER
TAB_REMIND_DAILY_MARKER
news
NewsState
NewsConfig
NewsRun
NewsStatus
news.run
news.status
news.get
news.open
news.history
news.set_enabled
NewsGet
NewsOpen
NewsHistory
NewsSetEnabled
NewsEnabled
news_row
tick_news
NewsRow
ClientNewsState
NewsToggleSchedule
open_news_context_menu
open_news
OpenNews
settings_news
ClientNewsSettings
news_click_applies
news.json
herdr:news
news_assets
handle_news_tasks
next_news_deadline
quiet_hours

## 10. Fork smoke tests (run by name in the gate)
server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot
server::headless::tests::fork_smoke::session_restore_keeps_a_suspended_pane_parked
server::headless::tests::fork_smoke::claude_hook_asset_reports_the_transcript_path_the_backup_store_uses
server::headless::tests::fork_smoke::claude_subagent_hooks_reach_the_client_shell_snapshot
client::shell::tests::tab_sidebar::fork_smoke::locked_suspended_pane_emits_no_pane_input
client::shell::tests::settings_backups::fork_smoke::every_settings_section_fits_the_76_column_popup
client::shell::tests::sticky_notifications::fork_smoke::three_cards_stack_newest_nearest_the_corner
client::shell::tests::tab_sidebar::fork_smoke::colored_tab_label_reaches_the_renderer_in_its_color
client::shell::tests::idle_reminders::fork_smoke::marked_done_tab_reminds_after_the_interval
client::shell::tests::tab_sidebar::fork_smoke::tab_bar_status_reaches_the_tabs_sidebar_footer
client::shell::tests::tab_sidebar::fork_smoke::colored_multi_line_status_reaches_the_footer_in_color
server::headless::tests::fork_smoke::closed_agent_tab_reopens_from_the_list_with_the_same_session
server::headless::tests::fork_smoke::news_status_and_run_reach_the_news_tab

## 11. Fork changelog (moved out of docs/next/CHANGELOG.md)
### Fixed
- Tab groups: moves refuse multi-pane tabs and the bucket's last tab instead of tearing a tab apart or silently demoting a group; a jittered click on a tab row still focuses it; cross-group drop indicators sit where the tab will land; the focused group's header click and the fold toggle agree with what is drawn; the move-to-group prompt ignores the current group, treats the bucket's name as "ungroup", skips linked worktrees and matches exact names first; group keys are inert outside the `tabs` layout. Transcript store: reported paths are restricted to `<id>.jsonl` under a `projects` directory, restores never replace a native file, shrinking transcripts keep the previous copy, session.json is written before the shutdown backup pass, a stale reported path falls back to the glob, and stale temp files are cleaned.
- Suspend refuses agents that are blocked on a prompt, prompts and send-keys refuse suspended panes, activation waits for the observed exit, a failed process probe retries instead of ending the exit wait, and dropping a suspended record emits a status event. The tabs-layout input lock now also covers mouse gestures and selections on a suspended pane, the card occludes graphics, and `ui.tab_agent_glyphs` honours an `other` override.

### Changed
- Background Claude Code subagents stay counted after the main turn ends: the Claude integration (v11) adds a `Stop` hook that reports every subagent still running from Claude's `background_tasks` (`pane.report_subagent` `snapshot`, which replaces the pane's set), the set survives idle and finished turns and a transient unknown state (cleared on exit, another agent, a new conversation, suspend or release; an older Claude Code without the field keeps the old clear-on-idle), and Claude's internal helper agents (no agent type) are no longer reported. The `tabs` sidebar shows `⚭` for a working, idle or finished agent with subagents running, in its status color (blocked keeps `×`), and `agent.suspend` / `agent.restart` (so the menu items and keys too) refuse such an agent with `agent_subagents_running`, since exiting would stop them. Agent status, finished notifications, important reminders and the sort order are unchanged.
- The tab menu's important toggle reads `Important`, left-aligned like the other items, with `✓` after the label (`Important ✓`) when the tab is important; the menu keeps its width.
- The settings overlay's `reminders` tab edits `ui.daily_reminder_time`: a `daily at HH:MM` row below the intervals opens a list of 24-hour times in 30-minute steps (a configured time off the grid shows first as `custom: HH:MM`), Enter writes it and Esc goes back. The running client applies it at once: a new time still ahead today fires today, one already past waits for tomorrow, and the change itself never fires a reminder.
- In-app notification cards and the mobile banner show the notification's own glyph where the `●` was, in the same color: `✓` for a finished agent, `×` for one that needs attention, `★` for an important-tab reminder and the interval's remind marker (`◷` / `◑` / `☼`) for a scheduled one (the reminder engine tags its cards; nothing is read from the title). Update and custom notices keep `●`.
- In the `tabs` sidebar layout, agent notifications name the tab instead of the agent and drop the space number: "level plan finished" with the body "claude · leap-bi-4" (agent, then the basename of the pane's directory, else the space's name) instead of "claude finished" / "leap-bi-4 · 1 · level plan". The client rewrites them once as they leave its pending list, so in-app cards, the mobile banner and terminal/system notifications agree; idle reminder bodies use the same "agent · directory" form in both layouts. A tab the client cannot resolve and the `spaces` layout keep the server's text.
- `agent.suspend` (and so `herdr agent suspend`, the "Suspend agent" menu item and `keys.toggle_agent_suspend`) refuses a `working` agent with `agent_working`, the same guard `agent.restart` uses, and sends it no input.

### Added
- AI news desk, phase 3 (the News tab becomes native): new socket methods `news.get` (the schedule, the News tab while it exists, the run in flight, the last run and the unread mark), `news.history` (the editions index), `news.open` (focuses the News tab, creating it in the first space with the page viewer when it is gone; `--edition N` quits whatever holds the pane with `q` and reopens the viewer on that edition once the shell prompt is back, refused while a run is in flight) and `news.set_enabled` (writes `news.enabled` to the config file and reloads it); CLI `herdr news open [--edition N] | history [--days N] [--json] | enable | disable`. Focusing the News tab clears its important (unread) mark on the server, so it works from any client. In the `tabs` sidebar layout the News tab leaves the scrolling list for a pinned row between the list and the status footer: a state glyph, `News` and `running 2m` (a run in flight, ticking by the minute), `unread` (the tab is important), `failed` (the last run did not succeed), `paused` (scheduling off) or the last run's local time; a left click focuses the tab, a right-click offers Run now, Open and Pause / Resume schedule. The row exists only while the News tab does (closing the tab is allowed; the next run or `news.open` recreates it) and the `spaces` layout keeps the tab in its list. `keys.open_news` (unset by default) focuses the News tab from either layout, creating it with the viewer when it is gone. The settings overlay's new `news` tab (last) shows the desk from `news.get`: `scheduled runs: on|off` toggles `news.enabled`, `every N h` picks 3 / 6 / 12 / 24 hours, `quiet hours …` picks off or a window (a configured value off the list shows first), `run now` starts a run, and the model, the last run and the next run are listed; config rows are written like the other settings (config write + `server.reload_config`) and the tab pulls `news.get` again. The settings tab strip now holds nine tabs. The client fills it from `news.get` on the tick after the first snapshot of a connection, whenever the tab set or the News tab's status or mark changes, after every `news.*` reply and otherwise once a minute (a `herdr news enable` from a shell changes no snapshot), and never asks a server that does not advertise the method. No wire change.
- AI news desk, phase 2 (server-owned runs): `herdr news run` (`news.run`) installs the bundled runner (`news_run.py`, `anchors.py`, `viewer.py`, `system.md`, `topic.md`, `sources.json`, embedded in the binary) under `<session data dir>/news/bin/`, keeps one `News` tab in the first space (created unfocused, reused while it keeps its label), quits the page viewer with `q` when it is showing, and types `python3 <home>/bin/news_run.py --home <home> --trigger manual|scheduled [--model M]` into its shell; a run is in flight until `runs/index.jsonl` gains its record (polled every 5 s) or a 65-minute watchdog interrupts it (Ctrl-C, agent released, a `timeout` record), and a finished run that changed the page marks the tab important. `[news] enabled = true` (default false) schedules runs every `interval_hours` (default 6) outside `quiet_hours` (default `00:00-08:00` local, deferred to their end, missed slots collapsed to one run, a manual run restarts the interval); `model` is handed to the runner. `herdr news status [--json]` prints the schedule, home, tab, next run, run in flight and the last five runs (`news.status`), `herdr news log [N] [--json]` the newest runs from the run log. The schedule, tab and run in flight persist in `news.json` next to `session.json`. The runner is the pane's only reporter and reports as agent `news` from source `herdr:news` (a plain hook source with a label no screen manifest owns, so the tab shows working / done without a detected process; a `claude` label would stay with screen detection, which never sees the `claude -p` child; no session id, so nothing is persisted or resumed on restart): it starts the editor without herdr's pane identity so the shared Claude hook stays silent (its `herdr:claude` session claim would make the server drop the runner's reports as a conflicting owner), and the server clears the News pane's stale agent identity (a session or hook authority from an interactive claude, the recent-exit marker) when it types the command. No wire change.
- Recently closed agent sessions: closing a tab, pane or space whose pane holds a live or suspended agent session (not a plain shell, not a tab moved to another group, not at server shutdown) records one entry per session in `closed-sessions.json` next to `session.json` (newest 100: agent and session reference, tab label, color and reminders, group, directory, close time). `herdr tab closed [--json]` lists them numbered, `herdr tab reopen <n|id|session-id-prefix>` reopens one, and the settings overlay's new `closed` tab lists `label · group · dir · 2h ago` with type-to-filter, Enter to reopen and Delete (or `d`) to remove. Reopening opens a tab in the original group (the first space when it is gone) with the same label, color and reminders, puts a deleted transcript back from the backup store, and types the native resume command; the entry is then removed. New socket methods `session.closed_list`, `session.closed_reopen`, `session.closed_remove`; no protocol change. The settings tab strip drops the labels' padding to fit the eighth tab.
- `ui.tab_bar_right` command entries accept `lines` (1–4, default 1, clamped with a config warning) to keep the last non-empty output lines, joined with `\n` in the segment, and `ansi` (default false) to keep SGR color and weight sequences (`ESC [ digits;… m`) while every other escape and control character is still stripped. The `tabs` sidebar footer draws one row per line (at most four in total, the list shrinks to match and scrolling still reaches the last tab), styles reset (0), bold (1), dim (2), normal intensity (22), foreground 30–37 / 90–97, `38;5;n`, `38;2;r;g;b` and the default foreground (39) over its dim base, and truncates each row with `…`. The `spaces` tab bar shows only a segment's last line without escapes. No wire change: the segment stays a string and the protocol is unchanged; with the defaults the text is byte-for-byte upstream's.
- In the `tabs` sidebar layout, where the horizontal tab bar is hidden, the `ui.tab_bar_right` status area (command, hostname, datetime and text entries) shows as a one-line dim footer under the tab list, joined with `ui.tab_bar_right_separator` and truncated with `…` when too wide. It takes one row off the list only while some status text exists; scrolling still reaches the last tab. The `spaces` layout is unchanged.
- Running Claude Code subagents show on the tab: `herdr integration install claude` adds `SubagentStart` and `SubagentStop` hooks (not on Windows) whose `subagent` action reports each subagent to the new `pane.report_subagent` method, the server keeps the running set per pane (at most 64, never persisted, forgotten when the agent stops working, is suspended, released or replaced), agent records carry an optional `subagents` count while the agent works, and the `tabs` sidebar layout shows `⚭` in the working color as a working tab's status icon while its agent has subagents running. Part of protocol 24.
- Tab reminders, two per tab, set from the tab menu without focusing the tab (both persist in session.json, follow a tab moved to another group, and appear on tab records as optional `important` and `remind_every`; `tab.set_reminder` sets either alone, `tab.set_remind` stays as the boolean form of important; the client wire changed, so the protocol is now 25). **Important** (the `Important` item (`Important ✓` when on), `keys.toggle_tab_important` (unset by default; `toggle_tab_remind` still accepted) or `herdr tab important <tab_id> <on|off>`): while its agent sits finished and unseen, or blocked, the client reminds you after `ui.idle_reminder_minutes` (default 10, 0 off, at most 240 with a config warning; the settings overlay's `reminders` tab picks off, 5, 10, 15, 30 or 60 minutes) and every as many minutes until the tab is focused, the agent works again or the mark is removed ("planner finished 20 min ago", "planner still waiting"). **Remind** (a two-row selector `remind  5m  10m  30m` / `1h  6h  daily`, the current one bracketed and picked again for off, or `herdr tab remind <tab_id> <off|5m|10m|30m|1h|6h|daily>`): reminds you on that schedule whatever the agent does ("planner reminder", body agent · directory, or the directory for a plain shell), daily at `ui.daily_reminder_time` (default "09:30" local, validated with a config warning); a firing due while the tab is focused is skipped and restarts the interval, a daily one missed while no client was attached fires once when the client next sees the tab that day, interval ones resume without catching up. The `tabs` sidebar layout shows `★` for important and the remind marker (`◷` minutes, `◑` hours, `☼` daily; `remind_marker`) for remind before the agent glyph, overlay0 until a reminder fires, then lit (finished teal / blocked red for `★`, accent for `◷`) until it clears. Each tab shows one reminder card, which replaces its previous one; reminders follow `ui.toast.delivery` and play the new reminder sound, `ui.sound.reminder_path` (else the done or request sound), unless sounds are off. The settings overlay's `sound` tab gains pickers for the finished, needs-input and reminder sounds from `default` and the macOS system sounds (moving previews each once; Enter writes `ui.sound.done_path`, `request_path` or `reminder_path`, `default` removes it), and aiff, aif, caf and m4a sound files are accepted on macOS.
- Tab color tags: `tab.set_color` (and `herdr tab color <tab_id> <color|none>`) tags a tab with red, orange, yellow, green, cyan, blue or purple (fixed true colors, not theme slots, because 16-color themes such as `terminal` remap the ANSI slots), or clears it. The `tabs` sidebar layout draws the tab's name in its color on focused and unfocused rows (status icon, agent glyph, row highlight and bold unchanged), and the `spaces` tab bar tints unfocused tab names. The tab menu ends with a row of swatches, `∅` plus red, yellow, green and purple (`TabColor::OFFERED`; the other API colors still render): the current color is bracketed, Up/Down reach the row like any item and start on the current color, Left/Right or `h`/`l` move along it, Enter or a click sets the color and closes the menu, and `keys.cycle_tab_color` (unset by default) cycles the focused tab none → red → yellow → green → purple → none. The color persists in session.json, follows a tab moved to another group, and appears as an optional `color` on tab records.
- `ui.toast.herdr.sticky = true` keeps in-app toasts until they are handled: cards stack from `ui.toast.herdr.position` (newest nearest the corner, per-event positions stack in their own corner), and beyond `ui.toast.herdr.max_stack` cards (default 6, 1 through 20, clamped with a config warning) or half the frame height the older ones fold into a "+N more" line. Left-click focuses a card's pane and removes it, right-click dismisses it, `open_notification_target` focuses the newest card, and a card clears when its tab becomes focused, its pane closes, a needs-input agent is no longer blocked, a finished agent starts working again, a newer notification for the same pane arrives, or its machine's server restarts. Sticky cards only block the retained fast path for pane rows they cover. The default timed toast is unchanged.
- `ui.sidebar_layout = "tabs"` lists one row per tab across every space, in tab order, with each tab's agent status. Rows focus on click and open the tab menu on right-click; the horizontal tab bar is dropped and `next_tab`/`previous_tab` cycle the whole list. The default `"spaces"` layout is unchanged.
- Park a running Claude Code agent with `herdr agent suspend <target>` (`agent.suspend`): Herdr submits its exit command, keeps the pane's native session reference and agent name, and reports the new `suspended` status. `herdr agent activate <target>` (`agent.activate`) relaunches it in the same pane with the native resume command. Suspended panes survive server restarts as suspended and are never relaunched automatically.
- Tab groups in the `tabs` sidebar layout: spaces after the first render as collapsible groups with header rows; fold per group (header click) or all at once (one toolbar toggle: ⏶ folds, ⏷ expands once everything is folded; `keys.toggle_groups_folded` is the keyboard form), move the focused tab to a group by name with `keys.move_tab_to_group` or the `+` button (a new name creates the group), reorder groups by dragging headers, drag tabs within or across groups, and rename / ungroup / close a group from its header menu.
- The tab context menu offers "Suspend agent" / "Activate agent" for the tab's agent, and the new `keys.toggle_agent_suspend` binding (unset by default) suspends the focused pane's agent or activates it again.
- In the `tabs` sidebar layout a suspended pane shows a card (status, agent, directory, the activate binding) instead of its shell, and swallows all pane input until the agent is activated. The layout also keeps one pane per tab: split, swap, zoom, resize and pane-focus actions are inert, and right-clicking the pane opens the tab menu. `switch_tab` (for example `alt+1..9`) indexes the whole list in this layout, and shell output in a suspended pane always goes through a full compose so the card is never overdrawn. Tab rows show a right-aligned agent glyph (`ui.tab_agent_glyphs`; default ⧆ claude, ⧇ codex, ⍾ other agents, ⧅ plain shell).
- The focused row's agent glyph in the `tabs` sidebar layout wears the agent's brand color (`ui.tab_agent_glyph_colors`; default claude `#D97757`, codex `#3B82F6`; other agents and plain shells stay monochrome, as do unfocused rows). An invalid color keeps the glyph monochrome and reports a config diagnostic.
- Restart an idle agent in place with `herdr agent restart <target>` (`agent.restart`), the tab menu's "Restart agent" item, or the new `keys.restart_agent` binding (unset by default): Herdr suspends it (graceful exit, `suspended` in between) and relaunches it in the same pane with the native resume command as soon as the exit is observed and the shell prompt is back, without a second request. Working agents are refused with `agent_working`, blocked ones with `agent_blocked`, suspended ones with `agent_suspended`; a restart whose exit needed `SIGKILL` or whose shell prompt does not return within 30 seconds leaves the pane suspended with a warning, and a restart in flight never survives a server restart.
- Herdr backs up the native conversation transcript behind every open or suspended Claude Code pane under the session directory (`agent-transcripts/<agent>/<session-id>/`) every five minutes, on suspend, and on shutdown, and puts the copy back before a native resume when Claude has deleted its own transcript, so `claude --resume` no longer fails with "No conversation found" after Claude's cleanup period. `session.backup_agent_transcripts = false` stops new backups; `herdr agent transcripts` lists the store, and the settings overlay's `backups` tab shows its size, session count, the last backup pass and the next scheduled one (`agent.transcripts` over the socket API). Herdr never deletes a backup.

## 12. Install rule
Compare PROTOCOL_VERSION (src/protocol/wire.rs) and ENDPOINT_PROTOCOL_GENERATION (src/protocol/endpoint.rs)
between ~/.local/state/herdr-fork-sync/installed-sha and the merge. Changed: stage as ~/.local/bin/herdr.next, never install.
Unchanged: cp .new, codesign -s - -f, keep .prev, mv -f. Never restart the server.
Any fork change that adds a field or enum variant to a bincode-encoded wire type (ServerMessage, ClientShellSnapshot and everything it contains) must bump PROTOCOL_VERSION, so a new client refuses an old server with "Stop the old server" instead of misdecoding.
