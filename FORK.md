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
scripts/test_browser_companion.mjs
scripts/test_browser_overlay.mjs
scripts/test_fork_sync.py
scripts/test_news_run.py
scripts/test_news_viewer.py
src/api/schema/closed_sessions.rs
src/api/schema/agent_messages.rs
src/api/schema/news.rs
src/app/agent_suspend.rs
src/app/agent_transcripts.rs
src/app/closed_sessions.rs
src/app/message_queue.rs
src/app/message_queue/tests.rs
src/app/news.rs
src/app/subagents.rs
src/app/tab_bar_status/output.rs
src/app/tab_color.rs
src/app/tab_remind.rs
src/cli/news.rs
src/cli/tab_closed.rs
src/client/shell/breathe.rs
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
src/client/shell/tests/breathe.rs
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
src/server/headless/news_notify.rs
src/server/headless/tests/fork_smoke.rs
src/api/schema/browser.rs
src/app/browser.rs
src/browser/activity.rs
src/browser/brand.rs
src/browser/host.rs
src/browser/hub.rs
src/browser/ntp.rs
src/browser/launch.rs
src/browser/mod.rs
src/browser/node.rs
src/browser/profiles.rs
src/browser/serve.rs
src/browser/setup.rs
src/browser/shape.rs
src/browser/shots.rs
src/browser/state.rs
src/browser/tests.rs
src/cli/browser.rs
src/cli/browser_mcp.rs
src/client/shell/browser.rs
src/client/shell/browser_overlay.rs
src/client/shell/settings_browser.rs
src/client/shell/tests/browser.rs
src/client/shell/tests/settings_browser.rs
src/config/browser.rs
src/integration/assets/browser/activity.mjs
src/integration/assets/browser/branding/herdr-plus-browser-icon.png
src/integration/assets/browser/companion/companion.js
src/integration/assets/browser/companion/dashboard.html
src/integration/assets/browser/companion/manifest.json
src/integration/assets/browser/companion/newtab.html
src/integration/assets/browser/companion/newtab.js
src/integration/assets/browser/companion/sw.js
src/integration/assets/browser/extract.mjs
src/integration/assets/browser/host.mjs
src/integration/assets/browser/package-lock.json
src/integration/assets/browser/package.json
src/integration/assets/browser/smoke.mjs
src/integration/browser_assets.rs
src/persist/browser.rs
src/api/schema/coordinator.rs
src/cli/coordinator.rs
src/coordinator/api.rs
src/coordinator/assets/coordinator.md
src/coordinator/assets/dashboard.html
src/coordinator/assets/icons/agent.png
src/coordinator/assets/icons/coordinator.png
src/coordinator/assets/icons/empty.png
src/coordinator/assets/icons/favicon.png
src/coordinator/launch.rs
src/coordinator/live.rs
src/coordinator/lock.rs
src/coordinator/mcp.rs
src/coordinator/messages.rs
src/coordinator/mod.rs
src/coordinator/registry.rs
src/coordinator/serve.rs
src/coordinator/turn.rs
src/coordinator/watch.rs
src/app/coordinator.rs
src/client/shell/coordinator.rs
src/client/shell/coordinator_shell.rs
src/client/shell/settings_coordinator.rs
src/client/shell/tests/coordinator.rs
src/config/coordinator.rs
src/coordinator/engine.rs
src/persist/coordinator.rs
src/server/headless/coordinator_notify.rs
src/server/headless/tests/fork_smoke/coordinator.rs
src/server/headless/tests/fork_smoke/stub_claude.sh
src/agent_wrap/mod.rs
src/agent_wrap/instructions.rs
src/agent_wrap/tests.rs
src/config/agents.rs
src/cli/agent_wrap.rs
src/cli/agent_notify.rs
src/app/agent_notices.rs
src/app/agent_wrap_settings.rs
src/api/schema/agent_notices.rs
src/api/schema/agent_wrap.rs
src/server/headless/agent_notices.rs
src/server/headless/tests/agent_notices_smoke.rs
src/client/shell/agent_cards.rs
src/client/shell/settings_agents.rs
src/client/shell/tests/agent_cards.rs
src/client/shell/tests/settings_agents.rs
src/workspace/team.rs
src/app/team.rs
src/api/schema/team.rs
src/server/headless/teams.rs
src/server/headless/tests/teams_smoke.rs
src/cli/team.rs
src/agent_wrap/team.rs
src/client/shell/teams.rs
src/client/shell/team_overlay.rs
src/client/shell/tests/teams.rs
src/client/shell/tests/team_overlay.rs
src/api/schema/notes.rs
src/notes/mod.rs
src/notes/bounded.rs
src/notes/checkpoints.rs
src/notes/transcript.rs
src/notes/worker.rs
src/notes/tests.rs
src/app/notes.rs
src/cli/notes.rs
src/config/notes.rs
src/client/shell/info_dock.rs
src/client/shell/info_dock_model.rs
src/client/shell/info_dock_render.rs
src/client/shell/tests/info_dock.rs
src/server/headless/tests/fork_smoke/notes.rs
src/app/typing_guard.rs
src/client/shell/context_menu_session.rs
src/client/shell/tests/context_menu_session.rs
src/agents_model/mod.rs
src/agents_model/actions_log.rs
src/agents_model/envelope.rs
src/agents_model/limits.rs
src/agents_model/policy.rs
src/agents_model/reply_index.rs
src/agents_model/turn.rs
src/api/schema/agents_model.rs
src/app/agents_model.rs
src/app/agents_close.rs
src/app/agents_migrate.rs
src/client/shell/tests/agents_model.rs
src/server/headless/tests/agents_model_smoke.rs

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
| TerminalState | subagent_session | None |
| PaneReportSubagentParams | subagent_ids | Vec::new() |
| AgentInfo | subagents | 0 |
| ClientShellAgent | subagents | 0 |
| App | backup_agent_transcripts | config.session.backup_agent_transcripts |
| App | agent_transcript_backup_deadline | None |
| App | agent_transcript_backup_thread | None |
| App | agent_transcript_backup_last | None |
| App | agent_transcript_backup_pending | std::collections::BTreeMap::new() |
| App | message_queue | message_queue::MessageQueue::new(policy.persist_session, crate::coordinator::coordinator_dir()) |
| App | news | news::NewsState::new(&config.news, policy.persist_session, Instant::now()) |
| App | coordinator | coordinator::CoordinatorState::new(&config.coordinator, policy.persist_session) |
| AppState | coordinator_terminal_id | None |
| Config | news | crate::config::NewsConfig::default() |
| Config | browser | crate::config::BrowserConfig::default() |
| Config | coordinator | crate::config::CoordinatorConfig::default() |
| BrowserProfileInfo (fork-owned type; listed for the schema artifact) | companion | None |
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
| KeysConfig | open_browser | crate::config::BindingConfig::default() |
| KeysConfig | open_coordinator | crate::config::BindingConfig::default() |
| KeysConfigOverlay | toggle_agent_suspend | None |
| KeysConfigOverlay | move_tab_to_group | None |
| KeysConfigOverlay | toggle_groups_folded | None |
| KeysConfigOverlay | restart_agent | None |
| KeysConfigOverlay | cycle_tab_color | None |
| KeysConfigOverlay | toggle_tab_important | None |
| KeysConfigOverlay | open_news | None |
| KeysConfigOverlay | open_browser | None |
| KeysConfigOverlay | open_coordinator | None |
| Keybinds | toggle_agent_suspend | crate::config::ActionKeybinds::default() |
| Keybinds | move_tab_to_group | crate::config::ActionKeybinds::default() |
| Keybinds | toggle_groups_folded | crate::config::ActionKeybinds::default() |
| Keybinds | restart_agent | crate::config::ActionKeybinds::default() |
| Keybinds | cycle_tab_color | crate::config::ActionKeybinds::default() |
| Keybinds | toggle_tab_important | crate::config::ActionKeybinds::default() |
| Keybinds | open_news | crate::config::ActionKeybinds::default() |
| Keybinds | open_browser | crate::config::ActionKeybinds::default() |
| Keybinds | open_coordinator | crate::config::ActionKeybinds::default() |
| ClientShellConfig | sidebar_layout | crate::config::SidebarLayoutConfig::Spaces |
| ClientShellConfig | tab_agent_glyphs | std::collections::BTreeMap::new() |
| ClientShellConfig | tab_agent_glyph_colors | std::collections::BTreeMap::new() |
| ClientShellConfig | toast_sticky | false |
| ClientShellConfig | toast_max_stack | 6 |
| ClientShellConfig | idle_reminder_minutes | 10 |
| ClientShellConfig | daily_reminder_minutes | 570 |
| ClientShellConfig | sound_files | [None, None, None] |
| ClientShellConfig | system_sounds_dir | std::path::PathBuf::from("/System/Library/Sounds") |
| ClientShellConfig | browser_active_glyph_secs | 120 |
| HerdrToastConfig | sticky | false |
| HerdrToastConfig | max_stack | 6 |
| ClientShellState | suspended_pane_ids | std::collections::HashSet::new() |
| ClientShellState | idle_reminders | std::collections::HashMap::new() |
| ClientShellState | scheduled_reminders | std::collections::HashMap::new() |
| ClientShellState | reminder_epochs | std::collections::HashMap::new() |
| ClientShellState | reminder_local_time | None |
| ClientShellState | reminder_daily_minutes | None |
| ClientShellState | news | Default::default() |
| ClientShellState | browser | Default::default() |
| ClientShellState | coordinator | Default::default() |
| ClientPendingNotification | reminder | None |
| ClientVisibleNotification | reminder | None |
| ShellHitMap | sidebar_tabs | Vec::new() |
| ShellHitMap | breathing | false |
| ClientShellState | breathe_epoch | std::time::Instant::now() |
| ClientShellState | breathe_clock | None |
| ShellRenderState | breathe_phase | 0.0 |
| ShellRenderState | breathe_reset_rgb | None |
| ShellHitMap | sidebar_groups | Vec::new() |
| ShellHitMap | group_toggle_all | Rect::default() |
| ShellHitMap | group_new | Rect::default() |
| ShellHitMap | notification_toasts | Vec::new() |
| ShellHitMap | context_menu_swatches | Vec::new() |
| ShellHitMap | context_menu_remind_options | Vec::new() |
| ShellHitMap | news_row | Rect::default() |
| ShellHitMap | browser_row | Rect::default() |
| ShellHitMap | coordinator_row | Rect::default() |
| OverlayRender | menu_swatches | Vec::new() |
| OverlayRender | menu_remind_options | Vec::new() |
| ShellRenderState | sidebar_tab_drop_row | None |
| ShellRenderState | idle_reminders | &self.idle_reminders |
| ShellRenderState | scheduled_reminders | &self.scheduled_reminders |
| ShellRenderState | news_row | None |
| ShellRenderState | browser_row | None |
| ShellRenderState | browser_marked_tabs | std::collections::HashSet::new() |
| ShellRenderState | coordinator_row | None |
| ShellRenderState | coordinator_managed_tabs | None |
| ClientSettingsOverlay | transcripts | None |
| ClientSettingsOverlay | loading_transcripts | false |
| ClientSettingsOverlay | closed | Box::default() |
| ClientSettingsOverlay | idle_reminder_minutes | 10 |
| ClientSettingsOverlay | sound_picker | None |
| ClientSettingsOverlay | daily_time_picker | None |
| ClientSettingsOverlay | news | Box::default() |
| ClientSettingsOverlay | browser | Box::default() |
| ClientSettingsOverlay | coordinator | Box::default() |
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
| Config | agents | crate::config::AgentsConfig::default() |
| App | agent_notices | agent_notices::AgentNotices::new(config.agents.notices) |
| App | agents_config | config.agents.clone() |
| App | agents_setup_env | None |
| ClientConnection | shell_agent_notices_sent | None |
| ClientShellState | agent_cards | std::collections::HashMap::new() |
| ShellHitMap | agent_cards | Vec::new() |
| ClientSettingsOverlay | agents | Box::default() |
| Workspace | team | None |
| WorkspaceSnapshot | team | None |
| AppState | team_index | std::collections::HashMap::new() |
| AppState | team_count | 0 |
| AppState | teams_view_rev | 0 |
| App | team_tombstones | team::TeamTombstones::default() |
| ClientConnection | shell_teams_sent | None |
| ClientShellState | teams | HashMap::new() |
| ShellRenderState | teams | None |
| ShellHitMap | team_overlay | Vec::new() |
| ShellHitMap | team_overlay_popup | Rect::default() |
| ClientConfirmCloseOverlay | ungroup | false |
| ClientContextMenuTarget::Tab | team | None |
| ClientContextMenuTarget::Group | team | None |
| ClientShellLayout | info_dock | Rect::default() |
| ClientChromePreferences | info_dock_width | None |
| ClientShellState | info_dock | None |
| ClientShellState | info_dock_width | 44 |
| ClientShellState | info_dock_width_manual | false |
| ClientInputContext | info_dock_focused | false |
| ShellHitMap | info_dock | Default::default() |
| ClientShellConfig | info_pane_width | 44 |
| UiConfig | info_pane_width | 44 |
| App | notes | notes::runtime_for(&config.notes) |
| Config | notes | crate::config::NotesConfig::default() |
| KeysConfig | toggle_info_pane | crate::config::BindingConfig::default() |
| KeysConfigOverlay | toggle_info_pane | None |
| Keybinds | toggle_info_pane | crate::config::ActionKeybinds::default() |
| PaneRuntime | last_user_input | Cell::new(None) |
| AgentPromptParams | guard_user_typing | false |
| ClientContextMenuTarget::Tab | session_id | None |
| ClientContextMenuTarget::Pane | session_id | None |
| App | agents_model | agents_model::AgentsModelRuntime::new(policy.persist_session.then(crate::coordinator::coordinator_dir)) |
| AppState | agents_close_deadline | None |
| TerminalState | agent_meta | crate::agents_model::PaneAgentMeta::default() |
| TerminalState | turn | crate::agents_model::turn::TurnState::default() |
| TerminalState | created_unix | 0 |
| SuspendedAgent | suspended_by | None |
| PaneSnapshot | agent_meta | None |
| SuspendedAgentSnapshot | suspended_by | None |

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
app::actions::AppState::update_terminal_state -> pub(crate) (was private; src/app/subagents.rs calls it)
app::api::agents::App::queue_agent_prompt -> pub(in crate::app) (was private; src/app/coordinator.rs delivers wake-ups through it); agents v2: (id, params) -> (id, params, source: crate::agents_model::InputSource) (a new upstream call site = E0061: pass InputSource::Programmatic(Programmatic::Api))
client::shell::tabs::render_tab_bar(b, area, snapshot, config, tab_scroll, reveal_focused_tab, tab_drag_insert_index, hits) -> (b, area, snapshot, config, coordinator_mark: Option<(&str, String)>, tab_scroll, reveal_focused_tab, tab_drag_insert_index, hits) (the spaces layout's coordinator tab mark)
Visibility widened by the fork (upstream renaming or narrowing one breaks fork code): app::agents::DEFAULT_AGENT_START_TIMEOUT, app::agents::available_shell_name, app::api::agents::AGENT_PROMPT_SUBMIT_DELAY, app::terminal_targets::{terminal_targets, terminal_target_candidate}, integration::home_dir, client::shell::notification_policy::{notification_target_is_active, COMPLETION_EVIDENCE_GRACE} (pub(super)), app::agent_resume::shell_command_from_argv (pub(super); the closed-session reopen types it), app::api::sanitized_notification_text (pub(super); app::news sanitizes the editor's notification text with it), api::server::dispatch_to_app_with_timeout (pub(crate), re-exported as api::dispatch_to_app_with_timeout; browser::serve resolves the calling pane through it)
client::shell::ClientShellConfig::layout(cols, rows, sidebar_collapsed, tab_count, sidebar_width) -> (cols, rows, sidebar_collapsed, tab_count, sidebar_width, info_dock_width: Option<u16>) (None = no info dock; the two call sites are ClientShellState::layout, which passes info_dock_width_for_focused_tab(), and ClientShellConfig::initial_surface_size, which passes None)

## 4. Owned enum variants (append last; E0004 in upstream match = deny)
AgentStatus::Suspended   [src/api/schema/common.rs, last after Unknown; wire: JSON "suspended" in the socket API, events and the client shell snapshot, any non-human-readable serde codec encodes it as variant index 5; append-closed, deny on conflict]
Method::AgentSuspend   [src/api/schema.rs, after AgentStart, not last; wire by serde name "agent.suspend", order irrelevant]
Method::AgentActivate   [src/api/schema.rs, after AgentSuspend; wire "agent.activate"]
Method::AgentRestart   [src/api/schema.rs, after AgentActivate; wire "agent.restart"]
Method::AgentTranscripts   [src/api/schema.rs, after AgentRestart; wire "agent.transcripts"]
Method::AgentNotify   [src/api/schema.rs, after AgentTranscripts; wire "agent.notify"]
Method::AgentNotices   [src/api/schema.rs, after AgentNotify; wire "agent.notices"]
Method::AgentNoticeDismiss   [src/api/schema.rs, after AgentNotices; wire "agent.notice_dismiss"]
Method::AgentMessageSend   [src/api/schema.rs, after AgentNoticeDismiss; wire "agent.message_send"]
Method::AgentMessageClaim   [src/api/schema.rs, after AgentMessageSend; wire "agent.message_claim"]
Method::AgentsSettings   [src/api/schema.rs, after AgentMessageClaim; wire "agents.settings"]
Method::AgentsSettingsSet   [src/api/schema.rs, after AgentsSettings; wire "agents.settings.set"]
Method::AgentsFix   [src/api/schema.rs, after AgentsSettingsSet; wire "agents.fix"]
Method::TabSetColor   [src/api/schema.rs, after AgentsFix; wire "tab.set_color"]
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
Method::NewsSetEnabled   [src/api/schema.rs, after NewsOpen; wire "news.set_enabled"]
Method::NewsSetTimes   [src/api/schema.rs, after NewsSetEnabled; wire "news.set_times"]
Method::NewsSetQuietHours   [src/api/schema.rs, after NewsSetTimes; wire "news.set_quiet_hours"]
Method::BrowserRun   [src/api/schema.rs, after CoordinatorSetNotify; wire "browser.run"; the one method agents call, its BrowserOp (src/api/schema/browser.rs, tagged `op`, Unknown the serde(other) fallback) grows instead of Method]
Method::BrowserGet   [src/api/schema.rs, after BrowserRun; wire "browser.get"]
Method::BrowserStatus   [src/api/schema.rs, after BrowserGet; wire "browser.status"]
Method::BrowserFocus   [src/api/schema.rs, after BrowserStatus; wire "browser.focus"]
Method::BrowserStart   [src/api/schema.rs, after BrowserFocus; wire "browser.start"]
Method::BrowserStop   [src/api/schema.rs, after BrowserStart; wire "browser.stop"]
Method::BrowserLog   [src/api/schema.rs, after BrowserStop; wire "browser.log"]
Method::BrowserResolveCaller   [src/api/schema.rs, after BrowserLog; wire "browser.resolve_caller"; internal to the connection lane, harmless on the socket]
Method::BrowserProfiles   [src/api/schema.rs, after BrowserResolveCaller; wire "browser.profiles"]
Method::BrowserProfileCreate   [src/api/schema.rs, after BrowserProfiles; wire "browser.profile_create"]
Method::BrowserProfileDelete   [src/api/schema.rs, after BrowserProfileCreate; wire "browser.profile_delete"]
Method::CoordinatorGet   [src/api/schema.rs, after NewsSetQuietHours, before BrowserRun; wire "coordinator.get"]
Method::CoordinatorOpen   [src/api/schema.rs, after CoordinatorGet; wire "coordinator.open"]
Method::CoordinatorOpenDashboard   [src/api/schema.rs, after CoordinatorOpen; wire "coordinator.open_dashboard"]
Method::CoordinatorWake   [src/api/schema.rs, after CoordinatorOpenDashboard; wire "coordinator.wake"]
Method::CoordinatorStart   [src/api/schema.rs, after CoordinatorWake; wire "coordinator.start"]
Method::CoordinatorSetEnabled   [src/api/schema.rs, after CoordinatorStart; wire "coordinator.set_enabled"]
Method::CoordinatorSetWakeCaps   [src/api/schema.rs, after CoordinatorSetEnabled; wire "coordinator.set_wake_caps"]
Method::CoordinatorSetModel   [src/api/schema.rs, after CoordinatorSetWakeCaps; wire "coordinator.set_model"]
Method::CoordinatorSetNotify   [src/api/schema.rs, after CoordinatorSetModel, directly before BrowserRun; wire "coordinator.set_notify"]
Method::BrowserSettings   [src/api/schema.rs, after BrowserProfileDelete; wire "browser.settings"]
Method::BrowserSettingsSet   [src/api/schema.rs, after BrowserSettings; wire "browser.settings.set"]
Method::BrowserFix   [src/api/schema.rs, after BrowserSettingsSet, last of the fork block; wire "browser.fix"]
BrowserOp::Click, Type, Press, Select, Fill, Hover, Snapshot, Batch   [src/api/schema/browser.rs, fork-owned enum, after Focus and before the `Unknown` serde(other) fallback (which stays last); wire "click" "type" "press" "select" "fill" "hover" "snapshot" "batch"; Snapshot is the read path with the snapshot format (a batch step whose refs serve the following steps); a batch's steps are BrowserBatchStep { tab, flattened op }, Batch carries close_opened]
KeybindAction::OpenBrowser   [src/input/keybindings.rs, after OpenNews; internal]
KeybindAction::OpenCoordinator   [src/input/keybindings.rs, after OpenBrowser; internal]
ClientShellOverlayKind::Browser   [src/client/shell/state.rs, last after Settings; internal]
ClientShellOverlay::Browser   [src/client/shell/state.rs, last after Settings; the Browser overlay (client/shell/browser.rs), client-local, never on the wire]
ClientContextMenuTarget::Browser   [src/client/shell/state.rs, last after News; the tabs layout's pinned Browser row, `{ profile, running, local }`; internal]
ClientContextMenuTarget::Coordinator   [src/client/shell/state.rs, last after Browser; the tabs layout's pinned coordinator row, `{ items }` captured when the menu opened; internal]
ClientContextMenuAction::BrowserFocusWindow   [src/client/shell/state.rs, after NewsToggleSchedule; the Browser row menu's Focus window; internal]
ClientContextMenuAction::BrowserToggleProfile   [src/client/shell/state.rs, after BrowserFocusWindow; Start / Stop profile; internal]
ClientContextMenuAction::BrowserOpenOverlay   [src/client/shell/state.rs, last after BrowserToggleProfile; Open overlay; internal]
ClientContextMenuAction::Coordinator   [src/client/shell/state.rs, last after BrowserOpenOverlay; carries a CoordinatorMenuAction (Open dashboard, Focus, Wake now, Restart, Pause/Resume, Settings); internal]
PendingEndpointKind::BrowserGet   [src/client/shell/state.rs, after NewsSetQuietHours; internal]
PendingEndpointKind::BrowserFocus   [src/client/shell/state.rs, after BrowserGet; internal]
PendingEndpointKind::BrowserStart   [src/client/shell/state.rs, after BrowserFocus; internal]
PendingEndpointKind::BrowserStop   [src/client/shell/state.rs, after BrowserStart; internal]
PendingEndpointKind::BrowserSettings   [src/client/shell/state.rs, after BrowserStop; internal]
PendingEndpointKind::BrowserSettingsSet   [src/client/shell/state.rs, after BrowserSettings; internal]
PendingEndpointKind::BrowserFix   [src/client/shell/state.rs, after BrowserSettingsSet; internal]
PendingEndpointKind::AgentsSettings   [src/client/shell/state.rs, after BrowserFix; internal]
PendingEndpointKind::AgentsSettingsSet   [src/client/shell/state.rs, after AgentsSettings; internal]
PendingEndpointKind::AgentsFix   [src/client/shell/state.rs, after AgentsSettingsSet; internal]
PendingEndpointKind::Coordinator   [src/client/shell/state.rs, after AgentsFix; carries a CoordinatorRequestKind; internal]
ClientSettingsSection::Browser   [src/client/shell/state.rs, last after News, in ALL and label() ("browser"); internal]
ClientSettingsSection::Coordinator   [src/client/shell/state.rs, after Browser, in ALL and label() ("coordinator"); internal]
ClientSettingsSection::Agents   [src/client/shell/state.rs, last after Coordinator, in ALL and label() ("agents"); internal]
EndpointControlMessage::AgentNotices   [src/client/endpoint/control.rs, after AgentCompletions; the decoded `endpoint.agent-notices.v1` optional control (malformed data → Ignored); internal]
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
ResponseResult::NewsHistory   [src/api/schema/response.rs, after CoordinatorGet; wire "news_history"]
ResponseResult::BrowserRun   [src/api/schema/response.rs, after NewsHistory; wire "browser_run"]
ResponseResult::BrowserGet   [src/api/schema/response.rs, after BrowserRun; wire "browser_get"]
ResponseResult::BrowserStatus   [src/api/schema/response.rs, after BrowserGet; wire "browser_status"]
ResponseResult::BrowserLog   [src/api/schema/response.rs, after BrowserStatus; wire "browser_log"]
ResponseResult::BrowserActor   [src/api/schema/response.rs, after BrowserLog; wire "browser_actor"]
ResponseResult::BrowserProfiles   [src/api/schema/response.rs, after BrowserActor; wire "browser_profiles"]
ResponseResult::CoordinatorGet   [src/api/schema/response.rs, directly after NewsGet; wire "coordinator_get"; every coordinator.* method answers it]
ResponseResult::BrowserSettings   [src/api/schema/response.rs, after BrowserProfiles; wire "browser_settings"]
ResponseResult::AgentNotify   [src/api/schema/response.rs, after BrowserSettings; wire "agent_notify"]
ResponseResult::AgentNotices   [src/api/schema/response.rs, after AgentNotify; wire "agent_notices"; agent.notices and agent.notice_dismiss answer it]
ResponseResult::AgentMessageSend   [src/api/schema/response.rs, after AgentNotices; wire "agent_message_send"]
ResponseResult::AgentMessageClaim   [src/api/schema/response.rs, after AgentMessageSend; wire "agent_message_claim"]
ResponseResult::AgentsSettings   [src/api/schema/response.rs, after AgentNotices; wire "agents_settings"; agents.settings and agents.settings.set answer it]
ResponseResult::AgentsFix   [src/api/schema/response.rs, after AgentsSettings, last of the fork block; wire "agents_fix"]
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
ConfigEdit::NewsEnabled   [src/config/write.rs, after DailyReminderTime; `news.enabled`, written by news.set_enabled on the server (the settings news tab's toggle goes through that method); internal]
ConfigEdit::NewsTimes   [src/config/write.rs, after NewsEnabled; `news.times` as a one-line TOML array of quoted `HH:MM`, written by news.set_times on the server (the settings news tab's time rows go through that method); internal]
ConfigEdit::NewsQuietHours   [src/config/write.rs, after NewsTimes; `news.quiet_hours` as a quoted window or "" for off, written by news.set_quiet_hours on the server (the settings news tab's quiet-hours picker goes through that method; a server without it gets the client's local write); internal]
ConfigEdit::BrowserBool { key, value }   [src/config/write.rs, after SidebarLayoutTabs; a `[browser]` toggle (`enabled`, `show_activity`, `pin_dashboard`, `steer_agents`, `disable_native_browser`; `wrap_agents` goes through ConfigEdit::AgentsWrap and `shell_hook` is refused with `moved`), written by browser.settings.set on the server; internal]
ConfigEdit::BrowserString { key, value }   [src/config/write.rs, after BrowserBool; `browser.activity_color` as a quoted `#rrggbb`; internal]
ConfigEdit::BrowserList { key, values }   [src/config/write.rs, after BrowserString; `browser.mcp_agents` as a TOML array of quoted names; internal]
ConfigEdit::AgentsBool { key, value }   [src/config/write.rs, after BrowserList; an `[agents]` toggle (`tools`, `instructions`, `notices`), written by agents.settings.set on the server; internal]
ConfigEdit::AgentsWrap(bool)   [src/config/write.rs, after AgentsBool; `[agents] wrap` and the legacy `[browser] wrap_agents` removed in the same toml_edit write (agents.settings.set wrap, browser.settings.set wrap_agents); internal]
ConfigEdit::AgentsWrapWithBrowser { wrap, browser }   [src/config/write.rs, after AgentsWrap; `[agents] wrap` (legacy key dropped) plus `[browser]` toggles in one write (browser.settings.set `steer_wrap` = steer_agents + the wrap); internal]
ConfigEdit::AgentsInstructionsFile(Option<&str>)   [src/config/write.rs, last after AgentsWrapWithBrowser; `[agents] instructions_file`, None removes it (the built-in paragraph); internal]
ConfigEdit::CoordinatorEnabled   [src/config/write.rs, after NewsQuietHours, before BrowserBool; `coordinator.enabled`, written by coordinator.set_enabled on the server; internal]
ConfigEdit::CoordinatorWakeCaps { cap_hour, cap_day }   [src/config/write.rs, after CoordinatorEnabled; both caps, coordinator.set_wake_caps; internal]
ConfigEdit::CoordinatorModel   [src/config/write.rs, after CoordinatorWakeCaps; `coordinator.model`, None removes it; internal]
ConfigEdit::CoordinatorNotify   [src/config/write.rs, after CoordinatorModel; `coordinator.notify`; internal]
ConfigEdit::SidebarLayoutTabs   [src/config/write.rs, after CoordinatorNotify, directly before BrowserBool; `ui.sidebar_layout = "tabs"`, the coordinator settings hint row (client-local write); internal]
AppEvent::CoordinatorPassFinished   [src/events.rs, last after WorktreeReadFinished; the coordinator worker's pass output; internal]
PinnedRow::Coordinator   [src/client/shell/tab_sidebar.rs, fork-owned enum, last after News; internal]
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
PendingEndpointKind::NewsSetTimes   [src/client/shell/state.rs, after NewsSetEnabled; internal]
PendingEndpointKind::NewsSetQuietHours   [src/client/shell/state.rs, after NewsSetTimes; internal]
AgentRenameError::Suspended   [src/app/agents.rs, after PendingLaunch; internal, surfaces as error code agent_suspended]
SubagentEvent::Snapshot   [src/api/schema/panes.rs, fork-owned enum, last after Stop; wire "snapshot"]
AgentSuspendError::SubagentsRunning   [src/app/agent_suspend.rs, fork-owned enum, after Working; surfaces as error code agent_subagents_running, restart inherits it through AgentRestartError::Suspend]
Method::TeamList, TeamGet, TeamMake, TeamDisband, TeamSetPurpose, TeamSetRole, TeamJoin, TeamLeave, TeamContext   [src/api/schema.rs, directly after AgentsFix (before TabSetColor); wire "team.list" … "team.context" (src/api/schema/team.rs `method` consts)]
ResponseResult::TeamList, TeamReply, TeamContext   [src/api/schema/response.rs, after AgentsFix; wire "team_list", "team_reply", "team_context"]
TeamActor::{User, Coordinator, Agent { name }, Unknown}   [src/api/schema/team.rs, fork type; snake_case, Unknown is the serde(other) fallback; append-closed (persisted in session.json through TeamSnapshot.purpose_by)]
EndpointControlMessage::Teams   [src/client/endpoint/control.rs, after AgentNotices; the decoded `endpoint.teams.v1` optional control (malformed data → Ignored); internal]
ClientShellOverlay::TeamInfo, ClientShellOverlayKind::TeamInfo   [src/client/shell/state.rs, last of the fork overlays; the Team info panel (team_overlay.rs); internal]
ClientRenameTarget::TeamPurpose { workspace_id, make, reopen }, ClientRenameTarget::TeamRole { pane_id, workspace_id, reopen }   [src/client/shell/state.rs, last; the Rename modal edits a team purpose (make: team.make) or a member role, reopening Team info when `reopen`; internal]
ClientContextMenuAction::MakeTeam, TeamInfo, EditPurpose, DisbandTeam, SetTeamRole, LeaveTeam, JoinTeam   [src/client/shell/state.rs, last after Coordinator(CoordinatorMenuAction); group menu (Make team… / Team info / Edit purpose… / Disband team) and tab menu (Set team role… / Leave team / Join team) items, listed after upstream's; internal]
PendingEndpointKind::Team(TeamRequestKind)   [src/client/shell/state.rs, after Coordinator; `team.*` replies routed to teams.rs; internal]
Method::NotesGet, NotesSet, NotesAppend, CheckpointsList, CheckpointsAdd, CheckpointsUpdate, CheckpointsRemove, CheckpointsContext   [src/api/schema.rs, after BrowserFix, last of the fork block in that order; wire "notes.get" "notes.set" "notes.append" "checkpoints.list" "checkpoints.add" "checkpoints.update" "checkpoints.remove" "checkpoints.context"]
ResponseResult::NotesGet, NotesWrite, CheckpointsList, CheckpointWrite, CheckpointContext   [src/api/schema/response.rs, after TeamContext, last of the fork block in that order; wire "notes_get" "notes_write" "checkpoints_list" "checkpoint_write" "checkpoint_context"]
NotesAuthor, CheckpointKind, NotesWriteOutcome, CheckpointContextSource   [src/api/schema/notes.rs, fork-owned enums, snake_case with `Unknown` the serde(other) fallback (last); JSON only, append-closed]
KeybindAction::ToggleInfoPane   [src/input/keybindings.rs, after OpenCoordinator; internal, handled by the client only]
ClientChromeDrag::InfoDockWidth   [src/client/shell/state.rs, last; the info dock divider drag; internal]
ClientContextMenuAction::ToggleInfoPane   [src/client/shell/state.rs, last after Coordinator; the pane menu's "Info pane"; internal]
PendingEndpointKind::InfoNotesGet, InfoNotesWrite, InfoCheckpointsList, InfoCheckpointWrite, InfoCheckpointContext { id }   [src/client/shell/state.rs, after Coordinator, last in that order; internal]
AppEvent::NotesWorkerFinished   [src/events.rs, last after CoordinatorPassFinished; the notes worker's transcript locate / context read; internal]
ClientContextMenuAction::CopySessionId   [src/client/shell/state.rs, last after ToggleInfoPane; the tab and pane menus' "Copy session ID"; internal]
PendingEndpointKind::ContextMenuSession { pane_id }   [src/client/shell/state.rs, after InfoCheckpointContext; the menu's pane.get for the agent session id, routed to context_menu_session.rs; internal]
Method::AgentsActor, AgentsDirectory, AgentsRead, AgentsOpenTab, AgentsSendMessage, AgentsRenameTab, AgentsMoveTab, AgentsSetMeta, AgentsCloseTab, AgentsReopenTab, AgentsNotesAppend, AgentsCheckpoint, AgentsActions, AgentsCheck   [src/api/schema.rs, directly after CheckpointsContext, last of the fork block in that order; wire "agents.actor" "agents.directory" "agents.read" "agents.open_tab" "agents.send_message" "agents.rename_tab" "agents.move_tab" "agents.set_meta" "agents.close_tab" "agents.reopen_tab" "agents.notes_append" "agents.checkpoint" "agents.actions" "agents.check"]
ResponseResult::AgentsActor, AgentsDirectory, AgentsRead, AgentsOpenTab, AgentsMessage, AgentsRenameTab, AgentsMoveTab, AgentsSetMeta, AgentsCloseTab, AgentsReopenTab, AgentsActions, AgentsCheck   [src/api/schema/response.rs, directly after CheckpointContext, last in that order; wire "agents_actor" "agents_directory" "agents_read" "agents_open_tab" "agents_message" "agents_rename_tab" "agents_move_tab" "agents_set_meta" "agents_close_tab" "agents_reopen_tab" "agents_actions" "agents_check"; agents.notes_append answers NotesWrite and agents.checkpoint CheckpointWrite]
AgentPaneKind, AgentActorKind, AgentsAccess, AgentsScreenAccess, AgentsTurnOrigin, AgentsOriginDetail, AgentsActionOutcome, AgentsCloseOutcome, AgentsMessageOutcome, AgentsCheckAction, AgentsReadSource, AgentsReadFormat, AgentsWho   [src/api/schema/agents_model.rs, fork-owned enums, snake_case with `Unknown` the serde(other) fallback (last); JSON only, append-closed]
InputSource, Programmatic, TurnOrigin, LimitRefusal, policy::{Actor, Relation, Dest, SoftEdit, OwnOnly, TeamOp, Action, Decision}, NotSuspendableReason   [src/agents_model/*.rs and src/app/agent_suspend.rs; internal, never serialized except TurnOrigin's projection AgentsTurnOrigin]
ClientRenameTarget::AgentRole { pane_id, member, known }   [src/client/shell/state.rs, last after TeamRole; the tab menu's "Set role…" prompt (agents.set_meta, team.set_role for a member on a server without it); internal]
ClientContextMenuAction::SetRole   [src/client/shell/state.rs, last after ToggleInfoPane; the tab menu's "Set role…"; internal]

## 5. Owned API methods and digests
agent.suspend, agent.activate, agent.restart, agent.transcripts: fork-defined (Method variants, api_method_name arms, request_changes_ui for suspend/activate/restart, CLI `herdr agent suspend|activate|restart|transcripts`).
agent.restart digest: dc124dcfe9d7fc0fe3d9a85e00548a0263574d3de68ffd16370f2b6ba67ad062 (AgentRestartParams { target }).
agent.prompt (upstream method): fork optional param `guard_user_typing` (serde default false, omitted when false, so the JSON of an unguarded prompt is upstream's; an older server ignores it and types as before). When set, the server refuses with `user_typing` (nothing written) while a client typed into the pane within app::typing_guard::USER_INPUT_QUIET (10 s) or a Claude Code / Codex input box shows non-faint text. Set by the coordinator MCP's agents_send_message (coordinator::api::prompt) and the coordinator wake-up; `herdr agent prompt` leaves it off. Not advertised to the client shell, so no digest.
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
news.run, news.status: fork-defined (Method::NewsRun/NewsStatus, api_method_name arms, request_changes_ui for news.run only (it may open the News tab), handlers in src/app/news.rs, CLI `herdr news run|status|log [n]` in src/cli/news.rs). Both take EmptyParams and answer ResponseResult::NewsStatus { status: NewsStatusInfo } (a response type, so not digest-covered; carries pending_notifications since phase 4); news.run starts a run first and refuses with news_run_in_flight while one lasts, news_unavailable without a persisted session, news_run_failed when the runner install or the tab fails. news.status is not advertised to the client shell; news.run is (the News row menu's "Run now").
news.get, news.history, news.open, news.set_enabled: fork-defined (Method::NewsGet/NewsHistory/NewsOpen/NewsSetEnabled, api_method_name arms, request_changes_ui for news.open only (it focuses the News tab), handlers in src/app/news.rs, CLI `herdr news open [--edition N] | history [--days N] [--json] | enable | disable` in src/cli/news.rs). news.get (EmptyParams), news.open (NewsOpenParams { edition: Option<u32> }) and news.set_enabled (NewsSetEnabledParams { enabled }) answer ResponseResult::NewsGet { news: NewsGetInfo }; news.history (NewsHistoryParams { days: Option<u32> }, not advertised to the client shell) answers ResponseResult::NewsHistory { editions: Vec<NewsEditionInfo> } from `<home>/editions/index.json`. news.open focuses the tab (created with the page viewer when gone), with `edition` quits the pane with `q` and runs the viewer on that edition once the shell prompt is back (a pending pane command driven from handle_news_tasks), refusing news_run_in_flight while a run lasts and news_edition_not_found for an unknown number; news.set_enabled writes `news.enabled` through ConfigEdit::NewsEnabled and calls App::reload_config (news_config_write_failed on a write error). Focusing the News tab clears its important mark on the next scheduler pass (clear_news_unread_when_focused in handle_news_tasks). The same pass records the edition read (note_news_read_when_focused; at most once per READ_CHECK_INTERVAL = 1 s, the file read only when its mtime moved since the last judgement, and never while a run or a pending viewer command owns the pane): while the News tab is focused and its pane is not at a shell prompt, the edition the viewer reports in `<home>/viewer-state.json` (`showing`, written by viewer.py on every edition it opens) becomes `last_read_edition` when it is newer — never moving backwards — persisted in news.json and mirrored to `<home>/read.json` (`last_read_edition`, read by the viewer as its "since you read" baseline). NewsGetInfo carries `last_read_edition` and `new_stories` (stories of the latest edition absent from the last one read, by url else normalised headline — persist::news::new_story_count_in over one read of the index, computed by NewsState::refresh_new_story_count when the mark moves, a run finishes and at load, never on the request path; absent until something was read); the News row's unread state reads `N new` from it, and a run notification's body starts with `N new · ` (or is just `N new`) when the count is above zero.
news.get digest: aa473a7480d449faed99b6e7958a359b79c42352fa2baf37a080c17d17c8c855 (EmptyParams).
news.open digest: bb5614fbd62d35a07127482009b8f69808325fecf27588dc70da6a19b15b4fb7 (NewsOpenParams { edition: Option<u32> }).
news.run digest: 19594c58ed9598a02c2edd64f939dfd11e71b82267bdcaac244a3f5e840209ff (EmptyParams).
news.set_enabled digest: 1246ec256f7160ad56a4bc171faceb2e68649e592628021b0f821624c87b2d74 (NewsSetEnabledParams { enabled: bool }).
news.set_times: fork-defined (Method::NewsSetTimes, api_method_name arm, no request_changes_ui, handler App::set_news_times in src/app/news.rs, CLI `herdr news times [HH:MM ...] [--clear]` in src/cli/news.rs). NewsSetTimesParams { times: Vec<String> }: every entry `HH:MM` (news_invalid_time otherwise), written to `news.times` through ConfigEdit::NewsTimes sorted and without duplicates, then App::reload_config (news_config_write_failed on a write error); answers ResponseResult::NewsGet { news: NewsGetInfo }. The schedule follows at once: `next_run_at` is derived from `news.times` and `news.json`'s `last_started_at`, never stored. Advertised to the client shell (the settings news tab's time rows, `add time`, Delete).
news.set_times digest: 7962b61c93c83ef9e9cdd0b42a433ae171891fc11b1df42a5120417d9cb9d805 (NewsSetTimesParams { times: Vec<String> }).
news.set_quiet_hours: fork-defined (Method::NewsSetQuietHours, api_method_name arm, no request_changes_ui, handler App::set_news_quiet_hours in src/app/news.rs, CLI `herdr news quiet [HH:MM-HH:MM|--off]` in src/cli/news.rs). NewsSetQuietHoursParams { quiet_hours: String }: `HH:MM-HH:MM` (parsed by config::parse_quiet_hours, written in the canonical `HH:MM-HH:MM` form) or an empty string for off (news_invalid_quiet_hours otherwise), written to `news.quiet_hours` through ConfigEdit::NewsQuietHours, then App::reload_config (news_config_write_failed on a write error; the window is applied in memory when the reload could not); answers ResponseResult::NewsGet { news: NewsGetInfo }. Advertised to the client shell (the settings news tab's quiet-hours picker).
news.set_quiet_hours digest: 985cacc65cb6e91faef3336f78ae66132ed67d8b2a815805876ea78be3682863 (NewsSetQuietHoursParams { quiet_hours: String }).
browser.run: fork-defined (Method::BrowserRun, api_method_name arm, no request_changes_ui, served on the connection thread by src/browser/serve.rs (browser::serve::run through finish_wait_response in src/api/server.rs), never by the App (a BrowserRun reaching handle_api_request answers browser_lane); CLI `herdr browser <open|navigate|back|forward|reload|read|snapshot|find|links|screenshot|console|network|wait|scroll|eval|dialog|tabs|use|close|focus|click|type|press|select|fill|hover|batch>` (plus `setup` (config-driven: [browser] mcp_agents and shell_hook, the sidecar; `--claude`/`--codex`/`--shell [--remove]`/`--no-mcp` restrict one run), `doctor [--json]` (the shared checks), `mcp`, `wrap`, `install-chromium` — src/browser/brand.rs, error code `install_failed`; setup/doctor code in src/browser/setup.rs) in src/cli/browser.rs; BrowserActor::Pane carries `shell_pid` (the pane's PTY child) so `herdr browser mcp` can refuse (`wrong_pane`) when that pid is not among its ancestors and the `herdr browser mcp` tools (25) in src/cli/browser_mcp.rs). The act family (click, type, press, select, fill, hover; target by `ref` from a snapshot or `selector`) runs in the sidecar with `force` on a background tab, is refused with act_disabled when `[browser] allow_act = false`, and refuses a password field (`input[type=password]`, autocomplete current-/new-password, the focused element for a bare press) with password_field_refused unless `[browser] type_into_password_fields = true`; the ledger records `act:<kind>` with the element's role and name and a text length, never the text; `eval` sends `guard_passwords` (= !type_into_password_fields): the sidecar snapshots the password fields in the `herdr` isolated world (a remote object id, values never leave the page), restores a changed one after the code and answers `password_field_refused`; its ledger detail is `eval: <code>` (sanitized, string literals longer than `shape::EVAL_LITERAL_KEEP` = 20 chars masked as `"‹N chars›"` by `shape::mask_long_literals` before clipping to 200 chars; `MAX_DETAIL_CHARS` 220), never the result; a guarded eval never goes through the sidecar's navigation retry — a navigation during it answers password_field_refused, the code is not run again. `batch` (BrowserOp::Batch { ops: Vec<BrowserBatchStep>, stop_on_error (default true), final: snapshot|screenshot, close_opened (default false: closes the tabs the batch opened after the final step and puts the pane's cursor back), animate (default true; false skips the activity cursor's glide between steps, frame and group stay) }; a `snapshot` step's refs serve the following steps and its output (paged to snapshot_max_chars) is kept in the step result) runs at most BATCH_MAX_STEPS (20) steps under one clamped deadline, each through the same checks and its own ledger entry plus one `batch` entry; nested batches and unknown steps are invalid_request, more than 20 steps batch_too_long, remaining steps after a stop are listed as skipped. BrowserRunParams { caller: Option<BrowserCaller { pane_id }>, profile, tab, op: BrowserOp (flattened, tagged `op`), timeout_ms }; answers ResponseResult::BrowserRun { result: BrowserRunResult } (header, text, tab, image paths, data, ms). Not advertised to the client shell, so no digest; new operations append BrowserOp variants in src/api/schema/browser.rs. Activity overlay (`[browser] show_activity`, src/browser/activity.rs): every sidecar request of a pane actor's call carries `HostRequest.activity` { frame, animate, color (`[browser] activity_color`, `#rrggbb`, live after reload-config: an installed overlay of another colour is re-injected), linger_ms (active_glyph_secs × 1000: how long the frame and the cursor stay after the op), group { key: pane id, title `<symbol> <herdr tab label>` (the symbol from `[browser] group_symbols` by agent id, `default` otherwise; the pane id when the tab has no label — no provider names), color: one of 8 tab-group colours by FNV-1a of the pane id (grey left out), collapse_ms: active_glyph_secs × 1000 } } (a thread-local in hub.rs set by `run`, `animate` cleared by a batch with animate = false, never set for the user or external callers); the sidecar (activity.mjs) injects the glow frame + cursor into an isolated world `herdr` (host `div[data-herdr-overlay]`, closed shadow root, pointer-events none, hidden during screenshots; `Overlay.show` pulses it while the op runs, `linger(linger_ms)` switches it to the calmer idle look (`.frame.idle`) and fades it at the end of the window, `reapply` on domcontentloaded puts it back in the same state without stretching the window, `dismiss` removes it at once when the pane is released; the cursor stays where the last op left it for the whole window) and keeps per-pane tab groups through the companion extension (`companion/`, MV3, permissions tabs + tabGroups + alarms + storage, loaded with `--load-extension=<host>/companion`; the manifest pins the extension id with a `key` (a committed public key; the private half is not needed for an unpacked extension); Chrome keeps an unpacked command-line extension's worker script for good (a manifest version bump, a browser restart, `chrome.runtime.reload()` — which disabled the extension on a permission increase — and `importScripts` of a new URL all failed live; a worker unregistered at runtime does not come back until the next browser start), so a worker older than this herdr's files (`herdrPing` at attach) keeps serving the session and is reported as `extension update pending: herdr browser stop and open again` in status/doctor; right before herdr closes the browser (sidecar op `close_browser`) `Companion.stopStaleWorker` retires it — `ServiceWorker.enable`, `unregister { scopeURL }` and `stopWorker` in one page-target session over a fresh `connectOverCDP` connection of its own (the domain is not served on the browser target; through the sidecar's long-lived connection the unregister never took; an unregister alone, or a worker still running at shutdown, came back with the old script — all verified live) — and the next start registers the files on disk afresh; nothing under the profile is deleted any more (the earlier `Default/Service Worker` wipe took every site's workers with it) — bump `VERSION` in companion.js, the manifest version `0.<n>.0`, `COMPANION_VERSION` in activity.mjs and `COMPANION_VERSION` in browser_assets.rs (doctor prints it) together, driven by Runtime.evaluate on its service worker over a held DevTools WebSocket; groups are keyed by pane id — `herdrGroup { key, tabId, title, color, wasGrouped, active }` records key → groupId in `chrome.storage.session`, never adopts a group by title, and a tab in any other group (the user's, another pane's) is left alone; while the pane is active the group's title carries the `● ` activity mark (`ACTIVE_MARK` in companion.js; ownership, comparisons and restores go by the plain title, a group restored under the marked title is adopted too) and `herdrCollapse { key, title }` at the end of the window collapses the group and restores the plain title (an older sidecar's bare key still collapses); target ↔ tab id by newest-blank-tab at open, else exact URL (+ title); a tab the user pulls out stays out); sidecar op `ntp { snapshot }` (no profile) hands the new tab page's snapshot (`herdrSnapshot` → `chrome.storage.session.snapshot`; rows carry the CDP `target`, the top level `active_secs`, and the sidecar adds `tabIds` target → Chrome tab id from the companion driver's cache) to every ready companion — the page activates exactly that tab id on a click (no URL or title lookup; a tab that is gone re-renders), decides live/idle from `now - last_at` against `active_secs` itself, and its 1 s tick only rewrites ages and the live state, a full render happens on `storage.onChanged` only; a failed `ntp`/`dashboard` push is logged once as `browser.ntp.push`/`browser.dashboard.push` (warn) when it starts failing and once more when it recovers; sidecar op `release { keys: [pane id…] }` (no profile) first dismisses the overlay of every page whose `paneKey` (set by the host when a directive arrives) is listed — `dismissPanes` in activity.mjs, whatever the companion's state — then dissolves only each key's recorded group (`herdrRelease`) and answers `{ released, failed, reason }` — the hub (`release_panes`, from `known_pane_ids` checked against the App's panes on every `browser.get`/`browser.status`, independent of since_seq) marks a pane released only when it worked, logs `browser.release.failed` and retries a failed key after 30 s × attempts, at most 5 times; `attach` answers `companion: { state: ready|missing|unsupported, detail }`, shown as BrowserProfileInfo.companion (`off` when show_activity = false) in `browser status` and `doctor`. A sidecar `attach` whose connectOverCDP times out answers the code `attach_timeout` (retried once by the hub); an `open` whose navigation fails answers its error with `target` (HostError.target → BrowserError.target): the hub adopts the tab for the caller and makes it current before returning the error, and a batch's close_opened covers it. The caller is resolved once per call with browser.resolve_caller (BrowserCaller → ResponseResult::BrowserActor { actor: BrowserActor pane|user|external }, pane_not_found when unknown). Errors: browser_disabled, browser_unavailable, browser_config_write_failed, browser_executable_missing, browser_runtime_missing, browser_runtime_outdated, browser_start_failed, browser_launch_timeout, attach_blocked, profile_in_use, profile_not_found, profile_running, profile_busy, profile_protected, invalid_profile, host_failed, browser_host_restarted, no_current_tab, tab_not_found, tab_closed, stale_ref, browser_timeout, tab_not_rendered, dialog_open, no_dialog, eval_disabled, act_disabled, password_field_refused, batch_too_long, navigation_failed, too_large, unknown_op, browser_lane, invalid_request.
browser.get, browser.focus, browser.start, browser.stop: fork-defined (Method::BrowserGet/BrowserFocus/BrowserStart/BrowserStop, api_method_name arms, no request_changes_ui, handlers in src/app/browser.rs over the process-global crate::browser::hub()). browser.get (BrowserGetParams { since_seq: Option<u64> }) answers ResponseResult::BrowserGet { browser: BrowserGetInfo } (seq, unchanged, enabled, host, profiles, open tabs with opened_by / last / last_actor / users, recent_panes, setup_needed: a check only an explicit request may fix — an MCP registration, the shell hook — is failing, the Browser row's `!` hint; pane actors carry `gone` when the pane no longer resolves); browser.focus (BrowserTabTarget { profile, tab }) selects the tab and raises the window, answers Ok; browser.start (BrowserProfileTarget { profile }) and browser.stop (BrowserStopParams { profile, all }) enqueue the work on a hub thread and answer browser.get's record. All four advertised to the client shell: the client (src/client/shell/browser.rs) pulls browser.get on the first snapshot of a connection, after every browser.* reply it sent, every 2 s while a profile runs or starts or the overlay is open and every 15 s otherwise, and never from a server that does not advertise it; the pinned Browser row, the `◎` tab marker and the Browser overlay are drawn from it; browser.focus / start / stop are sent by the overlay and the row menu on a local endpoint only.
browser.get digest: 9957cef2770129f543c007b2ae28b61b960e7c8c060f287c0fe0cdbb64a08fb8 (BrowserGetParams { since_seq: Option<u64> }).
browser.focus digest: 8e5a572a95b7e84c5c9802ce81f6a18ff723ad2574247981627afd04383ab9a4 (BrowserTabTarget { profile: Option<String>, tab: String }).
browser.start digest: cfcaeeee2210fdf0d9425d462b0947b389be24d1357bbcdd39dd99608b150f51 (BrowserProfileTarget { profile: Option<String> }).
browser.stop digest: 7b8ec002e4def703fdb73643a82bb087e2afa69f56629f371a4c79b56a631e41 (BrowserStopParams { profile: Option<String>, all: bool }).
browser.settings, browser.settings.set, browser.fix: fork-defined (Method::BrowserSettings/BrowserSettingsSet/BrowserFix, api_method_name arms, no request_changes_ui, handlers in src/app/browser.rs over the process-global hub; the settings overlay's browser section). All three answer ResponseResult::BrowserSettings { settings: BrowserSettingsInfo } (the `[browser]` keys the section edits — enabled, show_activity, pin_dashboard, activity_color, steer_agents, wrap_agents, disable_native_browser, mcp_agents, shell_hook —, the default profile and whether it runs, a status line, the server-cached doctor checks `[BrowserCheckInfo { id, ok, detail, fixable, fix_kind: none|safe|edits_files }]` with ids executable, helper, extension, mcp_claude, mcp_codex, shell_hook, launch_context, `checked_at`, `checking`, `fixing`, the last `fixes: [BrowserFixResult { id, ok, detail }]`, `host`). browser.settings (EmptyParams) refreshes checks older than 20 s on a `herdr-browser-checks` thread (the reply carries the cache; `checking` says a newer one is coming); browser.get refreshes them when older than 5 min (the Browser row's `!` notices external changes without spawning node/launchctl per pull). browser.settings.set (BrowserSettingsSetParams { key, value: JSON }) validates the value (bools; `activity_color` `#rrggbb`, lower-cased; `mcp_agents` an array of claude/codex), writes it through ConfigEdit::Browser{Bool,Bools,String,List} — the browser edits go through toml_edit (`[browser]  # note`, `[ browser ]`, multi-line arrays, trailing comments and every other table survive; a file that does not parse comes back unchanged) and are written atomically (temp + rename next to the real file through a symlink, mode kept) — to the server's config file and calls App::reload_config, then compares the running config's value with the request: a mismatch (an invalid `[browser]` section the reload kept, an unparsable file) answers `browser_config_write_failed` "written but not applied: …" and runs no fix (`invalid_request` for a bad key or value; the key `steer_wrap` sets steer_agents and wrap_agents in one write); `mcp_agents` and `shell_hook` then run their file-editing fix (the request is the user's explicit act), every other key refreshes the checks. browser.fix (BrowserFixParams { ids }; empty = every failing fixable check) runs src/browser/setup.rs's fixes on a `herdr-browser-fix` thread (helper: assets + npm ci + runtime.json under setup::helper_lock — a process-wide mutex plus flock on `<browser home>/setup.lock`; mcp_claude: `claude mcp remove` only when the user-scope store has the entry, the store re-read afterwards, then `add-json --scope user` ("removed, re-adding failed" names the command to run by hand); mcp_codex: the config.toml table through toml_edit; shell_hook: the managed file and the guarded line, replacing another instance's line; extension: a synchronous stop_profile → Starting → ensure_running of the default profile, `ok` only when it is up again, only offered with 0 agents), then refreshes the checks; one worker at a time — a request during a running fix is merged into a pending set (an empty "all" request absorbs named ids) that the same worker drains with a fresh config before it clears `fixing`; every batch bumps a generation and a check refresh started before it drops its result instead of overwriting the fresh checks. The CLI `herdr browser setup` / `doctor [--json]` run the same code (setup::install_helper / register_claude / register_codex / install_shell_hook / checks); the fixes that edit user files only ever run on these explicit requests, the safe ones also from setup::auto_repair on the first browser use after the herdr binary changed (`<browser home>/setup.json`: herdr version, binary size and mtime, assets sha) — hub.auto_repair_once, a `std::sync::Once` per hub reached from start()'s thread and from ensure_running before a launch (the agent `run()` path; ensure_host alone only refreshed assets in place and never ran npm ci), so concurrent first uses wait for the one that runs it.
browser.settings digest: 3c10d3f5d952a2f78ea6536fc0730f87833b6bd41385ba2c9a208451bfff2cf8 (EmptyParams).
browser.settings.set digest: 6460c5c56d7e1e241ed59c4d76174e7fc4b7ab03b53c50fed717d359fddea833 (BrowserSettingsSetParams { key: String, value: serde_json::Value }).
browser.fix digest: 71558d5a9cf80cc82952b6bc1330776bce901118546e242640bf2384f51f5a8c (BrowserFixParams { ids: Vec<String> }).
coordinator.get, coordinator.open, coordinator.open_dashboard, coordinator.wake, coordinator.start, coordinator.set_enabled, coordinator.set_wake_caps, coordinator.set_model, coordinator.set_notify: fork-defined (Method::Coordinator*, api_method_name arms from crate::api::schema::coordinator::method, request_changes_ui for coordinator.open, coordinator.start and coordinator.wake, handlers in src/app/coordinator.rs, CLI `herdr coordinator enable|disable|start [--new]|wake|status [--json]|dashboard [--print]` in src/cli/coordinator.rs). Every method answers ResponseResult::CoordinatorGet { info: CoordinatorGetInfo }; write methods take an optional caller_pane (the CLI fills it from HERDR_PANE_ID, the TUI omits it) and refuse in_coordinator_turn from the coordinator's own pane during its turn. coordinator.open_dashboard {open = true}: clears unread suggestions and answers the URL at once; with open set, a short herdr-coordinator-open thread opens it through crate::browser::hub() when [browser] enabled, else platform::open_url (it cannot go through browser.run from the App: that answers browser_lane). Remote client shells send open: false and show the URL instead. Errors: coordinator_disabled, coordinator_unavailable, coordinator_blocked, coordinator_not_running, in_coordinator_turn, coordinator_config_write_failed, coordinator_restart_failed, invalid_caps, dashboard_unavailable.
coordinator.get digest: d6ec446e6d7ee38199feb4f9c48198613e9f339a534012bde7160fb07a63eaa8 (EmptyParams).
coordinator.open digest: 24a5c11d01bf8c6a65452115dc121a17f077ddf9d504d5c88c137f3b850536cc (EmptyParams).
coordinator.open_dashboard digest: d3d061db09bc59874312f676504bed2a6cc127bf6da2738f7214e32fd87facc0 (CoordinatorOpenDashboardParams { open: bool = true }).
coordinator.set_enabled digest: e7a703e3d1daa27cbe5b8637f7a3b8ac74a0c8f40497c88a53b699311d1bd71c (CoordinatorSetEnabledParams { enabled: bool, caller_pane: Option<String> }).
coordinator.set_model digest: 33c2821d6c15c99671f9df050f2f6caef09fc91c9724b70e22fcc92ad1996e7b (CoordinatorSetModelParams { model: Option<String>, caller_pane: Option<String> }).
coordinator.set_notify digest: 78ad0eb2d53f3fe07c1dda181ede781ee94914dd7d33d037df56cdcd6e503126 (CoordinatorSetNotifyParams { enabled: bool, caller_pane: Option<String> }).
coordinator.set_wake_caps digest: 9f6f9010ba7b4b9a8b8d8835d117f0ddd864f4184ee3324f50d0737fa084510a (CoordinatorSetWakeCapsParams { cap_hour: u32, cap_day: u32, caller_pane: Option<String> }).
coordinator.start digest: d536a69f3c65a742c3ab09e46e3b2f28aaa44c0d1570aa75befb14fb2bf3482e (CoordinatorStartParams { resume: bool = true, caller_pane: Option<String> }).
coordinator.wake digest: 9b8885f0516494756d440daf6de982049a3c0e7ea65b4f2395ff6446a2a62f94 (CoordinatorWakeParams { caller_pane: Option<String> }).
browser.status, browser.log, browser.profiles, browser.profile_create, browser.profile_delete: fork-defined (Method::BrowserStatus (EmptyParams → ResponseResult::BrowserStatus { status: BrowserStatusInfo }, the full picture: browser.get plus home, ledger path, executable, node, runtime.json, recent log), BrowserLog (BrowserLogParams { limit, pane_id, tab } → ResponseResult::BrowserLog { entries: Vec<BrowserActivity> }), BrowserProfiles (EmptyParams → ResponseResult::BrowserProfiles { profiles }), BrowserProfileCreate (BrowserProfileCreateParams { name, temporary }; `new` = a fresh tmp-<stamp>), BrowserProfileDelete (BrowserProfileName { name }; refuses the default profile and a running one, moves the directory to the Trash); api_method_name arms, handlers in src/app/browser.rs, CLI `herdr browser status|log|profile list|create|delete|start|stop|setup|doctor|mcp`). Not advertised to the client shell, so no digest.
notes.get, notes.set, notes.append, checkpoints.list, checkpoints.add, checkpoints.update, checkpoints.remove, checkpoints.context: fork-defined (Method::Notes*/Checkpoints*, api_method_name arms naming crate::api::schema::notes::method constants, none in request_changes_ui nor public_request_may_change_geometry (both asserted by tests), handlers in src/app/notes.rs over App.notes (crate::notes::NotesRuntime: the notes store, the checkpoint store, the pane → key map, the `herdr-notes-worker` thread), CLI `herdr notes read|path|append|write` and `herdr checkpoint add|list|show|rm|edit` in src/cli/notes.rs, agent MCP tools agents_notes_read / agents_notes_append / agents_notes_write / agents_checkpoint / agents_checkpoints_list in src/coordinator/mcp.rs). Every method takes a NotesTarget { tab_id, pane_id, key } (precedence key > pane_id > tab_id, all None = invalid_params): a pane resolves to its agent session's key `<agent>-<session id>` (hashed `<agent>-h<16 hex>` when the id is not a safe key), a tab to its focused pane's session, else the first pane with one, else `tab-<tab id>`; files `<config dir>/notes/<key>.md`, `<key>.meta.json`, `<key>.checkpoints.jsonl`. notes.get (NotesGetParams { target, known_revision }) and notes.set (NotesSetParams { target, text, base_revision, author }: CAS, `none` and None match missing notes; identical text answers `unchanged` before the base check; a mismatch answers outcome `conflict` with the current notes — never an error) and notes.append (NotesAppendParams { target, text, section, stamp, author }; never conflicts) answer ResponseResult::NotesGet { notes: NotesInfo } / NotesWrite { write: NotesWriteInfo }; checkpoints.list (CheckpointsListParams { target, kinds, since_seq, limit }) answers CheckpointsList { checkpoints: CheckpointsListInfo } (`seq` the JSONL length, `unchanged` when since_seq matches); checkpoints.add / update / remove (CheckpointsAddParams, CheckpointsUpdateParams, CheckpointTarget) answer CheckpointWrite { checkpoint: CheckpointWriteInfo } (60 adds per key per hour, same kind/title/author within 120 s folds into an update); checkpoints.context (CheckpointsContextParams { target, id, chars }) answers CheckpointContext { context: CheckpointContextInfo } with `source: pending` while the worker reads the transcript window (src/notes/transcript.rs; Claude and Codex), then native / backup / backup_previous / missing / unsupported. A tab's notes are copied once to the agent session's key when an agent session first appears in that tab (never after a /clear, never over an existing file). Errors: invalid_params, not_found, too_large, rate_limited, notes_disabled (`[notes] enabled = false`), notes_io (a file read or write failed).
notes.get digest: 514adb7872ed9fe98237146f87911e1f00d4fb3fb3d75e4054c783fc23e848bc (NotesGetParams { target: NotesTarget, known_revision: Option<String> }).
notes.set digest: 1ae65a5c7d8087a5e95169878a81ccd8bb532255d22f438fadc0ee2fb428afdb (NotesSetParams { target, text: String, base_revision: Option<String>, author: NotesAuthor }).
notes.append digest: 00925fe4941e3de5b418a568645b880f65e5c24c33f12b1b820a018f36555b51 (NotesAppendParams { target, text: String, section: Option<String>, stamp: bool, author: NotesAuthor }).
checkpoints.list digest: df0d37ec4550faaef6d7cd032f75aeac6403b70415a293d29a24f1c458f04ce2 (CheckpointsListParams { target, kinds: Vec<CheckpointKind>, since_seq: Option<u64>, limit: Option<u32> }).
checkpoints.add digest: f7e43fa3c09ace07c80fcd4b06e0e47d8df8d83eacbb76613ad17cd881deb580 (CheckpointsAddParams { target, kind: CheckpointKind, title: String, detail: Option<String>, tags: Vec<String>, author: NotesAuthor }).
checkpoints.update digest: a563335e063a6aa7a277b71b745b3b2cf6695efeae5457e9ff44130aba72b098 (CheckpointsUpdateParams { target, id: String, kind: Option<CheckpointKind>, title: Option<String>, detail: Option<String>, tags: Option<Vec<String>> }).
checkpoints.remove digest: f510c7c9e04c24b347efd4ea37ac98bb11016f1654816f35dab5d455ff329248 (CheckpointTarget { target, id: String }).
checkpoints.context digest: bce19edc6742c1a9c961a4bee33d9c1b5d0c516a03e65d0ca5ab722b6da47d93 (CheckpointsContextParams { target, id: String, chars: Option<u32> }).
NotesAuthor, CheckpointKind, NotesWriteOutcome and CheckpointContextSource (src/api/schema/notes.rs) are fork-owned and append-closed with the `Unknown` serde(other) fallback; NotesAuthor and CheckpointKind are inside the notes.* / checkpoints.* digests, so a new author or kind needs new method names.
pane.move: upstream method, advertised to the client shell only by the fork (absent from base CLIENT_SHELL_METHODS).
pane.get: upstream method, advertised to the client shell only by the fork (absent from base CLIENT_SHELL_METHODS) for the context menus' session id lookup (src/client/shell/context_menu_session.rs). Its digest covers upstream-owned PaneTarget, like pane.move's.
pane.get digest: c7d8d69186e7ac9806b4b20ad78e88a3bd37dcc2d57435cef107698bf9124643 (PaneTarget { pane_id }).
CLIENT_SHELL_METHODS (src/server/client_commands.rs): union, sorted; the test advertised_client_shell_methods_are_sorted_unique_and_in_schema enforces it.
agent.notify, agent.notices, agent.notice_dismiss: fork-defined (Method::AgentNotify/AgentNotices/AgentNoticeDismiss in src/api/schema/agent_notices.rs, api_method_name arms from its `method` consts, request_changes_ui for agent.notice_dismiss only (agent.notify marks the render dirty itself, only when a card is newly shown), handlers in src/app/agent_notices.rs over App.agent_notices, server-owned and not persisted). agent.notify (AgentNotifyParams { caller_pane, kind: AgentNoticeKind info|question|done|warning with the Unknown serde(other) fallback, title, body: Option }) → ResponseResult::AgentNotify { id, outcome: shown|deduped }; errors rate_limited (1/20 s, 5/10 min, 20/h per sender, "retry in Ns"), notices_off, pane_not_found, invalid_params. The sender (name, agent) comes from the server's pane record, never from params; one card per sender (session id, else pane id), at most 16. agent.notices (EmptyParams) and agent.notice_dismiss (AgentNoticeDismissParams { ids, all }, serde defaults) answer ResponseResult::AgentNotices { notices: Vec<AgentNoticeInfo> }. Callers: the herdr_agents MCP tool agents_notify (Verdict::Verified only; extra args ignored) and `herdr agent notify` (caller_pane from HERDR_PANE_ID). Client shells get the full list as the optional control `endpoint.agent-notices.v1` (src/server/headless/agent_notices.rs: AgentNoticesPayload { boot_id, revision, notices, initial }, sent from the render pass when ClientConnection.shell_agent_notices_sent != revision); old clients ignore it, new clients detect dismissal support by the advertised agent.notice_dismiss. A card clears on dismiss, when its pane goes, or when a client's own navigation (focus_shell_client_on_tab, the WorkspaceFocus arm of apply_shell_navigation_request) enters its tab; API focus never does.
agent.notice_dismiss digest: b82b1a70813888abb724380331ba44fb72812e9b177af4ba08cd17e104bba094 (AgentNoticeDismissParams { ids: Vec<String>, all: bool }).
agent.message_send, agent.message_claim: fork-defined (Method::AgentMessageSend/AgentMessageClaim in src/api/schema/agent_messages.rs, api_method_name arms from its `method` consts, request_changes_ui for both (they type into panes), handlers in src/app/message_queue.rs over App.message_queue); sent by `herdr coordinator mcp` (agents_send_message, agents_wait_for_message's claim); not advertised to the client shell, so no digest. agent.message_send { target, id, envelope, text, unix (default 0 = now), from_pane, from_name, from_role, reply_to, team, to_name (optional) } answers ResponseResult::AgentMessageSend { id, outcome: AgentMessageOutcome sent|queued (Unknown fallback), status, reason? }; agent.message_claim { id, pane } answers ResponseResult::AgentMessageClaim { claimed }. Errors: invalid_request, offline, queue_full (256 queued), and the agent.prompt errors that are not retryable. An older server answers invalid_request and the MCP server falls back to its own agent.prompt path.
agents.settings, agents.settings.set, agents.fix: fork-defined (Method::AgentsSettings/AgentsSettingsSet/AgentsFix in src/api/schema/agent_wrap.rs, api_method_name arms from its `method` consts, no request_changes_ui, handlers in src/app/agent_wrap_settings.rs over src/agent_wrap; the settings overlay's agents section). agents.settings and agents.settings.set answer ResponseResult::AgentsSettings { info: AgentsSettingsInfo { wrap, wrap_source agents|browser_legacy|default, tools, instructions, instructions_file, instructions_detail, steer_browser, notices, checks: Vec<AgentsCheckInfo { id shell_hook|claude|codex, state ok|outdated|missing|absent (Unknown fallback), detail, fixable, edits_files }>, hook_preview } }. agents.settings.set (AgentsSettingsSetParams { key wrap|tools|instructions|instructions_file|notices, value }) writes through ConfigEdit::Agents* and checks the reloaded config (agents_config_write_failed when it did not apply). agents.fix (AgentsFixParams { ids, confirm }) → ResponseResult::AgentsFix { results }; any request naming shell_hook without confirm == true answers confirm_required and writes nothing; it is the only server path that edits ~/.zshrc.
agents.fix digest: 5c28b30fb6a27e53ea9144fadfe971cc9ef8c7682fabe21cc99f5485ff250dd5 (AgentsFixParams { ids: Vec<String>, confirm: bool }).
agents.settings digest: bdae4ed7aed2040ce09b342c53c7e0ca69882ca001d12f7a61f7127fbd2a5b6a (EmptyParams).
agents.settings.set digest: 1b012b44b0d82bfa63daaf37373d75b76289217c1677345bfd4dc8c27c2b59d0 (AgentsSettingsSetParams { key: String, value: serde_json::Value }).
browser.settings.set and browser.fix since the Agents section: `shell_hook` answers `moved` and is no longer a browser check id or fix (browser.fix and hub.auto_repair_once never reach .zshrc); `wrap_agents` writes ConfigEdit::AgentsWrap and `steer_wrap` ConfigEdit::AgentsWrapWithBrowser (legacy key dropped); BrowserSettingsInfo.wrap_agents reports the effective `[agents] wrap` (BrowserConfig.effective_wrap, serde skip, filled by both config loaders).
team.list, team.get, team.make, team.disband, team.set_purpose, team.set_role, team.join, team.leave, team.context: fork-defined (Method::Team* in src/api/schema/team.rs, api_method_name arms from its `method` consts, request_changes_ui for make/disband/set_purpose/set_role/join/leave, handlers in src/app/team.rs; team state is server-owned on Workspace.team and persisted in session.json). team.list (EmptyParams) → ResponseResult::TeamList { revision, teams: Vec<TeamInfo> }; team.get (TeamGetParams { workspace_id?, caller_pane? }), team.make (TeamMakeParams { workspace_id, purpose?, caller_pane? }), team.disband (TeamWorkspaceParams { workspace_id }), team.set_purpose (TeamSetPurposeParams { workspace_id, purpose?, caller_pane? }), team.set_role (TeamSetRoleParams { pane_id, role?, caller_pane? }), team.join (TeamJoinParams { pane_id, role? }), team.leave (TeamPaneParams { pane_id }) → ResponseResult::TeamReply { team: Option<TeamInfo>, renamed? }; team.context (TeamContextParams { caller_pane, ack, full, ack_revision?, ack_key? }) → ResponseResult::TeamContext { member?, eligible, team?, text?, revision, ack_key? } (the agent-side read: the wrap's launch roster, the Claude per-turn hook `herdr team hook`, the herdr_agents MCP header; `ack` marks the member told up to `ack_revision`, else the current revision, and never bumps the view, saves or asks for a render; with `ack_key` (the opaque key a read returned: the team's per-process epoch, or the pending disband/removal line's sequence) the ack is ignored once the pane's team or line is no longer the one read). TeamInfo { workspace_id, workspace_label, purpose?, purpose_by?: TeamActor, created_unix, revision, members: Vec<TeamMemberInfo { pane_id, tab_id?, name?, agent?, role?, status?, status_since_unix?, joined_unix }>, excluded: Vec<String> } is re-projected to public ids on every reply and push. Errors: first_space, team_exists, no_team, workspace_not_found, pane_not_found, not_a_member, not_in_team_group, coordinator_pane, no_agent, invalid_params. caller_pane absent = the user; present = an agent, and purpose_by comes from the server's pane record (Coordinator when it is the coordinator pane, else Agent { name }), never from params. team.list and team.context are not client-shell methods. Client shells get the full list as the optional control `endpoint.teams.v1` (src/server/headless/teams.rs: TeamsPayload { boot_id, revision, teams } with no status fields, sent from the render pass when ClientConnection.shell_teams_sent != AppState.teams_view_rev; status changes never bump it); old clients ignore it, new clients gate the team menu items on the advertised team.get.
team.disband digest: 967a5429a196186df1bbfd4a382205220640fd473608296a690f6cc75e8bdf16 (TeamWorkspaceParams { workspace_id: String }).
team.get digest: c1f9494dc09f9a6f0284aae2c3c349d3599e12353606abc25f56c5eac9d52345 (TeamGetParams { workspace_id: Option<String>, caller_pane: Option<String> }).
team.join digest: a3f0ed909e8e9b17835308e4b416d4d4bbb746f3317d9cdd088396fe41853094 (TeamJoinParams { pane_id: String, role: Option<String> }).
team.leave digest: db3e86aca6a94e80e07238ad8d3ecb88997f6ec95524d4c45586b212b9eeaf2b (TeamPaneParams { pane_id: String }).
team.make digest: 51ce432dd19278c5258f1dc07a4b21261bf2788fd8de0d319cd75d19ffa2562c (TeamMakeParams { workspace_id: String, purpose: Option<String>, caller_pane: Option<String> }).
team.set_purpose digest: 9e3683747ca882cfa21e6200cc814a9d7c1ce91a90e5e68e3b548a7aec32f89f (TeamSetPurposeParams { workspace_id: String, purpose: Option<String>, caller_pane: Option<String> }).
team.set_role digest: 0d0663aa876a6f81790a20e944cfd655a045a3427edde7ffe13fb74da6cd29f3 (TeamSetRoleParams { pane_id: String, role: Option<String>, caller_pane: Option<String> }).
agents.actor, agents.directory, agents.read, agents.open_tab, agents.send_message, agents.rename_tab, agents.move_tab, agents.set_meta, agents.close_tab, agents.reopen_tab, agents.notes_append, agents.checkpoint, agents.actions, agents.check: fork-defined, the agents model v2 (Method::Agents*, api_method_name arms naming crate::api::schema::agents_model::method constants, request_changes_ui for open_tab, send_message, rename_tab, move_tab, set_meta, close_tab and reopen_tab, handlers in src/app/agents_model.rs and src/app/agents_close.rs over the pure policy in src/agents_model/policy.rs). Every method takes `caller_pane` (absent = the user; the TUI omits it): the server resolves the caller (agent, coordinator, or a shell = the user), its team and its turn origin, decides once, and logs every write and every refusal to `<coordinator dir>/actions.jsonl`. The herdr_agents MCP (src/coordinator/mcp.rs, resolving itself with agents.actor) and the CLI agent route (src/cli.rs agent_route, `herdr tab|pane|agent|workspace|team …` run from a live agent's pane) both go through these methods. Only agents.set_meta is advertised to the client shell (the tab menu's "Set role…"). Errors: invalid_params, not_found, caller_unresolved, outside_team, non_user_turn, shell_private, protected_tab, own_only, invalid_target, not_an_agent, busy, user_typing, offline, blocked, rate_limited, loop_guard, spawn_limit, target_busy, shell_busy, not_resumable, no_graceful_exit, agent_refused, failed.
agents.set_meta digest: 09a29ef4894ed7e5a78254457e1846c6fd96b7e48eb119895ae948eac235a512 (AgentsSetMetaParams { caller_pane: Option<String>, target: String, role: Option<String>, note: Option<String> }).
Team params reference only Team* types, plain strings, booleans and numbers (team_params_reference_no_existing_schema_type), so no other schema change can move a team digest. CLIENT_SHELL_METHODS has 94 entries (77 + the seven team.* client-shell methods + the eight notes.* / checkpoints.* methods + pane.get at their sorted places + agents.set_meta directly after agents.fix).
Digest asserts in advertised_client_shell_method_shapes_stay_at_the_v1_contract: the fork appends fifty-four `actual.remove(..)` asserts (agent.suspend, agent.activate, pane.move, agent.transcripts, agent.restart, tab.set_color, tab.set_remind, tab.set_reminder, session.closed_list, session.closed_remove, session.closed_reopen, news.get, news.open, news.run, news.set_enabled, news.set_times, news.set_quiet_hours, browser.focus, browser.get, browser.start, browser.stop, browser.settings, browser.settings.set, browser.fix, then the nine coordinator.* methods, then agent.notice_dismiss, agents.fix, agents.settings, agents.settings.set, then agents.set_meta, then team.disband, team.get, team.join, team.leave, team.make, team.set_purpose, team.set_role, then the eight notes.* / checkpoints.* methods, then pane.get) after upstream's pane.link.resolve assert. Resolve an assert-block conflict as the union of `actual.remove` blocks, upstream first, no method name twice.
Any digest value change = deny (contract change, never a fixture fix). pane.move's digest covers upstream-owned PaneMoveParams/PaneMoveDestination: an upstream reshape fails it after a clean merge, and that is a deny.
tests/fixtures/endpoint-*-v1.json and src/protocol/** frozen tests: never edited (the fork has no diff under tests/).
Upstream adding "pane.move" or "pane.get" to CLIENT_SHELL_METHODS, or adding any section 9 identifier = deny (collision).

## 6. Config keys (cross-checked by scripts/config_reference_check.py)
ui.sidebar_layout, ui.tab_agent_glyphs, ui.tab_agent_glyph_colors, session.backup_agent_transcripts,
keys.toggle_agent_suspend, keys.move_tab_to_group, keys.toggle_groups_folded, keys.restart_agent, keys.cycle_tab_color, keys.toggle_tab_important, keys.open_news (keys.open_browser directly after it, keys.open_coordinator directly after that, in every list),
ui.toast.herdr.sticky, ui.toast.herdr.max_stack, ui.idle_reminder_minutes, ui.daily_reminder_time, ui.sound.reminder_path,
news.enabled, news.times, news.quiet_hours, news.model,
browser.enabled, browser.autostart, browser.stop_with_server, browser.default_profile, browser.executable, browser.node, browser.extra_args, browser.restore_tabs, browser.read_max_chars, browser.snapshot_max_chars, browser.screenshot_max_px, browser.screenshot_keep, browser.active_seconds, browser.allow_eval, browser.allow_act, browser.type_into_password_fields, browser.active_glyph_secs, browser.launch_timeout_ms, browser.op_timeout_ms, browser.show_activity, browser.activity_color, browser.group_symbols, browser.steer_agents, browser.disable_native_browser, browser.wrap_agents, browser.pin_dashboard, browser.mcp_agents, browser.shell_hook, keys.open_browser
coordinator.enabled, coordinator.model, coordinator.cap_hour, coordinator.cap_day, coordinator.periodic_minutes, coordinator.relaunch_cap_hour, coordinator.notify, coordinator.notify_daily_cap, coordinator.quiet_hours, coordinator.dashboard_port, coordinator.wake_scope,
agents.wrap, agents.tools, agents.instructions, agents.instructions_file, agents.notices, agents.team_roster (browser.wrap_agents is legacy: `Option<bool>`, read only as the fallback for an unset agents.wrap through Config::agents_wrap; browser.shell_hook is legacy and read by nothing that edits files; both stay in config-reference.json for old configs and left DEFAULT_CONFIG),
notes.enabled, ui.info_pane_width, keys.toggle_info_pane,
ui.tab_bar_right command entry fields `lines` (u8, default 1, clamped to 1..=4 with a config warning) and `ansi` (bool, default false) are not separate keys: they are documented in the ui.tab_bar_right entry's description in config-reference.json (appended after upstream's sentences) and in configuration.mdx in the paragraph plus example directly after upstream's "Separators appear only between visible entries" paragraph.
Placement in docs/next/website/src/data/config-reference.json: keys.* directly after keys.clear_pane, ui.* directly after ui.sidebar_collapsed_mode (in the order ui.sidebar_layout, ui.tab_agent_glyphs, ui.tab_agent_glyph_colors, ui.idle_reminder_minutes, ui.daily_reminder_time), ui.sound.reminder_path directly after ui.sound.request_path, session.backup_agent_transcripts last in the session group, ui.toast.herdr.sticky and ui.toast.herdr.max_stack directly after ui.toast.herdr.position in the notifications group. The same keys appear as commented defaults in src/main.rs DEFAULT_CONFIG (after clear_pane, sidebar_collapsed_mode, startup_per_agent_delay_ms) and in docs/next/website/src/content/docs/configuration.mdx.
The news.* keys form their own group (id `news`, title News) directly after the session group in config-reference.json and a `[news]` block directly after the `[session]` block in DEFAULT_CONFIG; they are not in configuration.mdx (fork-only feature, no release docs).
The browser.* keys form their own group (id `browser`, title Browser) directly after the news group in config-reference.json and a `[browser]` block directly after the `[news]` block in DEFAULT_CONFIG; not in configuration.mdx either.
The coordinator.* keys form their own group (id `coordinator`, title Coordinator) directly after the browser group in config-reference.json and a `[coordinator]` block directly after the `[browser]` block in DEFAULT_CONFIG; not in configuration.mdx either. `dashboard_port` binds 127.0.0.1 only, 0 disables serving, and a value that is not a port is a diagnostic that keeps the rest of the section. coordinator.wake_scope (`opened` | `teams` | `all`, default `opened`; anything else is a diagnostic that keeps `opened`) is the last key of the group in config-reference.json and the last commented line of the `[coordinator]` block in DEFAULT_CONFIG. The agents model v2 has no config keys of its own: its limits are constants in src/agents_model/limits.rs.
The agents.* keys form their own group (id `agents`, title Agents) directly after the coordinator group in config-reference.json and an `[agents]` block directly after the `[coordinator]` block in DEFAULT_CONFIG; `"agents"` sits in KNOWN_TOP_LEVEL_CONFIG_KEYS after `"advanced"` and loads as a live section after browser (src/config/io.rs); not in configuration.mdx. Defaults: everything off except notices = true and team_roster = true (team_roster works without wrap: a claude / codex launched in a team group gets the team bits only; launches elsewhere are unchanged).
The notes.* keys form their own group (id `notes`, title Notes) directly after the agents group in config-reference.json and a `[notes]` block directly after the `[agents]` block in DEFAULT_CONFIG; `"notes"` sits in KNOWN_TOP_LEVEL_CONFIG_KEYS after `"news"` and loads as the last live section, after agents (src/config/io.rs); not in configuration.mdx. keys.toggle_info_pane sits directly after keys.open_coordinator and ui.info_pane_width directly after ui.daily_reminder_time, in config-reference.json and as DEFAULT_CONFIG comments (not in configuration.mdx).
After any merge touching config-reference.json: python3 -m json.tool on the file, then python3 scripts/config_reference_check.py.

## 7. Per-file merge rules
docs/next/CHANGELOG.md  take-theirs: fork entries live in section 11
docs/next/api/herdr-api.schema.json  take-theirs: after every .rs conflict is resolved, regenerate with HERDR_UPDATE_API_SCHEMA=1 cargo nextest run generated_protocol_schema_artifact_is_current, rerun it clean, require git diff --exit-code docs/next/api/
Cargo.lock  take-theirs+reapply: the fork's one difference is the `toml_edit 0.22.x` line under herdr's own dependencies (already an upstream transitive dependency, so no new package entry); after the merge run `cargo metadata --offline` once to put it back, then the --locked gate verifies
Cargo.toml  take-theirs+reapply: the fork adds one dependency line `toml_edit = "0.22"` right after `toml` (§9 hit `toml_edit`); any other fork difference = deny
skills/herdr/SKILL.md  take-theirs+reapply: re-apply the fork's hunks (two at the time of writing) with git diff <base> <fork tip> -- skills/herdr/SKILL.md | git apply --3way; a hunk that does not apply = deny
docs/next/website/src/data/config-reference.json  additive: upstream entries first, fork entries after (placement in section 6), then json.tool and config_reference_check.py
docs/next/website/src/content/docs/configuration.mdx  additive: upstream first, fork after
src/server/client_commands.rs  section-5
src/cli/agent.rs  additive: the `wrap` and `notify` arms directly after `transcripts` (super::agent_wrap::run, super::agent_notify::run) and their two help lines after the transcripts help line Agents v2: the agent_route check at the top of prompt, send-keys, suspend, activate and restart (agent read is not routed: agent screens are readable by everyone).
src/server/clients.rs  additive: ClientConnection.shell_agent_notices_sent directly after shell_agent_completions (struct and new()); ClientConnection.shell_teams_sent directly after shell_agent_notices_sent (struct and new()).
src/client/endpoint/control.rs  additive: EndpointControlMessage::AgentNotices after AgentCompletions and its decode branch directly after the AgentCompletions branch; EndpointControlMessage::Teams directly after AgentNotices, its decode branch (TEAMS_KIND) directly after the agent-notices branch, teams_round_trip_and_garbage_is_ignored last in the tests.
src/api/schema/common.rs  deny: AgentStatus is append-closed
src/detect/manifests/claude.toml  take-theirs+reapply: the fork's rule `turn_ended_idle` (idle, priority 1150, region above_prompt_box: Claude's turn-end status line, `✻ Waiting for N background agents to finish` or `✻ <Verb> for <time> …`, ends the region) replaces upstream's background_agents_working and background_mcp_task_working; re-add it on upstream's manifest, drop any upstream rule that maps background work to working, keep `version` newer than upstream's and copy the file to distribution/agent-detection/claude.toml (the remote catalog at herdr.dev is upstream's: a newer upstream claude manifest cached by the update check shadows the fork's bundled one until the fork's version is newer again)
distribution/agent-detection/claude.toml  take-theirs+reapply: identical to src/detect/manifests/claude.toml
tests/api_ping.rs  deny: the fork's one line is the protocol literal in ping_over_socket_returns_version; it must equal PROTOCOL_VERSION in src/protocol/wire.rs after the sync
tests/support/mod.rs  deny: the fork's one line is CURRENT_PROTOCOL; it must equal PROTOCOL_VERSION in src/protocol/wire.rs after the sync
src/protocol/wire.rs  deny: PROTOCOL_VERSION is the fork's value (upstream + 3: ClientShellTab.color (23), ClientShellTab.remind with ClientShellAgent.subagents (24), ClientShellTab.important and remind_every replacing remind (25); when upstream bumps, resolve to upstream's new value + 3 and keep the fork comment), the fork's other lines are one inside deserialize_client_shell_agent_status, ClientShellTab.color, .important, .remind_every and ClientShellAgent.subagents (serde default, deliberately not skip_serializing_if: the bincode round-trip needs every field) and `color: None` / `important: false` / `remind_every: None` in the client_shell_snapshot_roundtrip literal (section 2)
src/api/schema.rs  additive: fork Method variants stay directly after AgentStart (NewsRun … NewsSetTimes, then BrowserRun, BrowserGet, BrowserStatus, BrowserFocus, BrowserStart, BrowserStop, BrowserLog, BrowserResolveCaller, BrowserProfiles, BrowserProfileCreate and BrowserProfileDelete close the block); the fork's is_zero stays directly after is_false; `pub mod browser;` and `pub use browser::*;` directly after the agents lines, `pub mod closed_sessions;` and `pub use closed_sessions::*;` directly after them, `pub mod news;` and `pub use news::*;` after those; `pub mod coordinator;` directly after `pub mod commands;` (types only, no `pub use` yet); the coordinator.* Method variants (CoordinatorGet … CoordinatorSetNotify) sit directly after NewsSetQuietHours, before BrowserRun; `pub mod coordinator;` with the other schema modules Agents (wrap + notify): `pub mod agent_notices;` and `pub mod agent_wrap;` directly before `pub mod agents;`, their named `pub use` lists (not `*`: both have `method`/`error_code` modules) directly before `pub use agents::*;`, and the six Method variants AgentNotify … AgentsFix directly after AgentTranscripts. `pub mod team;` directly after `pub mod tabs;`, its named `pub use team::{…};` directly after `pub use tabs::*;`, the nine Method::Team* variants directly after AgentsFix. `pub mod notes;` directly after `pub mod integrations;` (no `pub use`); the notes.* / checkpoints.* Method variants directly after BrowserFix. `pub mod agents_model;` directly after `pub mod agents;` (no `pub use`); the fourteen Method::Agents* variants of the agents model (AgentsActor … AgentsCheck) directly after CheckpointsContext.
src/persist.rs  additive: `pub mod closed_sessions;` directly after `pub mod agent_transcripts;`, `pub mod news;` directly after it, `pub mod browser;` directly after that; the closed-sessions and news lines last in the module doc; `pub mod coordinator;` directly after `pub mod closed_sessions;`
src/app/mod.rs  additive: `mod closed_sessions;` directly after `mod agents;`, `pub(crate) mod news;` directly after it (pub(crate): the server's news_notify reads the queue), `mod browser;` directly after that; `pub(crate) mod coordinator;` directly after `pub(crate) mod news;` `pub(crate) mod agent_notices;` directly after `pub(crate) mod actions;`, `mod agent_wrap_settings;` directly after `pub(crate) mod agent_view;`; App.agent_notices, agents_config, agents_setup_env directly after coordinator. `pub(crate) mod team;` directly after `mod tab_color;`; App.team_tombstones (and the #[cfg(test)] team_follow_calls) after agents_setup_env; AppState team fields in App::new and both rebuild_team_index calls (restore and live handoff) per section 8. `mod notes;` directly after `mod closed_sessions;`, App.notes after team_follow_calls. `mod agents_close;`, `pub(crate) mod agents_migrate;` and `pub(crate) mod agents_model;` directly after `mod agents;` (rustfmt order); App.agents_model directly after App.notes. `pub(crate) mod typing_guard;` (with its doc line) directly after `mod theme_sync;`
src/cli.rs  additive: `mod tab_closed;` directly after `mod tab;`, `mod news;` directly after it, `mod browser;` and `mod browser_mcp;` directly after that; the `news` arm directly after the `session` arm in maybe_run, the `browser` arm directly after it; `mod coordinator;` directly after `mod completion;` (rustfmt order), the `coordinator` arm directly after the `browser` arm `mod agent_notify;` and `mod agent_wrap;` directly after `mod agent;`; `pub(crate) use browser_mcp::STEERING as BROWSER_STEERING;` directly before AGENT_HELP_FOOTER. `mod team;` directly after `mod tab_closed;`, the `team` arm (team::run_team_command; `herdr team hook` is caught before parsing and always exits 0) directly after the `coordinator` arm. `mod notes;` directly after `mod machine;` (rustfmt order), the `notes` and `checkpoint` arms directly after the `coordinator` arm. The agents-model route `pub(super) mod agent_route` (inline) directly after the `mod` list; its tests inside it.
src/client/shell.rs  additive: `mod settings_closed;` directly after `mod settings;`, `mod news;` directly after `mod mouse;`, `mod browser;` and `mod browser_overlay;` directly after it, `mod settings_news;` directly after `mod settings_daily_time;`; `mod coordinator;` and `mod coordinator_shell;` directly after `mod context_menu;`, `mod settings_coordinator;` directly after `mod settings_closed;` `mod agent_cards;` directly after `mod actions;`, `mod settings_agents;` directly before `mod settings_browser;`. `mod team_overlay;` and `mod teams;` directly after `mod tab_remind_menu;`. `mod info_dock;`, `mod info_dock_model;` and `mod info_dock_render;` directly after `mod endpoint_sidebar;` (rustfmt order).
src/api/schema/response.rs  additive: fork ResponseResult variants stay directly after AgentStarted (NewsStatus, NewsGet, NewsHistory, then BrowserRun, BrowserGet, BrowserStatus, BrowserLog, BrowserActor and BrowserProfiles close the block); `use super::browser::{…};` directly after the agents use, `use super::news::{NewsEditionInfo, NewsGetInfo, NewsStatusInfo};` directly after the closed_sessions use; ResponseResult::CoordinatorGet directly after NewsGet ResponseResult::AgentNotify, AgentNotices, AgentsSettings, AgentsFix directly after BrowserSettings. ResponseResult::TeamList, TeamReply, TeamContext directly after AgentsFix. The notes ResponseResult variants (NotesGet, NotesWrite, CheckpointsList, CheckpointWrite, CheckpointContext) directly after TeamContext. The agents-model ResponseResult variants (AgentsActor … AgentsCheck) directly after CheckpointContext.
src/api/schema/agents.rs  additive: fork params types stay after AgentStartParams; AgentInfo.subagents stays directly after state_change_seq; AgentPromptParams.guard_user_typing stays the last field
src/api/schema/panes.rs  additive: PaneReportSubagentParams and SubagentEvent stay last in the file
src/integration/mod.rs  additive: `mod claude_subagent_hooks;` directly after `mod claude_settings;`, `pub(crate) mod news_assets;` directly after it, `pub(crate) mod browser_assets;` directly after that
src/integration/targets.rs  additive: the claude_subagent_hooks install/uninstall lines stay directly after install_claude_settings / uninstall_claude_settings
src/integration/assets/claude/herdr-agent-state.sh  deny: the fork's hunks are `subagent|stop` in the action case, the send() helper, and the `subagent` (agent_type filter) and `stop` (background_tasks snapshot) branches before the SessionStart filter, plus HERDR_INTEGRATION_VERSION=11 (upstream 10 + 1; upstream bumping it = resolve to upstream + 1 in the .sh, the .ps1 and CLAUDE_INTEGRATION_VERSION); re-apply them on upstream's new asset by hand
src/integration/tests.rs  deny: the fork's lines are the two SubagentStop asserts after install_claude (one entry, the subagent hook; none on Windows), the three Stop asserts (one entry, the stop hook; none on Windows) and the expected Claude integration version 11 in the two status tests
src/api/schema/tabs.rs  additive: TabInfo.color, .important, .remind_every stay the last fields; TabColor, TabSetColorParams, TabSetRemindParams, TabRemindInterval, TabRemindEvery and TabSetReminderParams stay after TabInfo
src/cli/tab.rs  additive: the fork's `color`, `important`, `remind`, `closed` and `reopen` arms stay after `rename` (closed and reopen call src/cli/tab_closed.rs), tab_color, tab_important, tab_remind and send_reminder before tab_close, their help lines after the rename line; the fork's tests module stays last Agents v2: the agent_route call at the top of the mutating subcommands (create, rename, close and the fork's color, important and remind through send_reminder; reopen in src/cli/tab_closed.rs).
src/cli/spec.rs  additive: the fork's `closed` and `reopen` subcommands directly before the tab `close` subcommand; `.subcommand(news_command())` directly after session_command() in command(), `.subcommand(browser_command())` directly after it; fn browser_tab_arg, fn browser_common and fn browser_command directly before fn news_command, which stays directly before fn session_command; spec_models_tab_closed_and_reopen, spec_models_news_run_status_and_log, spec_models_news_open_history_enable_and_disable, spec_models_news_times then spec_models_browser_verbs_and_lifecycle directly before spec_models_tab_remind_values agent `wrap` and `notify` subcommands directly after the agent `focus` subcommand; browser `setup --shell` and `wrap` help text describe the Agents wrap. `.subcommand(team_command())` directly after `.subcommand(news_command())`, team_command() directly before news_command(), spec_models_team_verbs directly before spec_models_news_run_status_and_log.
src/api/server.rs  additive: fork api_method_name arms stay after the agent.start arm (the browser.* names directly after news.set_times); `dispatch_to_app_with_timeout` is pub(crate); the coordinator.* api_method_name arms directly after news.set_quiet_hours, before browser.run, naming crate::api::schema::coordinator::method constants The six agent.notify … agents.fix name arms directly after the agent.transcripts arm. The nine team.* api_method_name arms directly after the agents.fix arm. The notes.* / checkpoints.* api_method_name arms directly after browser.fix, naming crate::api::schema::notes::method constants. The agents-model api_method_name arms (agents.actor … agents.check) directly after the checkpoints.context arm, naming crate::api::schema::agents_model::method constants.
src/api/mod.rs  additive: fork arms stay after Method::AgentStart (Method::SessionClosedReopen directly after PaneReportSubagent, Method::NewsRun then Method::NewsOpen directly after it); `pub(crate) use server::dispatch_to_app_with_timeout;` directly after the api_method_name use; Method::CoordinatorOpen, CoordinatorStart and CoordinatorWake directly after Method::NewsOpen in request_changes_ui Method::AgentNoticeDismiss directly after Method::AgentRestart in request_changes_ui. Method::TeamMake, TeamDisband, TeamSetPurpose, TeamSetRole, TeamJoin, TeamLeave directly after Method::AgentNoticeDismiss in request_changes_ui. Method::AgentsOpenTab, AgentsSendMessage, AgentsRenameTab, AgentsMoveTab, AgentsSetMeta, AgentsCloseTab, AgentsReopenTab directly after Method::TeamLeave in request_changes_ui.
src/config/model.rs  additive: upstream first, fork lines directly after each clear_pane line; Config.news then Config.browser the last fields (NewsConfig and BrowserConfig in the super import); HerdrToastConfig sticky/max_stack directly after position (struct, Default, impl HerdrToastConfig after the Default impl), the *_TOAST_MAX_STACK consts directly after MAX_TOAST_DELAY_SECONDS, the *_IDLE_REMINDER_MINUTES consts after them; UiConfig.idle_reminder_minutes and daily_reminder_time directly after tab_agent_glyph_colors (struct and Default), effective_idle_reminder_minutes, effective_daily_reminder_minutes, daily_reminder_diagnostic and idle_reminder_diagnostic last in impl UiConfig, parse_time_of_day directly before impl Default for ToastConfig; KeysConfigOverlay.toggle_tab_important carries `alias = "toggle_tab_remind"`; Config.coordinator after Config.browser (CoordinatorConfig in the super import), KeysConfig/KeysConfigOverlay open_coordinator directly after open_browser in every list Config.agents after coordinator (AgentsConfig in the super import list). Config.notes the last field, after Config.agents (NotesConfig in the super import), KeysConfig/KeysConfigOverlay toggle_info_pane directly after open_coordinator in every list, UiConfig.info_pane_width directly after daily_reminder_time (struct and Default), DEFAULT/MIN/MAX_INFO_PANE_WIDTH after MAX_TOAST_MAX_STACK, effective_info_pane_width and info_pane_width_diagnostic in impl UiConfig.
src/config/tab_bar.rs  additive: MAX_TAB_BAR_COMMAND_LINES directly after MAX_TAB_BAR_RIGHT_ENTRIES, default_command_lines and effective_tab_bar_command_lines after default_command_timeout_seconds, Command.lines and .ansi the last fields of the variant, the lines clamp diagnostic first in tab_bar_right_diagnostics' Command arm (which binds `lines, ansi: _`), the fork test last in the tests module
src/app/tab_bar_status.rs  deny: the fork's hunks are `mod output; use output::StatusOutputFormat;` after the imports, TabBarCommandRuntime.format (last field), `lines, ansi` in configure_tab_bar_status' Command arm and `format: StatusOutputFormat::new(*lines, *ansi)`, spawn_status_command_with_format in handle_tab_bar_status_tasks, spawn_status_command turned into a #[cfg(test)] wrapper, run_status_command's `format` parameter and its two calls into output::, and `lines: 1, ansi: false` in three test literals; re-apply them on upstream's new file by hand
src/config/io.rs  additive: `"news"` and `"browser"` in KNOWN_TOP_LEVEL_CONFIG_KEYS (sorted: browser after advanced, news after keys); the `news` load_live_section call directly after the remote one, the `browser` one last, directly after it; `"coordinator"` in KNOWN_TOP_LEVEL_CONFIG_KEYS directly after `"browser"`, its load_live_section call directly after the news one `"agents"` after `"advanced"` in KNOWN_TOP_LEVEL_CONFIG_KEYS and in the live-section list; its load_live_section call directly after browser's; both loaders fill BrowserConfig.effective_wrap from Config::agents_wrap. `"notes"` in KNOWN_TOP_LEVEL_CONFIG_KEYS directly after `"news"`, its load_live_section call last, directly after the agents one.
src/config.rs  additive: `mod news;` directly after `mod model;`, `mod browser;` directly after it, `news::{format_hhmm, normalize_times, parse_hhmm, parse_quiet_hours, NewsConfig, QuietHours},` in the pub use block directly before the sound line and `browser::{is_forbidden_switch, valid_profile_name, BrowserConfig, AUTO_EXECUTABLE},` directly after it; `tab_bar::{effective_tab_bar_command_lines, MAX_TAB_BAR_COMMAND_LINES},` in the pub(crate) use block directly after the tab_bar group; the fork's `.chain(self.ui.toast.herdr.diagnostic())` then `.chain(self.ui.idle_reminder_diagnostic())` then `.chain(self.ui.daily_reminder_diagnostic())` then `.chain(self.news.diagnostics())` then `.chain(self.browser.diagnostics())` stay last in Config::collect_diagnostics; `mod coordinator;` directly after `mod browser;`, `coordinator::{validate_caps, CoordinatorConfig},` in the pub use block directly before the news line, `.chain(self.coordinator.diagnostics())` directly after the news diagnostics `mod agents;` directly before `mod browser;`, `agents::{AgentsConfig, WrapSource}` first in the pub use list. `mod notes;` directly after `mod model;` (rustfmt order), `notes::NotesConfig,` in the pub use block, `.chain(self.ui.info_pane_width_diagnostic())` in Config::collect_diagnostics.
src/config/write.rs  additive: ConfigEdit::IdleReminderMinutes, SoundFile, DailyReminderTime, NewsEnabled, NewsTimes, NewsQuietHours stay last in the enum and in each match (format_time_of_day directly after the enum, re-exported from src/config.rs); their tests first in the tests module; ConfigEdit::CoordinatorEnabled, CoordinatorWakeCaps, CoordinatorModel, CoordinatorNotify and SidebarLayoutTabs directly after NewsQuietHours (enum, label and apply) ConfigEdit::AgentsBool, AgentsWrap, AgentsWrapWithBrowser, AgentsInstructionsFile replace BrowserBools after BrowserList; document_edit is the fork's shared toml_edit helper. AgentsBool also takes `team_roster` (doc line and its test lines only).
src/sound.rs  additive: Sound::Reminder, ReminderBase, Sound::base and preview directly after the Sound enum; play()'s built-in match goes through base()
src/config/sound.rs  additive: SoundConfig.reminder_path after request_path (struct, Default, path_for arm, diagnostics list); supported_sound_extension directly before impl AgentSoundOverrides; its test before missing_sound_file_produces_diagnostic
src/config/keybinds.rs  additive: upstream first, fork lines directly after each clear_pane line; open_coordinator directly after open_browser in Keybinds, its default and apply_action; toggle_info_pane directly after open_coordinator in Keybinds, its default and apply_action
src/input/keybindings.rs  additive: upstream first, fork lines directly after each ClearPane line; KeybindAction::OpenCoordinator and its binding pair directly after OpenBrowser; KeybindAction::ToggleInfoPane and its binding pair directly after OpenCoordinator
src/input/keybind_help.rs  additive: upstream first, fork entries directly after the clear pane entry; the `open coordinator` entry directly after `open browser`; the `info pane` entry directly after `open coordinator`
src/main.rs  additive: `mod browser;` directly after `mod app;`; DEFAULT_CONFIG comment lines, upstream first (`open_browser` directly after `open_news`); the `[news]` block directly after the `[session]` block, the `[browser]` block directly after it; `mod coordinator;` directly after `mod config;` (rustfmt order); the `open_coordinator` comment line directly after `open_browser`, the `[coordinator]` block directly after the `[browser]` block `mod agent_wrap;`; the `[agents]` DEFAULT_CONFIG block directly after `[coordinator]`; the `[browser]` wrap_agents and shell_hook comment lines are gone. the `team_roster` comment lines last in the `[agents]` DEFAULT_CONFIG block. `mod notes;` directly after `mod metadata_tokens;` (rustfmt order); the `toggle_info_pane` comment line directly after `open_coordinator`, the `info_pane_width` comment block directly after `daily_reminder_time`, the `[notes]` block directly after the `[agents]` block. `mod agents_model;` directly after `mod agent_wrap;` (rustfmt order); the `wake_scope` comment lines last in the `[coordinator]` DEFAULT_CONFIG block.
src/client/shell/state.rs  additive: upstream first, fork after; ClientSettingsSection::ALL keeps Backups, Reminders, ClosedSessions then News last; ClientSettingsOverlay.closed sits between transcripts and loading_transcripts, .news is the last field; ClientSettingsSection::Coordinator last after Browser, ClientSettingsOverlay.coordinator after .browser, ClientShellState.coordinator after .browser, ShellHitMap.coordinator_row after browser_row ShellHitMap.agent_cards after notification_toasts; ClientSettingsSection::Agents last; ClientSettingsOverlay.agents last; PendingEndpointKind::AgentsSettings/AgentsSettingsSet/AgentsFix between BrowserFix and Coordinator; ClientShellState.agent_cards after visible_notifications. ClientShellConfig.info_pane_width, ClientShellLayout.info_dock, ShellHitMap.info_dock, ClientInputContext.info_dock_focused and ClientShellState.info_dock / info_dock_width / info_dock_width_manual are the last fork fields of their structs; ClientChromeDrag::InfoDockWidth, ClientContextMenuAction::ToggleInfoPane and PendingEndpointKind::Info* last. ClientRenameTarget::AgentRole and ClientContextMenuAction::SetRole last; ClientTabMenuAgent.set_role the last field.
src/client/shell/tests/mod.rs  additive: fork module lines (`mod browser;` directly after `mod news;`) `mod agent_cards;` and `mod settings_agents;`. `mod team_overlay;` and `mod teams;` directly after `mod sticky_notifications;`. `mod info_dock;` directly after `mod graphics;`. `mod agents_model;` directly after `mod agent_cards;`.
src/server/headless/tests/mod.rs  additive: the fork_smoke module line; test literals per section 2 `mod agent_notices_smoke;` next to the fork_smoke module line. `mod teams_smoke;` (with its #[path]) directly after `mod agent_notices_smoke;`. `mod agents_model_smoke;` (with its #[path]) directly after `mod agent_notices_smoke;`.
src/workspace.rs  additive: `pub mod team;` directly after `mod tab;`; Workspace.team the last field (`team: None` in every literal, section 2); Workspace::assert_invariants_for_test checks the team (members and exclusions live in this workspace, disjoint, unique, one-line caps, change log <= 16); test_adversarial_identity_state adds a team.
src/persist/snapshot.rs  additive: WorkspaceSnapshot.team (`#[serde(default, skip_serializing_if = "Option::is_none")]`) the last field, TeamSnapshot and TeamMemberSnapshot (old raw pane ids, optional pending_rename) after WorkspaceSnapshot, capture_workspace copies ws.team through TeamSnapshot::capture. PaneSnapshot.agent_meta the last field, SuspendedAgentSnapshot.suspended_by the last field (both `#[serde(default, skip_serializing_if = "Option::is_none")]`); capture_tab fills both.
src/app/state.rs  additive: AppState.team_index, team_count, teams_view_rev directly after coordinator_terminal_id (and test_new); assert_invariants_for_test checks the index, the count, that no member is the coordinator pane and that teams_view_rev >= 1 while any team exists. AppState.agents_close_deadline directly after teams_view_rev (and test_new); remember_public_alias, restore_public_aliases_from_meta and adopt_member_roles_into_meta after the team helpers; assert_invariants_for_test checks that a member's role mirror equals its pane meta role.
src/events.rs  additive: AppEvent::NotesWorkerFinished last, directly after CoordinatorPassFinished
src/cli/spec.rs  additive: `.subcommand(notes_command())` and `.subcommand(checkpoint_command())` directly after the coordinator subcommand in command(); fn notes_target_args, fn notes_command and fn checkpoint_command directly after fn news_command; spec_models_notes_verbs and spec_models_checkpoint_verbs last in the tests module
src/server/headless/client_views.rs  additive: the `notes_geometry_tests` module last in the file
src/server/headless/tests/fork_smoke.rs  additive: the `#[path = "fork_smoke/notes.rs"] mod notes;` lines last
src/client/shell/preferences.rs  additive: ClientChromePreferences.info_dock_width the last field (serde default, skip_serializing_if none)
src/cli/pane.rs  additive: agents v2: the agent_route call at the top of move, close, read, send-text, send-keys and run (pane_is_its_tabs_last, checked_shell_input and agent_read_through_model are fork helpers)
src/cli/workspace.rs  additive: agents v2: `workspace close` refuses (agent_refused) on the agent route before any request
*  deny: anything that is not a structural additive conflict (zdiff3 base empty, both sides pure insertions)

## 8. Extended surfaces (upstream touch forces human review in the report, even when green)
src/client/shell/notifications.rs  mid-logic: render_visible_notification and render_mobile_notification_banner swap the drawn `●` for notification_glyph (reminder marker, `✓` finished, `×` needs attention) via put_notification_glyph after the upstream render; render_notification_card and render_mobile_notice_banner are unchanged
src/terminal/state.rs  mid-logic: set_detected_state_with_screen_signals_at (suspend reconcile, hook-clear durable session, name kept on exit), clear_full_lifecycle_hook_suppression_for_detected_agent (replacement sessions), set_agent_session_ref_for_session_start (launch-session identity), release_agent_with_mutation, managed_agent_launch_pending, managed_agent_interactive_ready, managed_agent_kind, reconcile_managed_agent_at, clear_agent_name, clear_agent_runtime_identity_after_respawn; active_subagents (with subagent_snapshot_seen) is forgotten in recompute_effective_state on an agent label change (before the early return; before the first snapshot an Idle state also clears the set, the older Claude rule; a session other than subagent_session clears it too), release_agent_with_mutation, begin_agent_suspend and clear_agent_runtime_identity_after_respawn; a turn end and an Unknown with the same label keep it; the set never changes the status (recompute_effective_state uses the detected state as upstream does; background subagents are not a turn); report_subagents_with_mutation (start / stop / snapshot through the effective state) is the only path the API uses; active_subagent_count is len() in any state, 0 while suspended Agents v2: TerminalState carries agent_meta (persisted through PaneSnapshot.agent_meta), turn (runtime only; mark_restored makes a restored or handed-off terminal start Unknown) and created_unix; SuspendedAgent.suspended_by.
src/app/actions.rs  mid-logic: expire_agent_metadata_at, handle_app_event (transcript path before session routing), update_terminal_state_with_completion_policy (suspended in the captured tuple, dirty and completion suppression); update_terminal_state is pub(crate) (src/app/subagents.rs routes subagent reports through it, so a report that clears the set gets the usual event handling)
src/app/agent_suspend.rs  mid-logic: suspend_resolved_agent refuses with SubagentsRunning before the Working check (an idle agent can still have background subagents) (no input written)
src/app/api.rs  mid-logic: emit_pane_state_update (status computed with suspended on both sides); handle_api_request dispatches Method::NewsRun / NewsStatus / NewsGet / NewsHistory / NewsOpen / NewsSetEnabled / NewsSetTimes directly after the SessionClosedRemove arm, then Method::BrowserRun (answers browser_lane: it belongs to the connection thread) and BrowserGet / BrowserStatus / BrowserFocus / BrowserStart / BrowserStop / BrowserLog / BrowserResolveCaller / BrowserProfiles / BrowserProfileCreate / BrowserProfileDelete / BrowserSettings / BrowserSettingsSet / BrowserFix (handlers in src/app/browser.rs) directly after NewsSetTimes
src/app/browser.rs  mid-logic: install_browser_hub calls hub.refresh_checks(false) after the autostart (not under cfg(test), only with [browser] enabled); handle_browser_get calls hub.refresh_checks_if_older(5 min) and sets browser.setup_needed from hub.setup_needed(); set_browser_setting maps the key to a ConfigEdit::Browser* variant (steer_wrap → BrowserBools), writes it, calls App::reload_config, compares hub.config()'s value with the request (browser_config_value; mismatch = browser_config_write_failed, no fix), then hub.run_fixes for mcp_agents / shell_hook or hub.refresh_checks(true)
src/browser/hub.rs  mid-logic: start's `herdr-browser-start` thread and ensure_running (after the already-attached early return, before the attach/launch decision) call auto_repair_once (setup::auto_repair under a std::sync::Once, never in test_mode); get() fills setup_needed from the cached checks; Inner carries setup: SetupState (checks, checked_at, checking, fixing, fixes, pending, generation) and setup_env_override (tests)
src/app/api/agents.rs  mid-logic: queue_agent_prompt and handle_agent_send_keys refuse suspended panes; any new upstream input method does not
src/app/api/panes.rs  mid-logic: handle_pane_report_agent and handle_pane_report_agent_session derive transcript_path
src/app/api_helpers.rs  mid-logic: status mapping moved to workspace::aggregate
src/app/creation.rs  mid-logic: tab_info (color, important, remind_every), pane_info, workspace_info, terminal_agent_session_info
src/app/mod.rs  mid-logic: App::new (news field from NewsState::new; `app.install_browser_hub(&config.browser)` directly after configure_window_title, before `app` is returned), apply_live_config (session section block restructured; a `news` section block directly before the graphics check applies NewsState::apply_config, a `browser` section block directly after it hands config.browser to crate::browser::hub())
src/server/headless.rs  mid-logic: the run loop's exit calls crate::browser::hub().stop_with_server_if_configured() directly after save_session_on_shutdown, skipped while handoff_in_progress (the replacement server reattaches to the same Chromium)
src/api/server.rs  mid-logic: handle_connection_with_stop runs Method::BrowserRun on the connection thread (crate::browser::serve::run, answered through finish_wait_response) in an arm directly after the PaneWaitForOutput arm, before `method_body =>`
src/app/session.rs  mid-logic: save_session_on_shutdown (early return became if/else, backup pass appended)
src/app/agents.rs  mid-logic: rename refuses suspended; live_runtime_agent split into live_runtime_agent_job; agent_info sets subagents from active_subagent_count
src/app/agent_view.rs  mid-logic: apply_agent_view, validate_field_value, status_name
src/app/agent_resume.rs  mid-logic: start_pending_agent_resume restores the transcript backup before the resume command
src/app/runtime.rs  mid-logic: next_headless_loop_deadline_with_git_refresh gains four deadlines (suspend exit, restart resume, transcript backup, next_news_deadline directly after next_tab_bar_status_deadline)
src/workspace/aggregate.rs  mid-logic: pane_details, aggregate_state, agent_status, agent_status_priority
src/persist/snapshot.rs  mid-logic: capture_tab agent_session block rewritten to persistable_agent_session; capture_tab copies Tab.color, important (also as the legacy `remind`) and remind_every into TabSnapshot
src/persist/restore.rs  mid-logic: restore_tab (resume disabled for suspended panes, handoff exit wait, color restored with Unknown dropped to None, important from important or the legacy remind, remind_every with Unknown dropped), unavailable_restored_terminal, pane_restore_startup, persisted_agent_session_from_snapshot
src/server/headless.rs  mid-logic: `mod news_notify;` directly after `mod lifecycle;`; handle_scheduled_tasks_headless (backup pass, suspend escalation, start_pending_agent_restarts, handle_news_tasks directly after handle_tab_bar_status_tasks, then flush_news_notifications); the client-connected block of handle_server_event ends with flush_news_notifications (a queued news notification goes out when a client shell attaches) directly before its `true`
src/server/headless/lifecycle.rs  mid-logic: perform_live_handoff sets suspended_exit_pending
src/pane.rs  mid-logic: handoff_runtime_state, from_handoff_fd
src/protocol/wire.rs  mid-logic: deserialize_client_shell_agent_status
src/client/shell.rs  mid-logic: status_priority renumbered (Unknown 0 -> 1, Suspended 0), status_icon, status_text, status_color
src/client/shell/input.rs  mid-logic: push_pane_key and push_focused_pane_event are the only key/text/paste lock points; indexed_navigation_target_exists checks SwitchTab(n) against keyboard_tab_list (the list the action indexes; the pinned News tab left out in the tabs layout)
src/config/sound.rs  mid-logic: SoundConfig::diagnostics accepts supported_sound_extension (mp3; on macOS also aiff, aif, caf, m4a) instead of mp3 only; path_for returns early for Sound::Reminder
src/server/headless.rs  mid-logic: sound_notify_message gains a Sound::Reminder arm (never sent)
src/app/actions.rs  mid-logic: the client notification kind match treats Sound::Reminder like Done
src/client/shell_runtime.rs  mid-logic: the action loop plays ClientShellAction::PreviewSound
src/client/shell/mouse.rs  mid-logic: handle_mouse (the Browser row: a left press on hits.browser_row opens the Browser overlay and a right-click its menu, both checked directly before the News row checks; the News row: a left press on hits.news_row goes to activate_news_row before the sidebar tab press is taken (tab.focus on the News tab, or news.open while the row shows without one), and a right-click opens the News menu before the tab-row lookup; sidebar tab drag, group menu, locked-pane gestures, notification card hits: timed left-click keeps the upstream pane_id gate, sticky left focuses / right dismisses / the fold line swallows), push_pane_mouse_event; the ContextMenu block asks route_tab_color_swatch_mouse, then route_tab_remind_option_mouse first (swatch / reminder option hover and click); a settings choice click also applies at once when reminders_click_applies (the reminders tab's daily time row and picker) or news_click_applies (every row of the news tab); a settings row click applies at once in the Browser section too (browser_click_applies)
src/client/shell/surface_patch.rs  mid-logic: fast_path_blocker else-if for suspended panes; the notification arm calls notification_blocks_patch (timed: any card blocks, as upstream; sticky: only patch rows over a drawn card)
src/client/shell/composition.rs  mid-logic: both ShellRenderState literals pass news_row, browser_row and browser_marked_tabs (computed by news_row(), browser_row() and browser_marked_tabs() before the mutable borrows); compose paints the suspended card and occludes graphics; compose draws the sticky notification stack (render_notification_stack), occludes every card rect, fills hits.notification_toasts, and hands the stack bounds to copy_feedback_offset_for_toast; the context menu branch copies rendered.menu_swatches and menu_remind_options into the hit map; both ShellRenderState literals pass idle_reminders, scheduled_reminders, breathe_phase and breathe_reset_rgb (computed before the mutable borrows); render_client_overlay gets &self.config
src/client/shell/render.rs  mid-logic: render_shell else-if for the tabs layout
src/client/shell/config.rs  mid-logic: layout (show_tab_bar), from_config (browser_active_glyph_secs from config.browser.active_glyph_secs), apply_live_config (sound_files, daily_reminder_minutes, browser_active_glyph_secs), reload_client_config (rebalance_notification_cards after a sticky flip)
src/client/shell/notification_policy.rs  mid-logic: retire_endpoint_notifications, queue_visible_notification (sticky push), promote_queued_notification, focus_visible_notification (split into focus_notification_at), receive_notification (replace-by-pane moved into replace_pane_notifications), tick_notifications (starts with tick_idle_reminders, whose due reminders it delivers from the pending list; a pending reminder's sound becomes Sound::Reminder; a validated pending event is passed through format_agent_notification before the target/sound/delivery code, so every path gets the `tabs` layout text; expiry gated on !toast_sticky); notification_validation is reused by sticky_notification_is_stale; notification_target_is_active is the idle reminders' focus test
src/client/shell/endpoints.rs  mid-logic: cache_endpoint_snapshot_with_surface ends with prune_sticky_notifications
src/client/shell/machine_diagnostics.rs  mid-logic: handle_machine_badge_event also yields to hits.notification_toasts
src/client/shell/state.rs  mid-logic: ClientShellState::new initialises visible_notifications, the reminder maps, news and browser; timer_delay chains next_idle_reminder_deadline, next_news_deadline (the running row's minute clock), next_breathe_deadline (while a breathing glyph was drawn), next_browser_deadline (the browser.get cadence) and next_browser_settings_deadline (the settings browser section's poll while the server checks or fixes); ClientShellState carries pending_browser_wrap (the section's steer + wrap row writes two keys); ClientShellState::new sets breathe_epoch (and a fixed breathe_clock under cfg(test))
src/client/mod.rs  mid-logic: the Timer arm's repaint chain ends with shell.tick_breathing(now)
src/config.rs  mid-logic: Config::collect_diagnostics chains tab_agent_glyph_color_diagnostics and HerdrToastConfig::diagnostic
src/client/shell/tests/graphics.rs  depends: assert_graphics_cover is pub(super) for tests/sticky_notifications.rs; its ClientContextMenuTarget::Tab literal carries `color: Default::default()` and its ClientSettingsOverlay literal `news: Box::default()` (section 2)
src/client/shell/actions.rs  mid-logic: record_binding (topology lock, group keys; OpenBrowser opens the Browser overlay directly after the Settings branch), endpoint_method_for_action (SwitchTab/NextTab/PreviousTab index keyboard_tab_list — the sidebar's order with the pinned News tab left out in the tabs layout, the focused space in the spaces layout; Next/Previous from the News tab land on the first/last entry —, CycleTabColor, ToggleTabImportant, OpenNews -> news.open); the `impl ClientShellState` block with keyboard_tab_list at the end of the file
src/client/shell/context_menu.rs  mid-logic: items (a Browser target lists Focus window and Stop profile or Start profile on a local endpoint, then Open overlay; a News target lists Run now, Open, Pause / Resume schedule; after Close, so upstream item indices hold: the important toggle, the two reminder selector rows, then the swatch row `Color` last), activate_context_menu_item's target match sends News to activate_news_context_action and Browser to activate_browser_context_action, open_tab_context_menu (captures the tab color, important and remind_every into the Tab target), activate_context_menu_item (the swatch row, Important and the selector act before the target dispatch, so no tab focus; the Tab arm ignores `color`, `important` and `remind` with `..`)
src/client/shell/overlay_input.rs  mid-logic: route_overlay_key hands a Browser overlay's keys to route_browser_overlay_key first (before the Onboarding check); save_rename_overlay, accept_close_confirmation (close_group now from the overlay); the ContextMenu key block asks route_tab_remind_menu_key, then route_tab_color_menu_key first (Up/Down move between and re-seat the selector and swatch cursors, Left/Right/h/l on either row)
src/client/shell/overlays.rs  mid-logic: render_context_menu widens a tab menu to the swatch row and the reminder selector, draws the swatches and the selector's options in place of their items' labels (only the cursor one highlighted, while its row is) and returns their rects as menu_swatches / menu_remind_options; render_client_overlay passes the config to the settings overlay and draws ClientShellOverlay::Browser through browser_overlay::render_browser_overlay (an arm directly before the ContextMenu | GlobalMenu arm)
src/client/shell/tabs.rs  mid-logic: render_tab_bar tints unfocused tabs with their color tag (focused tab unchanged)
src/app/api/tabs.rs  mid-logic: handle_tab_close captures closed_session_entries_for_workspaces (last tab) or closed_session_entries_for_tab before the close and calls record_closed_sessions after it, in both branches; a new upstream close path records nothing
src/app/api/panes.rs  mid-logic: close_pane captures closed_session_entries_for_workspaces (the pane closes its space) or closed_session_entries_for_pane before ws.close_pane and records after remove_plugin_pane_records; handle_pane_move records nothing (a moved tab is not closed)
src/app/api/workspaces.rs  mid-logic: handle_workspace_close captures closed_session_entries_for_workspaces(close_indices) before close_selected_workspace and records after shutdown_detached_terminal_runtimes
src/app/api/panes.rs  mid-logic: handle_pane_move (PaneMoveRecoveryContext.previous_tab_color, previous_tab_important and previous_tab_remind_every; a whole-tab move applies them to the NewTab / NewWorkspace tab), recover_failed_pane_move restores them Agents v2: pane.send_text, pane.send_keys and pane.run record Programmatic::Api with note_pane_input before writing; a pane moved to another tab remembers its previous public id (remember_public_alias).
src/client/shell/settings.rs  mid-logic: selected_index_for_settings_section (News and Browser 0), select_settings_section (enter_browser_section for Browser; refreshes idle_reminder_minutes, closes a sound picker and a daily time picker, requests session.closed_list on entering `closed`, enter_news_section on entering `news`), settings_choice_count (Sound counts sound_section_rows, Reminders counts reminders_section_rows, ClosedSessions the filtered rows, News news_section_rows), apply_settings_choice (Sound goes to apply_sound_choice, Reminders asks apply_daily_time_choice first, then writes ConfigEdit::IdleReminderMinutes, ClosedSessions reopens, News goes to apply_news_choice), route_settings_key (Esc closes an open sound, daily time or news picker first, then route_closed_sessions_key takes filter text, Backspace, Delete / `d` and Esc on a non-empty filter; Up/Down preview its sound), handle_settings_endpoint_result; settings_choice_count (Browser => browser_section_rows), apply_settings_choice (Browser => apply_browser_choice), route_settings_key (route_browser_key after route_news_key: → cycles the colour and MCP rows); open_settings_overlay initialises `browser: Box::default()`; ClientSettingsOverlay.sound_picker is Option<Box<ClientSoundPicker>> since the browser section (the overlay enum's size lint)
src/client/shell/settings_overlay.rs  mid-logic: render_settings_overlay (popup width 84 — ten tabs in one row — and height 32 for the Browser section, the Browser arm renders settings_browser::render_browser_section, show_primary for Browser while its record is there; show_primary match, ClosedSessions shows ` ↵ reopen ` while rows are listed; the section tab strip drops its one-cell gaps when it does not fit the 74-column inner width, and then drops each label's padding (settings_tab_label) and keeps the gaps, which with eight sections is always; Sound renders render_sound_section (rows or the open picker); Reminders arm: render_reminders draws the daily time row below the intervals, or the open daily time picker; ClosedSessions renders settings_closed::render_closed_sessions; News renders settings_news::render_news_section and shows the primary button only once a record is listed)
src/client/shell/actions.rs  mid-logic: push_endpoint_method_with_kind treats SessionClosedReopen and NewsOpen as focus-changing; handle_endpoint_result sends the SessionClosed* kinds to handle_closed_sessions_endpoint_result, the News* kinds to handle_news_endpoint_result and the Browser* kinds to handle_browser_endpoint_result; handle_endpoint_result routes BrowserSettings / BrowserSettingsSet / BrowserFix to handle_browser_settings_endpoint_result
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists the SessionClosed*, Browser{Settings,SettingsSet,Fix}, News* and Browser* kinds
src/client/shell/browser.rs  mid-logic: tick_browser first calls tick_browser_settings (the settings browser section's follow-up pulls and the steer + wrap row's second write); browser_row_state appends ` !` to a Running/Stopped row's status while browser.get says setup_needed (a crash or an open dialog outranks it); `herdr browser status` prints a `setup: needed — …` line for the same flag
src/client/shell/endpoint_navigation.rs  mid-logic: finish_endpoint_workspace_press early return in the tabs layout
src/client/mod.rs  mid-logic: the client loop's tick block calls shell.tick_news(now, &mut outcome) directly after tick_notifications and shell.tick_browser(now, &mut outcome) directly after that (the only non-input producers of endpoint requests: news.get and browser.get are pulled from there)
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
src/platform/unix_common.rs  mid-logic: signal_process_group (kill -SIG -pgid; the news watchdog's SIGTERM to the News pane's foreground job), re-exported by macos.rs and linux.rs in their `pub(crate) use super::unix_common` list
src/platform/windows.rs  mid-logic: signal_process_group no-op directly before process_exists
src/platform/fallback.rs  mid-logic: signal_process_group stub directly before process_exists
src/app/state.rs  mid-logic: AppState.coordinator_terminal_id (mirrored by app/coordinator.rs sync_coordinator_suppression: set only while the phase is Starting, Launching or Running) is the last field
src/app/actions.rs  mid-logic: update_terminal_state_with_completion_policy's suppress_completion also holds for the coordinator's terminal (AppState.coordinator_terminal_id), so its Working→Idle raises no Finished toast or Done sound; handle_app_event's AppEvent::CoordinatorPassFinished arm
src/app/agent_suspend.rs  mid-logic: emit_agent_status_transition first marks the coordinator's pass input dirty (mark_coordinator_input_dirty)
src/app/api.rs  mid-logic: emit_pane_updated first marks the coordinator's pass input dirty; emit_pane_state_update marks it on an agent label change or release and on every agent status change; emit_event first marks it for the structural kinds (workspace created/closed/renamed/moved/reordered, tab created/closed/renamed/moved, pane created/closed/moved/exited); handle_app_event applies AppEvent::CoordinatorPassFinished (apply_coordinator_output) directly before WorktreeReadFinished; handle_api_request dispatches the nine Method::Coordinator* arms directly after the news ones
src/app/mod.rs  mid-logic: App::new (coordinator field from CoordinatorState::new), apply_live_config (a `coordinator` section block directly after the news one calls apply_coordinator_config)
src/app/runtime.rs  mid-logic: next_headless_loop_deadline_with_git_refresh chains next_coordinator_deadline directly after next_news_deadline
src/server/headless.rs  mid-logic: `mod coordinator_notify;` directly after `mod news_notify;`; handle_scheduled_tasks_headless runs handle_coordinator_tasks then flush_coordinator_notifications directly after handle_news_tasks
src/events.rs  mid-logic: AppEvent::CoordinatorPassFinished (the coordinator worker's output, sent with try_send from the herdr-coordinator worker)
src/app/agents.rs  mid-logic: start_agent's shell check has a #[cfg(test)] coordinator.assume_shell_ready seam (the smoke tests' stub pane) Agents v2: an agent start writes its command as Programmatic::Api (note_input).
src/app/api/agents.rs  mid-logic: queue_agent_prompt is pub(in crate::app) and its runtime_hosts_agent check has a #[cfg(test)] coordinator.assume_shell_ready seam
src/platform/mod.rs  mid-logic: coordinator_supported() (cfg!(unix): the coordinator needs the public JSON API socket)
src/server/client_commands.rs  mid-logic: CLIENT_SHELL_METHODS lists the nine coordinator.* methods (sorted), every_coordinator_method_is_advertised_to_client_shells checks them
src/client/mod.rs  mid-logic: the client loop's tick block calls shell.tick_coordinator(now, &mut outcome) directly after tick_news
src/client/shell/actions.rs  mid-logic: push_endpoint_method_with_kind treats CoordinatorOpen as focus-changing; handle_endpoint_result routes PendingEndpointKind::Coordinator; KeybindAction::OpenCoordinator → coordinator.open; keyboard_tab_list leaves out the pinned coordinator tab (coordinator_pinned_tab_id) like the News tab
src/client/shell/mouse.rs  mid-logic: handle_mouse (the coordinator row: a left press on hits.coordinator_row focuses or opens it, a right-click opens its menu, both directly after the News row checks; a settings click applies at once in the coordinator section)
src/client/shell/composition.rs  mid-logic: both ShellRenderState literals pass coordinator_row (coordinator_row()) and coordinator_managed_tabs
src/client/shell/render.rs  mid-logic: render_shell calls tab_sidebar::render_tab_sidebar_with (the coordinator row and managed marks) and stores hits.coordinator_row; render_tab_bar gets the coordinator tab mark outside the tabs layout
src/client/shell/tabs.rs  mid-logic: render_tab_bar appends the coordinator mark (`● 2 ideas`) to the coordinator tab's label through a local tab_label closure
src/client/shell/context_menu.rs  mid-logic: items and activate route ClientContextMenuTarget::Coordinator to coordinator_shell.rs
src/client/shell/settings.rs  mid-logic: open_settings_overlay (coordinator: Box::default()), selected_index_for_settings_section (Coordinator 0), select_settings_section (enter_coordinator_section), settings_choice_count, apply_settings_choice and the esc picker chain (close_coordinator_picker)
src/client/shell/settings_overlay.rs  mid-logic: render_settings_overlay is 104 columns wide (twelve section tabs; when even the bare labels and gaps do not fit, the gaps go), the agents section 32 rows (render_agents_section; the primary button only with a record and not while the [fix] is armed), the browser section 30 rows, the coordinator section 30 rows, renders render_coordinator_section, the primary button only with a record
src/client/shell/idle_reminders.rs  mid-logic: is_news_tab also exempts the coordinator tab (is_coordinator_tab) from both reminder engines
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists PendingEndpointKind::Coordinator(_)
src/client/shell/tests/graphics.rs  depends: its ClientSettingsOverlay literal carries `coordinator: Box::default()` and `agents: Box::default()`
src/app/api.rs  mid-logic: emit_event drops/remaps agent cards on PaneMoved, PaneClosed, PaneExited, TabClosed, WorkspaceClosed (follow_agent_notice_panes, only with cards) and re-sends them on TabRenamed/WorkspaceRenamed when a card is in the renamed tab or space (follow_agent_notice_labels, only with cards); handle_app_event follows them after an AppEvent::PaneDied; handle_api_request dispatches Method::AgentNotify … AgentsFix directly after AgentTranscripts
src/app/mod.rs  mid-logic: App::new (agent_notices from config.agents.notices, agents_config, agents_setup_env None), apply_live_config (an `agents` section block directly after the browser hub block: notices on/off and agents_config)
src/server/headless/render.rs  mid-logic: the per-client render pass calls agent_notices::sync_client (one PassFrames per pass, O(1) per client when the revision was sent) directly after the agent completions block
src/server/headless/client_views.rs  mid-logic: focus_shell_client_on_tab clears the entered tab's agent cards when the client's focused tab changes; apply_shell_navigation_request's WorkspaceFocus arm does the same for the workspace's active tab (both client-driven only; focus_all_shell_clients_on_default_target never visits)
src/server/headless.rs  mid-logic: `pub mod agent_notices;` directly before `mod bootstrap;`
src/client/mod.rs  mid-logic: the endpoint control match handles EndpointControlMessage::AgentNotices (receive_agent_notices, its sound/terminal effects, a recompose) directly before the Ignored arm
src/client/shell/composition.rs  mid-logic: compose draws the agent cards (render_agent_cards under the tab bar, below a top-right toast stack; the mobile banner otherwise) after the notification toasts, only when has_agent_cards(); the switcher clears hits.agent_cards
src/client/shell/mouse.rs  mid-logic: handle_mouse tries handle_agent_card_mouse before the toast hit test (Terminal mode, no overlay); the settings overlay's left press goes through route_agents_click first (the armed [fix] buttons) and agents_click_applies
src/client/shell/surface_patch.rs  mid-logic: fast_path_blocker else-if agent_cards_block_patch directly after the notification arm
src/client/shell/settings.rs  mid-logic: open_settings_overlay (agents: Box::default()), select_settings_section disarms the agents [fix] and calls enter_agents_section, the key router gives route_agents_key ↵/esc first, apply_settings_choice routes Agents
src/client/shell/actions.rs  mid-logic: handle_endpoint_result routes PendingEndpointKind::AgentsSettings/AgentsSettingsSet/AgentsFix to handle_agents_settings_endpoint_result
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists AgentsSettings, AgentsSettingsSet, AgentsFix
src/app/api.rs  mid-logic: emit_event calls follow_teams (src/app/team.rs) on PaneMoved, PaneClosed, PaneExited, TabClosed, WorkspaceClosed, WorkspaceRenamed, TabRenamed, PaneAgentDetected, PaneAgentStatusChanged only while AppState.team_count > 0; handle_app_event reconciles teams after an AppEvent::PaneDied (pane.exited is emitted before the pane goes); handle_api_request dispatches Method::TeamList … TeamContext directly after AgentsFix
src/app/api/agents.rs  mid-logic: handle_agent_rename reads team_member_before_rename before and calls team_follow_agent_rename after a successful rename (a member's new name is a roster change) Agents v2: queue_agent_prompt takes the InputSource and records it; agent.prompt passes Programmatic::Api, agents.send_message AgentMessage, the coordinator wake HerdrWake.
src/app/mod.rs  mid-logic: App::new rebuilds the team index after restore (state.rebuild_team_index()), the live-handoff constructor does the same
src/persist/restore.rs  mid-logic: restore_workspace unions the per-tab reverse_id_maps, remaps WorkspaceSnapshot.team's member and excluded pane ids, drops entries whose tab did not survive, resets seen revisions and status_since_unix Agents v2: restore_tab reinstates the pane's agent meta (reinstate_agent_meta) and suspended_by, marks every restored terminal restored (turn Unknown), and the public id aliases are rebuilt from the meta after restore and live handoff.
src/server/headless/render.rs  mid-logic: the per-client render pass calls teams::sync_client (one PassFrame per pass, built only when a client is behind; O(1) per client when the revision was sent) directly after the agent notices block
src/server/headless.rs  mid-logic: `pub mod teams;` directly after `mod retained_surface;` Agents v2: every client write to a pane (client pane input events, terminal attach input, the clipboard image paste) records its InputSource with App::note_pane_input / note_terminal_input next to the runtime write (pane_input::client_input_source / attach_input_source classify it; mouse reports and key releases are not input); the scheduler tick runs drive_pending_agent_closes and maybe_run_agents_migration before the suspended-exit escalation.
src/server/client_commands.rs  mid-logic: CLIENT_SHELL_METHODS lists the seven team.* client-shell methods (sorted; 84 entries); team_methods_are_advertised_except_the_list_and_the_agent_read and team_params_reference_no_existing_schema_type
src/client/mod.rs  mid-logic: the endpoint control match handles EndpointControlMessage::Teams (receive_teams, a recompose) directly after AgentNotices; the client loop's tick block calls shell.tick_teams(now, &mut outcome) directly after tick_coordinator
src/client/shell/render.rs  mid-logic: ShellRenderState.teams (the active endpoint's ClientTeamsState) passed into TabSidebarCoordinator for the group header mark and the member row mark (O(1) lookups, only while its boot_id matches the snapshot)
src/client/shell/composition.rs  mid-logic: both ShellRenderState literals pass teams (teams::active_teams_of); compose draws ClientShellOverlay::TeamInfo through team_overlay::render_team_overlay and records hits.team_overlay, team_overlay_popup and overlay_cancel
src/client/shell/context_menu.rs  mid-logic: items (group menu: "Ungroup (disbands team)" relabel and the team items after Close group; tab menu: the team items after Close), activate (a team group's Ungroup confirms first when confirm_close, the team actions route to teams.rs)
src/client/shell/mouse.rs  mid-logic: handle_mouse hands every mouse event to the Team info overlay while it is open (route_team_overlay_click, scroll moves its cursor); a click outside a team Rename modal cancels it through cancel_rename_overlay (reopening Team info)
src/client/shell/overlay_input.rs  mid-logic: route_overlay_key hands Team info keys to team_overlay.rs; the Rename modal's TeamPurpose / TeamRole targets send team.make / team.set_purpose / team.set_role and reopen Team info on close; ConfirmClose with `ungroup` runs the ungroup
src/client/shell/overlays.rs  mid-logic: render_client_overlay returns None for ClientShellOverlay::TeamInfo (composed by team_overlay.rs, like the context menus)
src/client/shell/state.rs  mid-logic: ClientShellState::new initialises teams; ClientShellOverlay::kind maps TeamInfo
src/client/shell/actions.rs  mid-logic: handle_endpoint_result routes PendingEndpointKind::Team to handle_team_endpoint_result
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists PendingEndpointKind::Team(_)
src/client/shell/tests/graphics.rs  depends: its ClientConfirmCloseOverlay literal carries `ungroup: false` and its ClientContextMenuTarget literal `team: None`
src/app/api.rs  mid-logic: handle_app_event returns early for AppEvent::NotesWorkerFinished (handle_notes_worker_finished); handle_api_request dispatches the eight notes.* / checkpoints.* methods to src/app/notes.rs directly after the coordinator arms Agents v2: emit_pane_state_update calls note_turn_edge (the turn origin's status edge, O(1)); handle_api_request calls log_user_request for the user's tab/team writes and dispatches the fourteen Method::Agents* arms.
src/app/actions.rs  mid-logic: AppState's event match treats AppEvent::NotesWorkerFinished as no state change (App handles it first) Agents v2: the scheduler deadline chain includes AppState.agents_close_deadline.
src/app/mod.rs  mid-logic: App::new (notes field from notes::runtime_for(&config.notes)), apply_live_config (a `notes` block, `self.notes.enabled`, directly after the coordinator one)
src/config/io.rs  mid-logic: load_live_config_from_str loads the `notes` section last
src/client/shell/config.rs  mid-logic: layout carves ClientShellLayout.info_dock from the right of pane_surface when info_dock_width is Some and pane_surface is at least DOCK_MIN + TERM_MIN (28 + 30) wide (width clamped to 70% of cols and to pane_surface.width - TERM_MIN); apply_live_config and the preference write carry info_pane_width / info_dock_width
src/client/shell/state.rs  mid-logic: ClientShellState::new loads info_dock_width from the chrome preferences; layout() passes info_dock_width_for_focused_tab(); apply_active_snapshot prunes the dock's open tabs (prune_info_dock_tabs); input_context sets info_dock_focused; timer_delay chains next_info_dock_deadline
src/client/shell/input.rs  mid-logic: the TextCommit and Paste arms hand text to info_dock_insert_text while the dock has focus (no overlay), before insert_overlay_text; route_key_press hands keys to handle_info_dock_key after the binding and prefix checks, before the pane
src/client/shell/mouse.rs  mid-logic: handle_mouse (the InfoDockWidth drag and release arms; the wheel over hits.info_dock.area scrolls the dock; info_dock_press runs before any pane hit so a dock press never starts a pane gesture; a pane press calls info_dock_pane_pressed, which saves a dirty editor and gives the keyboard back)
src/client/shell/composition.rs  mid-logic: compose draws the info dock into the chrome buffer when layout.info_dock is not empty (hits.info_dock from the dock's compose), and hides the pane cursor while the dock has focus
src/client/shell/actions.rs  mid-logic: record_binding's ToggleInfoPane arm (toggle_info_pane_for_focused_tab); handle_endpoint_result returns early for PendingEndpointKind::Info* (handle_info_dock_endpoint_result) before the notice block, so dock errors never toast
src/client/shell/context_menu.rs  mid-logic: items adds "Info pane" to the pane menu; activate toggles the pane's tab's dock without focusing it
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind error arm lists the five PendingEndpointKind::Info* kinds
src/client/mod.rs  mid-logic: the client loop's tick block calls shell.tick_info_dock(now, &mut outcome)
src/server/client_commands.rs  mid-logic: CLIENT_SHELL_METHODS lists the eight notes.* / checkpoints.* methods (sorted); every_notes_method_is_advertised_to_client_shells
src/app/api/agents.rs  mid-logic: queue_agent_prompt runs the typing guard (app::typing_guard::typing_block) when params.guard_user_typing, after the hosts_agent check and before any write (the Copilot focus bytes included), answering `user_typing`
src/server/pane_input.rs  mid-logic: apply_client_terminal_input_events stamps runtime.note_user_input on a key press/repeat (not release, not the host-scrolled page keys), text and paste; apply_terminal_attach_input stamps non-empty input and calls send_terminal_attach_input, which apply_scroll's PageKey branch calls unstamped
src/pane.rs  mid-logic: PaneRuntime.last_user_input (Cell<Option<Instant>>, Cell::new(None) in every literal) with note_user_input / last_user_input directly after current_size
src/terminal/runtime.rs  mid-logic: note_user_input / last_user_input directly after current_size
src/app/coordinator.rs  mid-logic: deliver_coordinator_wake sends guard_user_typing and turns a `user_typing` refusal into WakeOutcome::Held("you are typing in the coordinator")
src/app/api/agents.rs  mid-logic: handle_deferred_agent_api_request routes an agent.prompt whose request id starts with `coordinator:` or `plus:` (an older herdr_agents MCP server) through message_queue::legacy_agent_message first (typing guard forced on; queued and answered AgentPrompted when the target cannot take it now, also after a user_typing / agent_blocked / agent_not_ready refusal from queue_agent_prompt); every other id is untouched
src/app/api.rs  mid-logic: emit_event calls note_message_queue_event (marks App.message_queue due on PaneAgentStatusChanged, PaneAgentDetected, PaneClosed, PaneExited, PaneMoved, TabClosed, WorkspaceClosed; O(1) while the queue is empty) directly before run_plugin_event_hooks; handle_api_request dispatches Method::AgentMessageSend / AgentMessageClaim directly after AgentNotify
src/app/runtime.rs  mid-logic: next_headless_loop_deadline_with_git_refresh chains next_message_queue_deadline directly after next_coordinator_deadline
src/server/headless.rs  mid-logic: handle_scheduled_tasks_headless runs handle_message_queue_tasks directly after handle_coordinator_tasks
src/client/shell/context_menu.rs  mid-logic: items (tab menu: "Copy session ID" right after the agent items, before Close, only with the target's session_id; pane menu: last after Info pane), open_tab_context_menu / open_pane_context_menu set session_id: None, activate_context_menu_item copies before the target dispatch (no tab focus)
src/client/shell/mouse.rs  mid-logic: the right-click arm calls request_context_menu_session after open_tab_context_menu / open_pane_context_menu (the tab-row and pane-surface opens)
src/client/shell/actions.rs  mid-logic: handle_endpoint_result returns early for PendingEndpointKind::ContextMenuSession (complete_context_menu_session) before the notice block, so the lookup never toasts
src/client/shell/worktrees.rs  mid-logic: the exhaustive PendingEndpointKind list after the worktree arms names ContextMenuSession
src/server/client_commands.rs  mid-logic: CLIENT_SHELL_METHODS lists pane.get (sorted; 93 entries) and advertised_client_shell_method_shapes_stay_at_the_v1_contract freezes its digest
src/server/pane_input.rs  mid-logic: agents v2: client_input_source and attach_input_source (fork helpers after apply_scroll) classify client input for the turn origin; upstream changing ClientPaneInputEvent's variants must extend client_input_source
src/cli/pane.rs, src/cli/workspace.rs  mid-logic: agents v2: a command run from a live agent's pane is checked as that agent (agent_route) before the upstream request; a new upstream mutating subcommand needs a route call

## 9. Identifier watch-list (any hit in the incoming upstream diff = deny "upstream collision")
coordinator
CoordinatorState
CoordinatorConfig
CoordPhase
coordinator.get
coordinator.open
coordinator.open_dashboard
coordinator.wake
coordinator.start
coordinator.set_enabled
coordinator.set_wake_caps
coordinator.set_model
coordinator.set_notify
CoordinatorGet
coordinator_row
ClientCoordinatorState
is_coordinator_tab
ensure_coordinator_tab
CoordinatorPassFinished
coordinator_terminal_id
OpenCoordinator
open_coordinator
SidebarLayoutTabs
herdr_agents
agents_
HERDR_COORDINATOR_
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
report_subagents_with_mutation
subagent_session
breathe_phase
breathe_epoch
breathe_clock
next_breathe_deadline
tick_breathing
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
news.set_times
news.set_quiet_hours
NewsSetQuietHours
NewsSetQuietHoursParams
news_invalid_quiet_hours
news_quiet
last_read_edition
new_stories
note_news_read_when_focused
refresh_new_story_count
new_story_count_in
edition_story_keys_in
viewer_state_mtime
READ_CHECK_INTERVAL
USER_INTERRUPT_ERROR
read_check_after
viewer_state_seen
new_story_count
edition_story_keys
viewer_showing
write_read_record
read_record_path
viewer_state_path
first_seen_index
first_seen.json
read.json
viewer-state.json
what_changed
keyboard_tab_list
is_news_tab
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
news_notify
flush_news_notifications
NewsNotifyRecord
PendingNewsNotify
notify_decision
DAILY_NOTIFY_CAP
FAILURE_ALERT_AFTER
news_notification_target
news.json
herdr:news
news_assets
handle_news_tasks
next_news_deadline
quiet_hours
signal_process_group
NewsError
RUN_BUDGET_MIN
first_run_pending
BrowserHub
BrowserOp
BrowserConfig
BrowserActor
BrowserRunParams
BrowserRunResult
BrowserGetInfo
BrowserTabRecord
BrowserState
BrowserRun
BrowserGet
BrowserFocus
BrowserStart
BrowserStop
browser.run
browser.get
browser.focus
browser.start
browser.stop
browser.status
browser.log
browser.resolve_caller
browser.profiles
browser.profile_create
browser.profile_delete
browser_lane
browser_assets
show_activity
--load-extension
data-herdr-overlay
herdr-companion
activity.mjs
linger_ms
dismissPanes
paneKey
ACTIVE_MARK
herdrPlainTitle
BrowserBools
steer_wrap
browser_config_value
browser_table_edit
set_browser_item
update_file_atomic_at
refresh_checks_if_older
run_fix_batch
store_checks
helper_lock
HelperLock
settings_browser
ClientBrowserSettings
enter_browser_section
apply_browser_choice
route_browser_key
browser_click_applies
browser_section_rows
tick_browser_settings
next_browser_settings_deadline
handle_browser_settings_endpoint_result
pending_browser_wrap
continue_browser_steer_wrap
render_browser_section
COLOR_PRESETS
row_labels
next_color
next_mcp_agents
fixable_issues
install_command
hex_color
SetupEnv
SetupRecord
SetupState
BrowserCheckInfo
BrowserFixKind
BrowserFixResult
BrowserSettingsInfo
BrowserSettingsSetParams
BrowserFixParams
mcp_agents
shell_hook
setup_needed
run_fixes
refresh_checks
setup_info
auto_repair
auto_repair_once
MCP_AGENTS
DEFAULT_MCP_AGENTS
CHECKS_STALE_AFTER
install_helper
install_shell_hook
register_claude
register_codex
hook_lines
hook_line_path
rewrite_hook_lines
mcp_command_fallback
claude_registration
codex_registration
McpRegistration
SETUP_FILE
CHECK_IDS
BrowserBool
BrowserString
BrowserList
toml_edit
upsert_codex_block
write_in_place
backup_once
shell_single_quote
is_hook_line
codex_developer_instructions
is_executable
install_hint
mask_long_literals
EVAL_LITERAL_KEEP
COMPANION_EXTENSION_ID
note_push
push_failing
stopStaleWorker
pageSession
release_panes
install_browser_hub
herdr-browser
browser-activity.jsonl
browser.json
BrowserBatchStep
BATCH_MAX_STEPS
execute_batch
act_disabled
password_field_refused
batch_too_long
stop_with_server_if_configured
ClientBrowserState
ClientBrowserOverlay
BrowserOverlayRow
BrowserRow
browser_row
browser_marked_tabs
tick_browser
next_browser_deadline
TAB_BROWSER_MARKER
open_browser
OpenBrowser
BrowserFocusWindow
BrowserToggleProfile
BrowserOpenOverlay
browser_active_glyph_secs
render_browser_overlay
route_browser_overlay_key
AgentNotice
AgentNotify
AgentsSettings
AgentsFix
AgentsConfig
agent.notify
agent.notices
agent.notice_dismiss
agents.settings
agents.fix
endpoint.agent-notices.v1
agent_wrap
agent_notices
agent_cards
settings_agents
herdr+ shell v2
TeamInfo
TeamActor
TeamContext
TeamsPayload
team.list
team.get
team.make
team.disband
team.set_purpose
team.set_role
team.join
team.leave
team.context
endpoint.teams.v1
team_roster
agents_team
team_index
team_count
teams_view_rev
follow_teams
notes.get
notes.set
notes.append
checkpoints.list
checkpoints.add
checkpoints.update
checkpoints.remove
checkpoints.context
NotesInfo
NotesTarget
CheckpointKind
NotesRuntime
NotesWorkerFinished
toggle_info_pane
ToggleInfoPane
info_pane_width
info_dock
InfoDockWidth
agents_notes_read
agents_notes_append
agents_notes_write
agents_checkpoint
agents_checkpoints_list
guard_user_typing
user_typing
note_user_input
last_user_input
typing_guard
TypingBlock
message_queue
MessageQueue
QueuedMessage
AgentMessageSend
AgentMessageClaim
AgentMessageOutcome
agent.message_send
agent.message_claim
legacy_agent_message
CopySessionId
ContextMenuSession
context_menu_session
agents.actor
agents.directory
agents.read
agents.open_tab
agents.close_tab
agents.reopen_tab
agents.send_message
agents.rename_tab
agents.move_tab
agents.set_meta
agents.check
agents.actions
agents_model
TurnOrigin
InputSource
note_input
agent_meta
public_aliases
actions.jsonl
wake_scope

## 10. Fork smoke tests (run by name in the gate)
server::headless::tests::fork_smoke::suspended_status_reaches_the_client_shell_snapshot
server::headless::tests::fork_smoke::session_restore_keeps_a_suspended_pane_parked
server::headless::tests::fork_smoke::claude_hook_asset_reports_the_transcript_path_the_backup_store_uses
server::headless::tests::fork_smoke::claude_subagent_hooks_reach_the_client_shell_snapshot
client::shell::tests::tab_sidebar::fork_smoke::locked_suspended_pane_emits_no_pane_input
client::shell::tests::settings_backups::fork_smoke::every_settings_section_fits_the_96_column_popup
client::shell::tests::sticky_notifications::fork_smoke::three_cards_stack_newest_nearest_the_corner
client::shell::tests::tab_sidebar::fork_smoke::colored_tab_label_reaches_the_renderer_in_its_color
client::shell::tests::breathe::fork_smoke::a_working_tab_glyph_breathes_and_schedules_frames
client::shell::tests::idle_reminders::fork_smoke::marked_done_tab_reminds_after_the_interval
client::shell::tests::tab_sidebar::fork_smoke::tab_bar_status_reaches_the_tabs_sidebar_footer
client::shell::tests::tab_sidebar::fork_smoke::colored_multi_line_status_reaches_the_footer_in_color
server::headless::tests::fork_smoke::closed_agent_tab_reopens_from_the_list_with_the_same_session
server::headless::tests::fork_smoke::news_status_and_run_reach_the_news_tab
server::headless::tests::fork_smoke::news_notification_waits_for_a_client_shell_and_reaches_it_on_attach
server::headless::tests::fork_smoke::news_enabled_on_a_desk_that_never_ran_starts_a_first_run
server::headless::tests::fork_smoke::browser_get_reports_a_fake_host_tab_with_its_actor
server::headless::tests::fork_smoke::browser_launch_argv_carries_no_automation_switches
server::headless::tests::fork_smoke::browser_settings_write_the_config_and_fix_the_codex_entries_but_never_the_hook
client::shell::tests::browser::fork_smoke::browser_row_shows_the_running_browser_above_the_footer
server::headless::tests::fork_smoke::coordinator::coordinator_enabled_starts_a_coordinator_in_a_pinned_tab
server::headless::tests::fork_smoke::coordinator::coordinator_wake_reaches_the_coordinator_pane
server::headless::tests::fork_smoke::coordinator::coordinator_suggestion_notification_waits_for_a_client_shell
server::headless::tests::fork_smoke::coordinator::coordinator_idle_sends_no_finished_toast
server::headless::tests::fork_smoke::coordinator::coordinator_dashboard_serves_board_json
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notify_reaches_every_client_and_a_dismiss_fans_out
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notice_is_cleared_only_by_the_users_own_visit
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notice_is_seen_by_a_workspace_switch_into_its_tab
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notice_follows_a_moved_pane_and_goes_with_it
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notice_follows_a_renamed_tab_or_space
server::headless::tests::agent_notices_smoke::fork_smoke_agent_notice_goes_when_its_pane_process_exits
server::headless::tests::agent_notices_smoke::fork_smoke_agents_settings_write_the_config_and_the_hook_fix_waits_for_confirm
client::shell::tests::agent_cards::fork_smoke::a_click_focuses_the_agent_and_x_or_right_click_dismisses
server::headless::tests::teams_smoke::fork_smoke_team_make_and_roles_reach_every_client_but_status_does_not
server::headless::tests::teams_smoke::fork_smoke_restored_teams_reach_clients_on_the_first_pass
server::headless::tests::fork_smoke::notes::dock_methods_round_trip_over_the_client_shell
client::shell::tests::info_dock::fork_smoke::dock_carves_the_surface_and_resizes_per_tab
server::headless::tests::agents_model_smoke::fork_smoke_agents_set_meta_from_a_client_reaches_the_team_push
server::headless::tests::agents_model_smoke::fork_smoke_agents_close_of_a_teammate_tab_needs_the_users_turn_and_is_logged
server::headless::tests::agents_model_smoke::fork_smoke_client_typing_in_an_agent_holds_messages_back

## 11. Fork changelog (moved out of docs/next/CHANGELOG.md)
### Added
- Agent messages to a busy agent are queued and typed in when it is free, for every agent: `agents_send_message` no longer refuses with `busy` / `user_typing` / a live coordinator turn / `blocked` / a suspended target. The new `agent.message_send` method types the message in now when the target can take it (its agent live, idle or done, not starting, its user not typing in it or holding an unsent draft, for the coordinator no live turn), else the server queues it and answers `queued` (not an error; the tool result says why and not to resend). Queued messages wait per target in FIFO order and go in once the target has been idle for 3 s with the typing guard clear, several at once as one paste with each message's own envelope and id; they are never typed into a blocked dialog or a suspended agent (they wait), expire after 2 h (`expired`) and are dropped when the target pane or agent stays gone for 60 s (`dropped`, the grace covers a restart re-detecting agents). The log stays append-only: the server logs the queued message in full (outcome `queued`) and later `update` lines (`delivered`, `expired`, `dropped`) that every reader folds in (agents_messages, which also counts the pending ones, `herdr coordinator messages`, the dashboard, the watcher). The sender's next agents_* result starts with a note for each of its messages that expired or was dropped. A reply to a busy asker is queued too; an asker waiting in agents_wait_for_message gets it there and claims it (`agent.message_claim`) so it is not typed in again (a reply that arrives while the asker is free is typed in as before). Queued messages count against the rate limits and the loop guard from when they were sent. A queued message to the coordinator does not wake it (it is typed in once the coordinator is free, which starts its turn; the server writes the turn marker right before). The queue is saved to `message_queue.json` next to `session.json` (per session; it survives restarts and live handoffs) by one writer thread that also appends the log lines; delivery runs from agent events and the server loop's deadlines, never from render paths. Compatibility: agents started before this build keep their old herdr_agents MCP server, which calls `agent.prompt` itself; the server takes an `agent.prompt` whose request id starts with `coordinator:` or `plus:` for an agent message, always applies the typing guard, and when the target cannot take it now queues it and answers success, so the old sender logs it as `sent` and does not resend (the server adds `queued` / `delivered` update lines under the id from its envelope); a user's own `herdr agent prompt` is unchanged. A new MCP server talking to an older server falls back to its own `agent.prompt` path. Tool descriptions, the instructions and coordinator.md drop the "retry later / wait_s" advice (wait_s only matters for an older server); the dashboard shows `queued` dim and `expired` / `dropped` as failures.
- Copy session ID: the tab menu (after the agent items) and the pane menu (last) of a Claude or Codex agent offer "Copy session ID", which copies the agent's native session id (the Claude session uuid, the Codex thread id) through the usual clipboard path with the clipboard toast. Opening the menu asks the server with one `pane.get` (its `agent_session`, id kind only; the fork now advertises `pane.get` to client shells, so an older server simply never shows the item); the item appears once the id arrives and never otherwise. A tab with several agents copies the focused pane's agent when that pane is in the tab, else the agent the Suspend / Restart items act on.
- Agents model v2: every tab is an agent target. One server check (`agents.*` methods, src/agents_model/policy.rs) decides what an agent may do: it may edit its own tab; inside a team it may rename, move, set the role and note of, and open tabs for its teammates; outside its team it may read agent screens and message, nothing more (`outside_team`). Shell screens are readable only by teammates and the coordinator. Closing a tab (`agents_close_tab`, graceful exit, then close, reopenable with `agents_reopen_tab`) needs a turn that started from the user's own input in that agent (`non_user_turn` otherwise); the coordinator's tab is protected from every agent. The server tracks a per-pane turn origin from client input (typing, Enter) versus programmatic writes (`agent prompt`, `pane send-*`, messages, wakes). Every agent write and refusal, and the user's tab and team actions, go to `actions.jsonl` (`agents.actions`, the dashboard's actions panel). Messaging and limits moved into the server (per terminal, loose fixed limits: 200 messages an hour, a 30-in-10-minutes pair loop guard, 20 spawns an hour, 40 agent-opened panes per team, 300 soft edits an hour) and a message waits while the target's user is typing (120 s guard). Agent-run `herdr tab|pane|agent|workspace|team …` commands from a live agent's pane are checked as that agent. The tab menu gains "Set role…" on any agent tab and "Info pane".
- Teams: a group (a space after the first) can be a team — Make team… on the group menu (or `herdr team make <group> [--purpose T]`, `team.make`, or the coordinator's `agents_team action=make`). The `tabs` sidebar shows the group header as `◆ <purpose>` (the group label, dim, until a purpose is set) and a dim `◆` on member tab rows in place of the managed `+`. Every claude / codex started in a team group (by the user or the coordinator) joins and is managed by the team; moving a tab in or out joins or leaves; a member is named after its role (`fixer`, `reviewer`, `reviewer-2` on a clash; the tab follows unless the user named it). Team info (group menu) lists members, roles, status and time in state with ✎ role and purpose editing (the Rename modal, which returns to Team info), × remove from team and + join; the tab menu has Set team role… / Leave team / Join team; Disband team and "Ungroup (disbands team)" end a team. State is server-side on the workspace, persisted in session.json, pushed to every client as `endpoint.teams.v1` and served by the new `team.*` methods and `herdr team list|get|make|disband|purpose|role|join|leave`. Members know the roster, roles and purpose from their first turn: with `[agents] team_roster = true` (default, Settings → Agents "team roster in team groups", works without `wrap`) a wrapped claude / codex launched in a team group gets the roster in its system prompt / developer_instructions plus the team tools (herdr_agents whoami, notify, list, get, read, messages, wait, send_message, team), and Claude a per-launch `--settings=<coordinator dir>/team/claude-settings.json` whose `UserPromptSubmit` / `SessionStart` hook (`herdr team hook`, always exit 0) adds roster changes to the next prompt; Codex gets them as the first line of its next agents_* tool result; agents_whoami always shows the team. Teammates may message and wake idle teammates without asking their user (`[herdr+ message … (teammate)]`; rate limit and loop guard unchanged, logged with the team). The dashboard marks team groups with `◆ purpose` and member cards with a `team` tag.
- Agent cards (`agents_notify`): an agent can put a sticky card in front of the user — the herdr_agents MCP tool `agents_notify { title, body?, kind: info|question|done|warning }` (verified callers only) or `herdr agent notify <title> [--body TEXT] [--kind KIND] [--json]` inside a pane, over the new `agent.notify` method. The card (Dusk palette, its own look, not a toast) names the agent and its tab, shows a kind glyph and accent (`●` info, `?` question, `✓` done, `!` warning), stacks at the top right under the tab bar (below a top-right toast stack, at most four, then `+N more from agents`; one banner line on the mobile layout), and stays until the user dismisses it (`×` or right click; the fold line dismisses all) or moves into the agent's tab (a click on the card does that); an agent focusing tabs through the API never clears it. Every attached client shows the same cards (`endpoint.agent-notices.v1`); a new question rings the request sound, done the done sound, with terminal/system delivery per `ui.toast.delivery`, and nothing rings on attach. The sender comes from the calling pane's record; one card per agent; rate limits 1/20 s, 5/10 min, 20/h; text sanitized (one-line title ≤ 80, body ≤ 3 lines / 280 chars). `[agents] notices = false` turns them off. Cards are not persisted and do not survive a live handoff.
- Settings → Agents and `herdr agent wrap`: a last `agents` section in Settings (the popup is 104 columns) with `wrap claude / codex in herdr+ panes` (`[agents] wrap`, its source shown when it falls back to the legacy `[browser] wrap_agents`), `add agent tools (notify)` (`tools`: the herdr_agents MCP server via `--mcp-config=` / `-c mcp_servers.herdr_agents.*` with whoami and notify pre-approved), `add herdr+ instructions to the system prompt` (`instructions`: `--append-system-prompt` / Codex `developer_instructions`, merged with the user's own), `instructions file` (built-in ↔ `<config dir>/agents.md`, seeded once), and a status row (shell hook · claude · codex) whose `▸ fix` only arms a confirm that shows the exact `.zshrc` line before `agents.fix { confirm: true }` edits it. New methods `agents.settings`, `agents.settings.set`, `agents.fix`. `herdr agent wrap <claude|codex> [--print] [--] ARGS…` is the one wrap implementation (browser steering and no built-in browser are its contributions while wrapped); managed coordinator launches get no extra paragraph or tools; per launch `--no-herdr`, `HERDR_NO_WRAP=1` or `command claude` opt out. Shell hook v2 (`# herdr+ shell v2`) inlines its guard in each function so it also works inside agents' shell tools.
- Info pane (v1): a per-tab dock on the right of the pane surface (`keys.toggle_info_pane`, unset by default, or the pane menu's "Info pane"), drawn by the client in the Dusk style and lazy — nothing is computed or requested until it is opened, and only for the focused tab. It starts with a `Notes` / `History` tab strip. Notes are the markdown notes of the tab's agent session (`<config dir>/notes/<agent>-<session id>.md`; a tab without an agent uses `tab-<id>.md`, copied to the session once an agent appears): rendered markdown-lite, click anywhere to edit in place (Esc, a view switch, a pane click, closing the dock or 2 s idle saves with a compare-and-swap; a conflict offers reload or keep mine, a line merge), and `- [ ]` task markers tick with a click. History is the session's checkpoints — decisions, milestones, failures and notes the agent marks, plus bookmarks (`b`) — as a timeline with ▸/▾ rows that expand to the prompt and reply around the checkpoint read from the agent's transcript (Claude and Codex), kind filter chips, expand / collapse all, show more / less, `[` / `]` to jump within a kind. Mouse click and wheel work everywhere; the divider drags (double-click resets) and `<` / `>` resize; the width is remembered by the client. The server owns the data: new methods notes.get / set / append and checkpoints.list / add / update / remove / context (advertised to client shells, `[notes] enabled`), the CLI `herdr notes read|path|append|write` and `herdr checkpoint add|list|show|rm|edit`, and five agent tools in the coordinator's MCP server (agents_notes_read, agents_notes_append, agents_notes_write, agents_checkpoint, agents_checkpoints_list), which work with the coordinator disabled. The dock polls every 5 s while open, one request at a time; its errors show in the dock, never as toasts.
- Coordinator, native (build 1): the coordinator runs inside the herdr server, which also serves its dashboard; `herdr plus` became `herdr coordinator`. `[coordinator] enabled = true` makes the server start a Claude coordinator in a `coordinator` tab (pinned as the bottom row of the `tabs` sidebar, below News: `○ N agents`, `◐ waking/working`, `● N ideas`, `◌ capped`, `! locked`, `× down`, `○ off`; managed agents' tabs get a dim `+`; under `spaces` the coordinator tab carries the same mark in the tab bar), relaunch it when it goes missing (`relaunch_cap_hour`, then `down`), wake it on agent events and messages (`cap_hour`, `cap_day`, `periodic_minutes`), migrate the POC's `plus/` directory to `coordinator/` once, and serve the HTML dashboard on `http://127.0.0.1:<dashboard_port>/` (7718 by default, 0 off). The row's menu and the new settings section open the dashboard (herdr's browser, else the system browser; remote shells get the URL), wake, restart, pause/resume, and set the model, wake caps and notifications; `keys.open_coordinator` (unbound) focuses it. Its own finished/attention toasts are suppressed; it raises its own capped notifications instead (new suggestions, down, blocked, locked; `notify`, `notify_daily_cap`, `quiet_hours`). The MCP server is `herdr_agents` with `agents_*` tools; coordinator-opened agents get short task names, the best-fitting existing group (a new one only when none fits), and priority work starts ungrouped in the top space (`agents_open_tab priority`). New API: `coordinator.get|open|open_dashboard|wake|start|set_enabled|set_wake_caps|set_model|set_notify`; CLI `herdr coordinator enable|disable|start [--new]|wake|status [--json]|dashboard [--print]|messages|manage|unmanage|clear-turn|seed|mcp`. Unix only for now.
- herdr browser, activity overlay lifetime: the glow frame stays on a tab for the `[browser] active_glyph_secs` window (120 s by default, the same window as the sidebar's `◎`) instead of 3 s — pulsing while an operation runs, a calmer steady glow for the rest of the window, a slow fade at its end, gone at once when the pane is released or the tab closes; a navigation inside the window puts it back in the same state without stretching the window; screenshots still hide it. The cursor stays where the last operation left it for the whole window (read-only operations keep it; a navigation re-places it) and fades with the frame. While a pane is active its tab group's title carries a `●` mark (`● ✻ planner`, plain Unicode, applied by the companion — never the page's document.title), removed when the window ends together with the collapse; ownership, comparisons and restores go by the plain title, so a browser restart mid-window still adopts the pane's group. The directive carries `linger_ms`; companion v11 (a running browser shows the mark after `herdr browser stop` and open again; a v10 worker keeps plain titles meanwhile). Checked by scripts/test_browser_overlay.mjs (run from the Rust test activity_overlay_lives_for_the_window_and_marks_the_group when node is on PATH).
- AI news desk, what's new since you read: the page viewer (viewer.py) draws a timeline spine beside every story — a first-seen chip (`today`, `yday`, `Mon`, `Sep 12`; the edition a story first ran in, from `<home>/first_seen.json`, which the runner keeps: built once from every edition in `editions/index.json` when the file is missing, then only added to, a story never moving later) and, against the baseline edition, a `●` node with a rust rail for a story that was not there (NEW) or a `◑` node, the editor's one-line `what_changed` note (`↻ revised:`) and a word diff of the text (`Δ`) for one whose text changed or that the editor flagged `"changed": true` (UPDATED; system.md asks for both fields, the validator accepts them: `changed` a bool, `what_changed` a string without control characters of at most 30 words). The title line reads `● N new ◑ M updated since you read the HH:MM edition` with a first-seen ribbon (how many stories per age bucket) and a legend, every section rule carries its own `N new · M updated`, the footer tallies them, and `n` / `N` jump to the next / previous new story, `u` toggles a new-only filter. The baseline: the edition herdr last saw the reader on (`<home>/read.json`, `last_read_edition`, recorded by the server's scheduler pass while the News tab is focused with the viewer up — the viewer writes `<home>/viewer-state.json` `showing` on every edition it opens — persisted in news.json and in news.get as `last_read_edition`); an edition older than that is compared against the one before it, and without a record the previous edition is the baseline. news.get also carries `new_stories` (the latest edition's stories absent from the last one read), the pinned News row's unread state reads `N new` and a run notification's body starts with `N new · `. The spread layout (≥ 160 columns) deals whole sections to two columns — the lead opens the left one, each section goes to the shorter column (ties left) and never splits across the fold (viewer.py `deal_sections`). `herdr news quiet [HH:MM-HH:MM|--off]` shows or sets the quiet hours through the new `news.set_quiet_hours` method (the settings news tab's quiet-hours picker uses it too, so a remote server's window changes; the canonical `HH:MM-HH:MM` or "" is written to `news.quiet_hours`, news_invalid_quiet_hours otherwise). Python checks for both assets live in scripts/test_news_viewer.py (marker states, baselines, chips, word diff, filter, dealing, a pty run of the pinned viewer that drains output and checks the keys, quit and the restored screen) and scripts/test_news_run.py (validator fields, first-seen index), run by the Rust test python_asset_checks_pass (which also asserts no `__pycache__` beside the assets).
- herdr+ settings → browser: a `browser` section right after `news` in the settings overlay (src/client/shell/settings_browser.rs; the popup is 84 columns wide so ten tabs fit, 32 rows tall for this section). Rows: `browser: on|off`, `show activity (groups, glow, cursor)`, `pinned dashboard`, `activity colour #rrggbb ■` (↵/→ cycles the presets #aa6eff #00c8ff #5fd3a0 #ffb86b #ff7aa8), `agents use herdr's browser (steer + wrap)` (both keys), `hide agents' own browsers`, `MCP for: claude ✓ codex ✗` (↵/→ cycles both / claude / codex / none → `mcp_agents`, the registration follows on the server), `shell hook in ~/.zshrc`, `▸ fix all (N issues)` (`browser.fix` for every failing fixable check — the extension fix restarts the browser), `open browser` / `stop browser` (the default profile), `install herdr+ Browser from a Chromium build… (↵ copies the command)` (the overlay has no text input: the exact `herdr browser install-chromium <Chromium.app>` goes to the clipboard). Every row acts through the active server (`browser.settings.set` writes the server's config); a remote endpoint's file-editing rows say `(on <machine>)`. Under a rule the facts — status, browser, helper, extension, MCP, shell hook, the last fix — with ✓/✗ first and the reason after. The section polls `browser.settings` every ~0.9 s while the server is checking or fixing and says `browser settings unavailable on this server (an older herdr)` when the method is not advertised. The Browser sidebar row and `herdr browser status` show a `setup needed` hint while a file-editing check fails.
- herdr browser, pinned dashboard: `[browser] pin_dashboard = true` keeps the herdr+ page (`companion/dashboard.html`, the new tab page under its own URL) as exactly one pinned first tab per normal window — the companion (v5) ensures it at worker start, on `windows.onCreated` and at every attach (`attach` carries `pin_dashboard`; a config change pushes the sidecar op `dashboard { pin }` live), dedupes restored copies; only pages under the companion's own id (`browser_assets::COMPANION_EXTENSION_ID`, derived from the manifest `key` and asserted against it by a test; `state::is_dashboard_url` in Rust, `chrome.runtime.getURL` in the worker) are herdr furniture — another extension's dashboard.html or newtab.html is an ordinary tab and is never closed; a window whose user unpinned or closed it is left alone for the session (`storage.session`), the setting itself is remembered in `storage.local` for the next launch; `false` removes herdr's dashboard tabs. Dashboard and new-tab pages never enter the ledger (`state::is_dashboard_url`: `reconcile` skips them, an `opened` event is ignored, a tab navigated to one leaves), so they are not listed, counted, grouped, overlaid, made current or closed by `close_opened`.
- herdr browser, shell hook: `herdr browser setup --shell` writes the managed file `<config dir>/herdr/shell/herdr-plus.zsh` (header "managed by herdr browser setup"; `function codex`, `function claude-z` and — only when no `claude` alias exists at source time — `function claude`; they wrap only with `HERDR_PANE_ID` and `HERDR_BIN_PATH` set) and adds one guarded line to `~/.zshrc` (`[ -n "$HERDR_PANE_ID" ] && [ -f '…' ] && source '…'  # herdr+` with the path single-quoted (`'\''` escaping), only when no live — uncommented — copy exists; `.zshrc`, a symlink resolved to its target, is backed up once to `.zshrc.herdr-backup` before the first edit or removal and rewritten through a temp file in its directory with its mode kept; `--shell --remove` takes the live line out and leaves a commented-out copy alone; since the settings section, plain `setup` applies `[browser] shell_hook` — default true — so it adds the line unless the key is false); `[browser] wrap_agents = true` decides per launch whether `herdr browser wrap` adds anything — `false` runs the agents unchanged except Codex's `--no-daemon`; `doctor` shows the three toggles and whether `.zshrc` has the line.
- herdr browser, agent steering: `[browser] steer_agents = true` puts a preference paragraph in front of the MCP server's `instructions` (herdr-browser tools for any browser work inside herdr+, not Claude in Chrome / Codex's in-app browser or browser_use; web search stays fine); `herdr browser wrap <codex|claude> -- ARGS` execs the real agent with that text as a developer/system instruction (`-c developer_instructions=…` for Codex — verified to reach the model; `--append-system-prompt` for Claude Code), always `--no-daemon` for Codex, and with `[browser] disable_native_browser = true` `--disable in_app_browser --disable browser_use` / `--no-chrome`; `wrap claude` execs `claude-z` when it is on PATH (a file with the execute bit, `is_executable`) so session UUIDs keep working — the user's arguments first (claude-z reads the session id from `$1`), herdr's flags after them and before a user `--`, and a flag the user already passed is not added again; `wrap codex` puts the user's own top-level `developer_instructions` from Codex's config.toml ahead of the steering text in the `-c` value (profiles are not covered); `setup` prints the `codex()` and `claude-z()` shell functions (a `claude` alias expands to `claude-z` before any `claude` function would run, so the wrapper function is `claude-z` and calls `command claude-z`); `doctor` shows both keys.
- herdr+ new tab page: the companion extension overrides the new tab page (`chrome_url_overrides.newtab` → `newtab.html`/`newtab.js`, the approved Dusk design: masthead, standing line, an Agents section with one block per agent pane — symbol, herdr tab label, pane location, last op + age, pulsing while active — listing that pane's tabs, then Yours, footer); herdr pushes a compact snapshot (src/browser/ntp.rs: agents from the ledger's pane actors and cursors, their open tabs with short id/title/URL/host/current, the user's tabs, profile, last action) through the sidecar op `ntp` into the worker's session storage after every ledger change (debounced to one per second) and on attach; the page renders with textContent only, ages tick client-side, a click activates the tab and raises its window; empty state "No agents in the browser right now."; with `show_activity = false` the extension is not loaded (Chromium's own new tab page) and a snapshot pushed with it off shows the masthead only. Chromium's one-time "extension changed your new tab page" dialog is left as is: the acknowledgement lives in `Secure Preferences` (`extensions.settings.<id>.ack_ntp_bubble`), a MAC-tracked pref enforced on macOS (`GetSettingsEnforcementGroup` → GROUP_ENFORCE_DEFAULT), so pre-seeding it is unreliable.
- herdr browser: `herdr browser install-chromium <Chromium.app> [--icon PNG|ICNS] [--name "herdr+ Browser"] [--dest ~/Applications]` (macOS) makes a branded copy of a built Chromium — name in Info.plist and every `*.lproj/InfoPlist.strings`, icon from the embedded herdr+ PNG (src/integration/assets/browser/branding/, 1024 px) or `--icon` PNG/.icns (sips iconset → iconutil), bundle id and compiled strings untouched so the "Chromium Safe Storage" keychain item keeps working, quarantine stripped, ad-hoc signed and verified, LaunchServices refreshed, swapped in atomically over an older install, refused while that app runs; `[browser] executable = "auto"` finds `~/Applications/herdr+ Browser.app` (the verb says so only for that default name and place — `install_hint`; elsewhere it prints the `[browser] executable = "<path>"` line to set; on a non-macOS host it refuses as macOS only) first (`doctor` shows the source; the product's display name is herdr+, every identifier, path and env var stays `herdr`); new profiles are seeded with the name `herdr+ · <profile>`, to restore the last session on startup with the new tab page as the home page (no hard-coded URL).
- herdr browser: the password rule covers `eval` — while `type_into_password_fields = false` the sidecar snapshots every password field (open shadow roots and same-origin frames included) in the isolated world before the code runs and, when one's value changed, restores it and answers `password_field_refused` instead of the result (fail closed when the guard cannot run); the ledger records the eval code (one line, sanitized, 200 chars) and never its result.
- herdr browser: `herdr browser setup --codex` registers the MCP server in Codex's `config.toml` (edited as a `toml_edit::DocumentMut` by `upsert_codex_block`: an existing entry in any shape — table, dotted keys, inline under `[mcp_servers]` — is replaced, every other table, comment and spacing survives, a file that does not parse is refused; `register_codex` writes through a symlink to the real file via temp + rename in its directory keeping the mode, after a one-time `config.toml.herdr-backup`; `[mcp_servers.herdr-browser]`, the same `sh -c 'exec "${HERDR_BIN_PATH:-…}" browser mcp'` line, plus `env_vars` forwarding the pane variables — Codex starts MCP servers with a minimal environment, so without the list the server cannot reach the pane's server or attribute the caller); an existing entry is replaced in place, the rest of the file is untouched; `--claude` keeps the Claude Code registration alone, neither flag does both; `doctor` reports the Codex registration. Tab group titles are `<symbol> <herdr tab label>` (`✻` claude, `◇` codex, `◌` default, `[browser] group_symbols` overrides) instead of provider names.
- herdr browser: `[browser] activity_color = "#aa6eff"` (`#rrggbb`; invalid → default with a config diagnostic) colours the activity overlay — frame border and glow (alpha unchanged), cursor fill and ripple — and applies live after `herdr server reload-config` (the directive carries it; an overlay of another colour is re-injected); tab-group colours stay Chrome's per-pane palette.
- herdr browser, activity overlay (`[browser] show_activity = true`): the tabs an agent pane opens or acts on sit in a Chrome tab group named `<agent> · <herdr tab>` in a stable colour per pane (a bundled MV3 companion extension loaded with `--load-extension`, driven over the DevTools port; expanded while the pane is active, collapsed after `active_glyph_secs`, dissolved when the pane is gone; tabs the user opened are never grouped and a tab the user pulls out stays out), and a purple glow frame with a macOS-style cursor is injected into the page (isolated world, closed shadow root, no pointer events) while an operation runs and for the `active_glyph_secs` window after (originally 3 s); the cursor glides to the element before click/type/fill/select/press/hover and ripples on click; batches take `animate: false` (`--no-animate`); screenshots hide the overlay; `browser status` / `doctor` report the companion (`ready`, `missing`, `off`).
### Fixed
- Automatic typing never lands in a pane while its user is typing there or has an unsent draft: an agent message (`agents_send_message`) or a coordinator wake-up used to paste into an idle agent's input box on top of the user's half-written text and press Enter, sending the user's text cut off. `agent.prompt` gains an optional `guard_user_typing` that refuses with `user_typing` (nothing typed) while a client sent key, text or paste input to the pane in the last 10 s (a per-pane stamp written on the server's client input path) or the agent's input box holds non-placeholder text (Claude Code: the text between the last two `─` rules; Codex: the last `›` line and its continuation; the faint placeholder counts as empty; other agents rely on the 10 s window). A message to such a target answers `user_typing` (a reply is `logged`, as for a busy asker) and `wait_s` waits for the guard to clear; a wake-up is held with "you are typing in the coordinator" and retried after the held backoff. `herdr agent prompt` is unchanged.
- AI news desk and browser overlay, audit fixes (feat/news-new-markers): the read mark is not recorded while a run or a pending viewer command owns the News pane (viewer-state.json outlives the viewer), the check runs at most once a second and reads the file only when it changed; `new_stories` is computed when the mark moves, a run finishes and at load, over one read of the editions index (the never-written cache is gone); the viewer builds the edition history only when first_seen.json is missing and keeps a bounded page cache (`PAGES_MAX`); a new story keeps its real first-seen time when the runner's log has one; `linger_ms = 0` means no linger (the default only when the value is missing or not a number); a released pane's overlays go whatever the companion's state (`paneKey` on each page, `dismissPanes`); the runner's "interrupted by the user" is a named constant on the server (`USER_INTERRUPT_ERROR`) with reciprocal comments; doc comments restored/extended (`store_path`, `waiting_important_tabs`, `active_glyph_secs`); the branch's edit to upstream's distribution workflow is reverted (cargo's python_asset_checks_pass runs the python modules).
- AI news desk, rough edges: keyboard tab switching (`SwitchTab(n)`, next / previous tab) indexes the list the sidebar shows — the pinned News tab is left out in the tabs layout, so the number keys match the rows (and the gate that decides whether the key reaches the pane agrees with the action; next / previous from the News tab land on the first / last listed tab); the News tab never gets an idle reminder (its important mark means an unread edition, not a waiting agent); a run the reader cancelled (two Ctrl-C in the pane: the runner's own `interrupted` record with the error `interrupted by the user`, outside the watchdog's Stopping phase) neither counts toward nor resets the three-failure alert, while timeouts, crashes and watchdog stops still count; `news.set_quiet_hours` replaces the settings tab's local config write for quiet hours.
- herdr browser, settings audit (tri + cross): a `browser.settings.set` whose reload could not apply the value (an invalid `[browser]` section, an unparsable file) answers `browser_config_write_failed` "written but not applied" and runs no fix (a fix would have acted on the old setting); the browser config edits go through toml_edit and an atomic write (comments, `[browser]  # note`, `[ browser ]`, multi-line arrays, trailing comments and other tables survive; symlink followed, mode kept); the `agents use herdr's browser` row writes steer_agents and wrap_agents in one request/one file write (`steer_wrap`); a fix requested while one runs is queued and drained by the same worker with a fresh config (an "all" request absorbs named ids), a check refresh overtaken by a fix drops its result (generation); the extension fix is a synchronous stop → start, `ok` only when the profile is up; `claude mcp remove` only runs when the entry exists and the store is re-read after it; the helper install takes a process-wide lock plus `flock` on `<browser home>/setup.lock`; an unchanged `browser.get` reply still carries `setup_needed`; `browser.get` refreshes the checks only when older than 5 min; the auto-repair also runs from an agent's first `run()` (ensure_running, Once-gated) — the old path only refreshed assets in place and never ran npm ci; the boot-time checks are skipped with `[browser] enabled = false`.
- herdr browser, settings → browser (server side): `src/browser/setup.rs` is the one place `herdr browser setup`, `doctor` and the new `browser.settings` / `browser.settings.set` / `browser.fix` methods run their checks and fixes (executable, helper, extension, MCP per agent, shell hook, launch context; `fix_kind` safe or edits_files). The shell-hook check matches the full path of THIS instance's managed file — the owner's real `setup --shell` had skipped its line because the dev instance's `~/.herdr-dev/…/herdr-plus.zsh` line was there; such a line is reported as "a herdr+ line for a different instance" and replaced by the fix (backup first); an MCP entry whose fallback names another herdr binary is reported the same way. New keys `[browser] mcp_agents` (default both) and `shell_hook` (default true): `setup` applies them — it now adds the `.zshrc` line and writes Codex's config.toml by default (previously only with `--shell` / when codex was found); `doctor --json` emits the checks. After a herdr update the safe fixes (assets, npm ci when the lock changed) run by themselves on the first browser start (`setup.json` in the browser home); user files are only ever edited on an explicit request. `browser.get` carries `setup_needed` for the Browser row's hint.
- herdr browser, round-3 audit: the companion worker refresh no longer deletes the profile's `Default/Service Worker` store (it held every site's workers) — a stale worker is reported as `extension update pending` and retired (unregister + stop over a fresh page-target CDP session) right before herdr closes the browser, so the next start loads the new files; only the companion's own extension id (`COMPANION_EXTENSION_ID`, asserted against the manifest key) counts as herdr furniture and the "older ids" cleanup is gone; Codex `config.toml` is edited as a `toml_edit` document (any entry shape replaced, comments kept, unparsable file refused, symlink resolved, mode kept, one-time `config.toml.herdr-backup`); the `.zshrc` hook ignores commented-out copies, single-quotes the path, backs up before `--remove` too and writes via temp + rename through a symlink with the mode kept; the managed shell file uses `function name { … }` throughout; `wrap claude` puts the user's arguments first (claude-z's `$1`), herdr's flags before a user `--`, never twice; `wrap codex` keeps the user's top-level `developer_instructions` ahead of the steering text; `wrap` only execs files with the execute bit; an MCP server with no process ancestors answers "cannot tell" instead of wrong_pane; `HostEvent::Log`/`Unknown` neither flush the ledger nor schedule a new-tab-page push; failed `ntp`/`dashboard` pushes warn once per state change; a guarded eval is never re-run after a navigation; eval ledger lines mask string literals longer than 20 chars as `"‹N chars›"`; the new tab page gets `active_secs` and per-row Chrome tab ids (exact-tab clicks, live/idle computed on the page, 1 s tick touches only ages); `install-chromium` says "auto finds it first" only for the default name in `~/Applications` (else prints the `executable = "<path>"` line) and refuses on non-macOS.
- herdr browser: tab groups stopped appearing after a while — a companion call that found no worker target (Chrome idles the extension worker out between the keep-alive alarms; `/json/list` briefly has no `sw.js`) latched the driver's state to `missing`, which silently turned off every later group touch and new-tab-page push until the next attach; a transient miss now fails that one call only (`scripts/test_browser_companion.mjs`, run from `browser_assets` tests when node is present). Restart continuity: the worker remembers the titles it gave to groups (`storage.local` `ownedTitles`; group ids do not survive a restart) and, when a pane has no recorded group, adopts the restored group in the window that carries the pane's own title — or the one a restored tab already sits in — before making a new one; a group whose title herdr never gave is still never touched (companion v9).
- herdr browser, live-trial fixes: the MCP server checks (via `browser.resolve_caller`, which now carries the pane's `shell_pid`) that the pane its environment names is among its own process ancestors and otherwise answers every tool with `wrong_pane` — a Codex app-server daemon spawns MCP servers with the daemon's environment, so sessions would be attributed to whichever pane started the daemon; `setup --codex` prints a `codex()` shell function that adds `--no-daemon` inside herdr panes (herdr edits no rc file; Codex's `daemon_auto_start` is untouched) and `doctor` notes a running daemon; `read` renders only data tables (a header row, or consistent columns with no nested table/form and not the page's wrapper) as pipe tables and flattens layout tables to lines with cells joined by ` · `, links kept (Hacker News no longer comes out as pipe noise); `install-chromium` drops `CFBundleIconName` so the replaced `app.icns` is used instead of the asset catalog's icon.
- herdr browser, round-2 audit fixes: act descriptions never carry a field's contents (no `value`, no `innerText` of an editable) and describe the control a label targets; the no-target `press` reads the deep active element through open shadow roots as a real handle, treats a frame as a password field and fails closed when it cannot look; tab groups are keyed by pane id in the worker's session storage (no adoption or dissolution by title, a same-named group of the user's is never touched) and gone panes are detected from the ledger on every `browser.get`, with failed releases logged and retried with a backoff; a failed `open` still adopts its tab for the caller (and `close_opened` closes it); the overlay stays hidden across both screenshot captures; `attach_timeout` is a sidecar code; DevTools `/json/list` fetches time out; one companion connect at a time, the socket closed on a timeout; `stop_with_server` never launches or attaches; sidecar timing lines go through the log channel; the client does not re-queue `browser.get` while one is in flight, the overlay stops "loading" when the pull fails or cannot happen, the ◎ marker follows a moved pane, and with room for one pinned row News keeps it; CLI text after `--` is never a tab id.
- herdr browser: `browser stop` right after a server restart attaches to the recorded browser first (Browser.close instead of a signal; a live pid from the state or the run record, never a launch), and a browser that survives the stop stays a running profile with its run record; a gone pane's group release carries the ledger's group title, so a sidecar that restarted since still dissolves the group.
- herdr browser, agent-trial fixes: `snapshot` is a batch step (`{"op":"snapshot","interactive":true}`) whose refs serve the following steps of the same batch and whose output is kept under its step line, so an `open` inside a batch can be followed by ref-based acts; `close_opened` closes the tabs a batch opened after the final step and returns the pane to its previous tab; the MCP instructions and the open/navigate/close tool texts tell agents to reuse their current tab with `browser_navigate`, to `browser_open` only for a separate tab and to close what they opened when the whole task is done unless the user may want to look at it (softened 2026-10-01: immediate closes hid the tab groups); the Browser overlay sizes the actor/op/age column to its widest visible row (capped at half the list) instead of a fixed gap before the title.
- herdr browser, audit fixes: `use`, `close` and `focus` take their tab from `BrowserRunParams.tab` (the op's own `tab` field made `{"op":"use","tab":…}` unreadable); an agent `open` whose `opened` event arrived first is still attributed to the agent; a qualified tab (`work:t3`) selects its profile before attaching and a disagreeing `--profile` is `invalid_request`; `browser.profile_delete` refuses starting / in-use profiles, a live run-record or SingletonLock pid, and answers `profile_busy` instead of waiting on a lifecycle lock; the ledger flush is one serialized transaction (no duplicate activity lines, `dirty` restored on a failed write) and console-error counts wait for the supervisor pass; an in-place asset refresh checks the installed playwright-core against the pin before stamping `runtime.json`; caller `timeout_ms` and `screenshot_keep` (>= 2) are clamped; element screenshots get the downscaled inline copy too; `read` paging and `find` reuse the last extraction of the same format, scope and URL; `--out` must be absolute and never overwrites; page titles, URLs and body text lose control characters before the terminal sees them; only `about:blank` passes among `about:` URLs; SingletonLock pids outside `1..=i32::MAX` are ignored; one forbidden-switch list (now with `--use-mock-keychain`) matches `--x` and `-x`; `/json/version` reads are bounded; unknown `wait` states are refused.
- AI news desk, audit fixes: the runner takes SIGTERM and SIGHUP as its interrupt (the editor's process group killed, `interrupted` recorded) and has one budget for the whole run (`--deadline-min`, 60 by default, passed by the server, whose watchdog is that plus 10 min); the server's watchdog sends SIGTERM to the News pane's foreground process group (two Ctrl-C a second apart when it finds none) and keeps the run in flight until the runner's record or the shell prompt, then records `timeout`; a run whose News tab was closed is recorded `interrupted` instead of waiting for the watchdog; the notification retry deadline no longer spins the loop without a client; malformed editor output is a validation error (or a `crashed` record), control characters are rejected by the runner and stripped by the viewer, URLs must be plain https (sources too; `anchors.py` refuses other schemes), notification text is sanitized when queued, and the daily cap is charged at delivery; the settings toggle uses `news.set_enabled` on the active server; `herdr news log` uses the server's history on a remote target; the viewer always restores the terminal, keeps a split escape sequence between reads, and shows story times with their day (`yesterday 14:05`, `Sun 14:05`, `27 Sep`); commands typed into the News pane start with kill-line; `http-cache.json` keeps only this run's sources; the run log is read from the saved offset or its tail; `NewsStartError` is `NewsError`.
- Tab groups: moves refuse multi-pane tabs and the bucket's last tab instead of tearing a tab apart or silently demoting a group; a jittered click on a tab row still focuses it; cross-group drop indicators sit where the tab will land; the focused group's header click and the fold toggle agree with what is drawn; the move-to-group prompt ignores the current group, treats the bucket's name as "ungroup", skips linked worktrees and matches exact names first; group keys are inert outside the `tabs` layout. Transcript store: reported paths are restricted to `<id>.jsonl` under a `projects` directory, restores never replace a native file, shrinking transcripts keep the previous copy, session.json is written before the shutdown backup pass, a stale reported path falls back to the glob, and stale temp files are cleaned.
- Suspend refuses agents that are blocked on a prompt, prompts and send-keys refuse suspended panes, activation waits for the observed exit, a failed process probe retries instead of ending the exit wait, and dropping a suspended record emits a status event. The tabs-layout input lock now also covers mouse gestures and selections on a suspended pane, the card occludes graphics, and `ui.tab_agent_glyphs` honours an `other` override.

### Changed
- Agents model v2 replaces the managed-agents registry: `managed.json` is no longer written (migrated once into pane meta — role, note, opener — with `agents-v2.json` recording matched and unmatched entries; kept for rollback); `agents_manage` sets the pane meta (`project` folds into the note) and `agents_unmanage` answers `unsupported`; `coordinator.get.managed` lists the wake-scope set (`[coordinator] wake_scope`, default `opened`: only agents the coordinator opened wake it); a role survives leaving a team and an exclusion follows the pane; the `+` tab mark is gone and the coordinator row reads "N watched"; the dashboard lists every tab.
- Managed agents now include team members, for messaging only: a team member without a registry entry can message, wait and read, but not open, rename or move tabs or create groups (agents_open_tab and the other tab tools stay registry-managed). An older build drops a session's teams on its next save; closed-session reopen does not restore team membership (the reopened agent joins again by detection, without its role); reordering a team group into the first position keeps its team.
- Agents wrap: the wrap is off unless `[agents] wrap` (or a written legacy `[browser] wrap_agents`) turns it on — it used to default on. The shell hook moved to Settings → Agents and is never installed automatically: plain `herdr browser setup` no longer touches `~/.zshrc` (only `--shell [--remove]`), `browser.settings.set shell_hook` answers `moved`, `browser.fix` and the browser checks no longer include it, and `herdr browser doctor --json` drops it. `browser.settings.set wrap_agents` / `steer_wrap` write `[agents] wrap` and remove the legacy key. `herdr browser wrap` stays as an alias of `herdr agent wrap`; Codex's `--no-daemon` is no longer added twice. The browser settings section lost its shell-hook row; its steering rows say "(when wrapped)".
- Background work no longer counts as working: a Claude Code agent whose own turn ended is `idle` (or `done`) while its background agents, shells or MCP tasks run — everywhere (agent records, events, the client snapshot, rollups, sorting, message delivery and coordinator wake-ups). The subagent count stays on the record and the `⚭` glyph, and suspend/restart still refuse while it is not zero; the finish happens when the turn ends, not when the last background agent does. The bundled Claude manifest (2026.10.03.1) adds `turn_ended_idle`: Claude 2.1.288 keeps its title spinner while background work runs and hides the in-screen spinner while it streams a reply, so the state follows its turn-end status line (`✻ Waiting for N background agents to finish`, `✻ Worked for 1m · … · N MCP tasks still running`) when that line ends the screen above the prompt box; upstream's background_agents_working and background_mcp_task_working rules are gone. Earlier fork builds held such an agent at working until the last background agent ended. In the `tabs` sidebar every working tab's agent glyph breathes: a cosine-eased RGB fade between its color and a dim one about every two seconds, focused and unfocused rows alike, redrawn at about 10 fps only while a visible tab is working (a two-step toggle when the colors cannot be resolved to RGB; no terminal blink).
- AI news desk: the first run no longer waits for a slot, and the `tabs` sidebar's pinned News row is always there while news is enabled. When news is enabled on a desk that has never run (no record in `runs/index.jsonl` and no edition in `editions/index.json`, what `herdr news status` / `news.history` read), the server starts one run (trigger `scheduled`) on the next scheduler pass: at server start with `news.enabled = true`, or when a config reload (so `news.set_enabled` and `herdr news enable` too) turns it on (`NewsState::first_run_pending`, set on the off-to-on switch, dropped on a switch-off, consumed once; the check waits while a run is in flight, whose record then makes the desk one that ran). A failed first run is a run; a first start that fails (no record) stands for it like a slot's, so nothing retries every tick; quiet hours still only hold notifications; afterwards the fixed times apply. The pinned row now shows whenever `news.get` reports news enabled, also without a News tab (the list gives it the row either way): the status keeps its order (`running`, `unread`, `failed`, `paused`) and otherwise reads `next 13:00`, `due`, or without a scheduled slot the last run's local time or `never run` (was `—`); a left click focuses the News tab, or without one sends `news.open` (which creates it in the first space with the page viewer and focuses it); the right-click menu (Run now, Open, Pause / Resume schedule) works with or without the tab. With news disabled the row still shows only while the News tab exists. No wire change, no new method.
- AI news desk: fixed local times instead of an interval. `[news] times = ["08:00", "13:00", "19:00"]` (24-hour `HH:MM`, local; invalid entries dropped with a config diagnostic, duplicates collapsed, kept sorted; an empty list schedules nothing while `enabled` stays a separate switch) replaces `interval_hours`, which now only reports a diagnostic ("replaced by news.times"). The next run is the next listed time later today, else the first one tomorrow; a manual run does not move the schedule (`news.json` remembers when the last run of any trigger started instead of a next-run time, so `next_run_at` is derived on every read); a scheduled run stands for its slot; a slot missed while herdr was off runs once on return when it was earlier today (local) and no run has started since it, several missed slots collapsing to one; a scheduled start that fails counts as the slot's attempt (the next listed time tries again). A `news.json` without that memory (written before this change) starts from the next slot. `quiet_hours` no longer affects scheduling and only holds a notification until the window ends. New socket method `news.set_times` ({ times: [String] }, validates `HH:MM`, writes `news.times` and reloads, answers `news.get`'s record); `news.status` and `news.get` carry `times` (sorted `HH:MM`) instead of `interval_hours` and keep `next_run_at`. CLI: `herdr news times` lists the times with the next one marked, `herdr news times 08:00 13:00 19:00` sets them, `--clear` empties the list; `herdr news status` prints `times 08:00 13:00 19:00` instead of `every 6 h`. The settings news tab shows `scheduled runs: on|off`, one row per time (Enter edits it with the daily reminder's time picker, Delete or Backspace removes it), `add time`, `quiet hours … (notifications)` and `run now`, every time change going to the active server as `news.set_times`; the facts below are the next run, the last run and the model. The sidebar row (`next 13:00`) and the runner's `--next-run` are unchanged. No wire change beyond the new method.
- Background Claude Code subagents stay counted after the main turn ends: the Claude integration (v11) adds a `Stop` hook that reports every subagent still running from Claude's `background_tasks` (`pane.report_subagent` `snapshot`, which replaces the pane's set), the set survives idle and finished turns and a transient unknown state (cleared on exit, another agent, a new conversation, suspend or release; an older Claude Code without the field keeps the old clear-on-idle), and Claude's internal helper agents (no agent type) are no longer reported. The `tabs` sidebar shows `⚭` for a working, idle or finished agent with subagents running, in its status color (blocked keeps `×`), and `agent.suspend` / `agent.restart` (so the menu items and keys too) refuse such an agent with `agent_subagents_running`, since exiting would stop them. Agent status, finished notifications, important reminders and the sort order are unchanged.
- The tab menu's important toggle reads `Important`, left-aligned like the other items, with `✓` after the label (`Important ✓`) when the tab is important; the menu keeps its width.
- The settings overlay's `reminders` tab edits `ui.daily_reminder_time`: a `daily at HH:MM` row below the intervals opens a list of 24-hour times in 30-minute steps (a configured time off the grid shows first as `custom: HH:MM`), Enter writes it and Esc goes back. The running client applies it at once: a new time still ahead today fires today, one already past waits for tomorrow, and the change itself never fires a reminder.
- In-app notification cards and the mobile banner show the notification's own glyph where the `●` was, in the same color: `✓` for a finished agent, `×` for one that needs attention, `★` for an important-tab reminder and the interval's remind marker (`◷` / `◑` / `☼`) for a scheduled one (the reminder engine tags its cards; nothing is read from the title). Update and custom notices keep `●`.
- In the `tabs` sidebar layout, agent notifications name the tab instead of the agent and drop the space number: "level plan finished" with the body "claude · leap-bi-4" (agent, then the basename of the pane's directory, else the space's name) instead of "claude finished" / "leap-bi-4 · 1 · level plan". The client rewrites them once as they leave its pending list, so in-app cards, the mobile banner and terminal/system notifications agree; idle reminder bodies use the same "agent · directory" form in both layouts. A tab the client cannot resolve and the `spaces` layout keep the server's text.
- `agent.suspend` (and so `herdr agent suspend`, the "Suspend agent" menu item and `keys.toggle_agent_suspend`) refuses a `working` agent with `agent_working`, the same guard `agent.restart` uses, and sends it no input.

### Added
- herdr browser, v1b (visible, act, batch): the `tabs` sidebar gains a pinned Browser row directly above the News row (a state glyph — `◎` running, lit accent while an agent uses a tab, `◐` starting, `◉` dialog open, `×` crashed, `◌` stopped — `Browser` and a short status such as `3 tabs · 2 agents`; hidden while the feature is disabled and no profile has run; left click opens the overlay, right-click offers Focus window, Start / Stop profile and Open overlay), tab rows whose pane used the browser within `browser.active_glyph_secs` (default 120) wear `◎` first in the marker slot, and a Browser overlay (`keys.open_browser`, unset by default; the row; the menu) lists each profile with its tabs, newest activity first — short id, who opened it, who last did what and when, title, URL, a dialog flag — with Enter = `browser.focus` on the tab, `s` = start / stop the profile, Esc closes; on a remote server the list is read-only. The client pulls `browser.get {since_seq}` on the first snapshot of a connection, after every `browser.*` reply, every 2 s while a profile runs or starts or the overlay is open, else every 15 s, and never from a server without the method; pane cursors now carry the pane's herdr tab so the marker survives restarts. The act family arrives: `herdr browser click|type|press|select|fill|hover` (MCP `browser_click` …), targets by aria ref or CSS selector, `force` on background tabs (never an implicit raise), `stale_ref` when a ref is gone, a password field (`input[type=password]`, autocomplete current-/new-password, the focused element for a bare press) refused with `password_field_refused` unless `[browser] type_into_password_fields = true`, the whole family off with `[browser] allow_act = false` (`act_disabled`); the ledger records `act:<kind>` with the element's role and name and a text length, never the text. `herdr browser batch` (MCP `browser_batch`) runs up to 20 steps on the current tab under one deadline, each checked and logged like a single call plus one `batch` entry, stopping at the first error unless told otherwise, optionally ending with a snapshot or screenshot. Leftovers: `[browser] stop_with_server` now closes the browsers at server exit (never on a live handoff), and `open` reports the first document's HTTP status.
- herdr browser, v1a (runtime): the server launches a Chromium of its own for agents (`[browser] executable`, our Chromium.app build; never Google Chrome) on a herdr-picked fixed loopback DevTools port with `--disable-blink-features=AutomationControlled` and no automation switch, through LaunchServices on macOS (`open -n -g -a … --args`, so a server in the persistent user service context still gets an on-screen window; the pid comes from the profile's `SingletonLock`), lazily on the first call (`autostart = false`), one persistent profile per name under `state_dir()/browser/profiles/<name>` (`main` by default, `--profile new` for a temporary one), relaunched with `--restore-last-session`; the window survives herdr restarts (the next server reattaches through `run/<name>.json` + `GET /json/version`), a profile held by a Chromium herdr did not launch is `profile_in_use`, a crash or Cmd-Q is reported and not auto-relaunched. One disposable playwright-core 1.63.0 sidecar per server (`host.mjs`, embedded, installed under `browser/host/` by `herdr browser setup`, which runs `npm ci`, records the node in `runtime.json` and registers the `herdr-browser` MCP server for Claude Code in user scope as `sh -c 'exec "${HERDR_BIN_PATH:-<herdr>}" browser mcp'`) attaches with `connectOverCDP({noDefaults: true})`, keeps a no-op dialog listener (dialogs stay open for the user), opens agent tabs with `Target.createTarget {background: true}` and keeps console/network ring buffers; it is killed after two missed pings and respawned on demand, and an open page dialog that blocks a fresh attach is reported as `attach_blocked`. New socket method `browser.run` (connection lane, one tagged `BrowserOp`: open, navigate, history, read markdown|text|snapshot|html with paging, find, links, screenshot with a downscaled inline copy, console, network, wait, scroll, eval, dialog, tabs, use, close, focus; no `act` yet) carries the calling pane, resolved once through `browser.resolve_caller`; every call lands in a ledger (`browser.json` + `browser-activity.jsonl` next to `session.json`: who opened and last used each tab, each pane's current tab, the last 500 operations) that `browser.get` (client shell, `since_seq`), `browser.status` and `browser.log` report. App-lane `browser.focus`, `browser.start`, `browser.stop`, `browser.profiles`, `browser.profile_create`, `browser.profile_delete`. CLI `herdr browser …` with one header line per result and `next:` paging footers, `herdr browser mcp` (18 tools, same text), `herdr browser setup`, `herdr browser doctor`. `[browser]` config: enabled, autostart, stop_with_server, default_profile, executable, node, extra_args (automation switches dropped), restore_tabs, read_max_chars, snapshot_max_chars, screenshot_max_px, screenshot_keep, active_seconds, allow_eval, launch_timeout_ms, op_timeout_ms. No wire change beyond the new methods (PROTOCOL_VERSION 25, endpoint generation 1). The pinned Browser row, the overlay and `act` follow in v1b.
- AI news desk, phase 4 (notifications): a finished run that changed the page and asked for one (`decision.notify`) queues at most one notification, two per local day, a `high` urgency one past the cap once a day; during quiet hours it waits for their end. It goes out the way `notification.show` does (a `Custom` semantic notification to every client shell, under the same one-per-second rate limit), titled `News: <title>` with the editor's body and the News pane as its target (a click focuses the tab; high urgency plays the done sound); without a client shell it stays queued in `news.json` and goes out when one attaches. The third failed run in a row queues one `News runs failing` alert (`N in a row · <last error>`), not repeated until a success. `herdr news status` and `news.get` report the pending count. No wire change.
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
