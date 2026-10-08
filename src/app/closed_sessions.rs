//! Recently closed agent sessions: `session.closed_list`,
//! `session.closed_reopen` and `session.closed_remove`.
//!
//! Closing a tab, a pane or a space over the API records every resumable
//! agent session (live or suspended) held by the closed panes, with its tab
//! context, in the store next to `session.json`
//! ([`crate::persist::closed_sessions`]). Plain shells are never recorded,
//! nor are panes that leave through `pane.move` or a server shutdown: only
//! the three close handlers record. Reopening creates a tab in the original
//! space (the first space when it is gone) with the tab's label, color and
//! reminders, puts a backed-up transcript back when the agent deleted its
//! own, and types the agent's native resume command into the new shell.

use std::path::PathBuf;
use std::time::SystemTime;

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::agent_resume::PersistedAgentSession;
use crate::agent_resume::{AgentResumePlan, AgentSessionRef, AgentSessionRefKind};
use crate::api::schema::{
    ClosedSessionInfo, ClosedSessionTarget, ResponseResult, TabColor, TabRemindInterval,
};
use crate::persist::closed_sessions;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// The native session an entry names, when its reference is still valid.
fn entry_session(entry: &ClosedSessionInfo) -> Option<PersistedAgentSession> {
    let session_ref = match entry.session_ref_kind {
        AgentSessionRefKind::Id => AgentSessionRef::id(entry.session_id.clone()),
        AgentSessionRefKind::Path => AgentSessionRef::path(entry.session_id.clone()),
    }?;
    Some(PersistedAgentSession {
        source: entry.source.clone(),
        agent: entry.agent.clone(),
        session_ref,
        transcript_path: entry.transcript_path.as_ref().map(PathBuf::from),
    })
}

fn not_found(id: String, entry_id: &str) -> String {
    encode_error(
        id,
        "closed_session_not_found",
        format!("closed session {entry_id} not found"),
    )
}

fn store_unavailable(id: String, err: &std::io::Error) -> String {
    encode_error(
        id,
        "closed_sessions_unavailable",
        format!("failed to update the closed sessions record: {err}"),
    )
}

impl App {
    /// Whether closes are recorded: only for a persisted session, and never
    /// while the server is shutting down.
    fn closed_sessions_enabled(&self) -> bool {
        self.policy.persist_session && !self.state.should_quit
    }

    /// The record for one pane's agent session, when it has a resumable one.
    fn closed_session_entry(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
        closed_at: u64,
    ) -> Option<ClosedSessionInfo> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let tab = workspace.tabs.get(tab_idx)?;
        let terminal_id = tab.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id)?;
        let session = terminal.persistable_agent_session()?;
        // Fork (agents v2): the pane's meta and who closed it ride the side
        // file, keyed by the session (the record makes ids unique later).
        let side = closed_sessions::ClosedAgentRecord {
            agent_meta: Some(terminal.agent_meta().clone()).filter(|meta| !meta.is_empty()),
            closed_by: self.agents_model.close_actor.clone(),
        };
        let cwd = self
            .terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.cwd_for_persistence())
            .unwrap_or_else(|| terminal.cwd.clone());
        Some(ClosedSessionInfo {
            id: closed_sessions::entry_id(
                closed_at,
                &session.session_ref.value,
                std::iter::empty(),
            ),
            source: session.source,
            agent: session.agent,
            session_ref_kind: session.session_ref.kind,
            session_id: session.session_ref.value,
            transcript_path: session
                .transcript_path
                .map(|path| path.display().to_string()),
            label: tab.custom_name.clone(),
            color: tab.color,
            important: tab.important,
            remind_every: tab.remind_every,
            pinned: tab.pinned,
            muted: tab.muted,
            space_id: workspace.id.clone(),
            space_name: workspace.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            cwd: cwd.display().to_string(),
            closed_at,
        })
        .inspect(|entry| {
            self.agents_model
                .closing_records
                .borrow_mut()
                .insert(closed_sessions::session_key(entry), side);
        })
    }

    /// Records for the agent sessions of one pane about to be closed.
    pub(crate) fn closed_session_entries_for_pane(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Vec<ClosedSessionInfo> {
        if !self.closed_sessions_enabled() {
            return Vec::new();
        }
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.find_tab_index_for_pane(pane_id))
            .and_then(|tab_idx| self.closed_session_entry(ws_idx, tab_idx, pane_id, unix_now()))
            .into_iter()
            .collect()
    }

    /// Records for the agent sessions of one tab about to be closed, in
    /// pane order.
    pub(crate) fn closed_session_entries_for_tab(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Vec<ClosedSessionInfo> {
        if !self.closed_sessions_enabled() {
            return Vec::new();
        }
        let closed_at = unix_now();
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .map(|tab| tab.layout.pane_ids())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|pane_id| self.closed_session_entry(ws_idx, tab_idx, pane_id, closed_at))
            .collect()
    }

    /// Records for every agent tab of the spaces about to be closed, in
    /// space and tab order.
    pub(crate) fn closed_session_entries_for_workspaces(
        &self,
        indices: &[usize],
    ) -> Vec<ClosedSessionInfo> {
        if !self.closed_sessions_enabled() {
            return Vec::new();
        }
        indices
            .iter()
            .flat_map(|ws_idx| {
                let tabs = self
                    .state
                    .workspaces
                    .get(*ws_idx)
                    .map_or(0, |workspace| workspace.tabs.len());
                (0..tabs)
                    .flat_map(move |tab_idx| self.closed_session_entries_for_tab(*ws_idx, tab_idx))
            })
            .collect()
    }

    /// Add closed sessions to the record. A failure only logs: the close
    /// itself already happened.
    pub(crate) fn record_closed_sessions(&self, entries: Vec<ClosedSessionInfo>) {
        if entries.is_empty() || !self.closed_sessions_enabled() {
            return;
        }
        let count = entries.len();
        let keys: Vec<String> = entries.iter().map(closed_sessions::session_key).collect();
        let store = closed_sessions::store_path();
        match closed_sessions::record(&store, entries) {
            Ok(written) => {
                // Fork (agents v2): the ids as written, and the side records.
                let ids = written
                    .iter()
                    .filter(|entry| keys.contains(&closed_sessions::session_key(entry)))
                    .map(|entry| entry.id.clone())
                    .collect();
                *self.agents_model.last_closed_ids.borrow_mut() = ids;
                let side: Vec<(String, closed_sessions::ClosedAgentRecord)> = {
                    let mut pending = self.agents_model.closing_records.borrow_mut();
                    keys.iter()
                        .filter_map(|key| pending.remove(key).map(|record| (key.clone(), record)))
                        .collect()
                };
                if let Err(err) = closed_sessions::record_agents(&store, side) {
                    tracing::warn!(err = %err, "failed to record the closed sessions' agent meta");
                }
                tracing::info!(
                    event = "session.closed.record",
                    outcome = "recorded",
                    count,
                    "recorded closed agent sessions"
                )
            }
            Err(err) => tracing::warn!(
                event = "session.closed.record",
                outcome = "error",
                count,
                err = %err,
                "failed to record closed agent sessions"
            ),
        }
    }

    pub(super) fn handle_session_closed_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionClosedList {
                sessions: closed_sessions::load(&closed_sessions::store_path()),
            },
        )
    }

    pub(super) fn handle_session_closed_remove(
        &mut self,
        id: String,
        params: ClosedSessionTarget,
    ) -> String {
        match closed_sessions::remove(&closed_sessions::store_path(), &params.id) {
            Ok(Some(_)) => encode_success(id, ResponseResult::Ok {}),
            Ok(None) => not_found(id, &params.id),
            Err(err) => store_unavailable(id, &err),
        }
    }

    pub(super) fn handle_session_closed_reopen(
        &mut self,
        id: String,
        params: ClosedSessionTarget,
    ) -> String {
        let path = closed_sessions::store_path();
        let Some(entry) = closed_sessions::load(&path)
            .into_iter()
            .find(|entry| entry.id == params.id)
        else {
            return not_found(id, &params.id);
        };
        let not_resumable = |id: String, why: &str| {
            encode_error(
                id,
                "closed_session_not_resumable",
                format!("closed session {} cannot be resumed: {why}", entry.id),
            )
        };
        let Some(session) = entry_session(&entry) else {
            return not_resumable(id, "its session reference is invalid");
        };
        let Some(plan) =
            crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
        else {
            return not_resumable(id, "the agent has no native resume command");
        };
        let cwd = PathBuf::from(&entry.cwd);
        if !cwd.is_dir() {
            return not_resumable(id, &format!("{} no longer exists", entry.cwd));
        }

        let reopened = match self.create_reopened_tab(&entry, cwd) {
            Ok(reopened) => reopened,
            Err(err) => return encode_error(id, "tab_create_failed", err.to_string()),
        };
        // Fork (agents v2): the pane's meta comes back; its turn is unknown.
        let side = closed_sessions::load_agents(&path)
            .remove(&closed_sessions::session_key(&entry))
            .unwrap_or_default();
        if let Some(terminal) = self.state.terminals.get_mut(&reopened.terminal_id) {
            terminal.mark_restored();
            if let Some(meta) = side.agent_meta.clone() {
                *terminal.agent_meta_mut() = meta;
            }
        }
        if let Err(err) = self.launch_closed_session_resume(&reopened.terminal_id, session, &plan) {
            // The tab exists as a plain shell; announce it and keep the entry.
            self.announce_reopened_tab(&reopened);
            return encode_error(id, "closed_session_reopen_failed", err);
        }
        if let Err(err) = closed_sessions::remove(&path, &entry.id) {
            tracing::warn!(
                event = "session.closed.reopen",
                outcome = "remove_failed",
                err = %err,
                "reopened a closed session but could not remove its record"
            );
        }
        tracing::info!(
            event = "session.closed.reopen",
            outcome = "reopened",
            agent = %entry.agent,
            session_id = %entry.session_id,
            "reopened a closed agent session"
        );

        self.announce_reopened_tab(&reopened);
        match self.tab_created_result(reopened.ws_idx, reopened.tab_idx) {
            Some(result) => encode_success(id, result),
            None => encode_error(id, "tab_create_failed", "the reopened tab disappeared"),
        }
    }

    /// Focus the new tab, save the session and emit its creation events.
    fn announce_reopened_tab(&mut self, reopened: &ReopenedTab) {
        self.state
            .switch_workspace_tab(reopened.ws_idx, reopened.tab_idx);
        self.state.mode = super::Mode::Terminal;
        self.state.mark_session_dirty();
        self.schedule_session_save();
        if reopened.new_workspace {
            self.emit_workspace_open_events(reopened.ws_idx);
        } else {
            self.emit_tab_created_events(reopened.ws_idx, reopened.tab_idx);
        }
    }

    /// A new tab for a closed session with its label, color and reminders:
    /// in the original space, else the first one, else a new space.
    pub(crate) fn create_reopened_tab(
        &mut self,
        entry: &ClosedSessionInfo,
        cwd: PathBuf,
    ) -> std::io::Result<ReopenedTab> {
        // Exact ids only: the numeric fallbacks of parse_workspace_id would
        // land a gone space's tabs in whichever space took its place.
        let target = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == entry.space_id)
            .or_else(|| (!self.state.workspaces.is_empty()).then_some(0));
        let (ws_idx, tab_idx, new_workspace) = match target {
            Some(ws_idx) => {
                let (rows, cols) = self.state.estimate_pane_size();
                let default_shell = self.state.default_shell.clone();
                let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
                let host_terminal_theme = self.state.host_terminal_theme;
                let host_terminal_appearance = self.state.host_terminal_appearance;
                let shell_mode = self.state.shell_mode;
                let (tab_idx, terminal, runtime) = self.state.workspaces[ws_idx].create_tab(
                    rows,
                    cols,
                    cwd,
                    scrollback_limit_bytes,
                    host_terminal_theme,
                    host_terminal_appearance,
                    crate::pane::PaneShellConfig::new(&default_shell, shell_mode),
                    Vec::new(),
                )?;
                self.terminal_runtimes.insert(terminal.id.clone(), runtime);
                self.state.terminals.insert(terminal.id.clone(), terminal);
                self.state.remove_alias_shadowed_by_new_pane(
                    self.state.workspaces[ws_idx].tabs[tab_idx].root_pane,
                );
                (ws_idx, tab_idx, false)
            }
            None => (self.create_workspace_with_options(cwd, true)?, 0, true),
        };
        let tab = &mut self.state.workspaces[ws_idx].tabs[tab_idx];
        if let Some(label) = entry.label.clone() {
            tab.set_custom_name(label);
        }
        tab.color = entry.color.filter(|color| *color != TabColor::Unknown);
        tab.important = entry.important;
        tab.remind_every = entry
            .remind_every
            .filter(|every| *every != TabRemindInterval::Unknown);
        tab.pinned = entry.pinned;
        tab.muted = entry.muted;
        let terminal_id = tab
            .terminal_id(tab.root_pane)
            .cloned()
            .ok_or_else(|| std::io::Error::other("the new tab has no terminal"))?;
        // Fork (sidebar v3): a reopened pin reaches clients.
        if entry.pinned {
            self.bump_tab_pins_view();
        }
        // Fork: so does a reopened mute.
        if entry.muted {
            self.bump_tab_mutes_view();
        }
        Ok(ReopenedTab {
            ws_idx,
            tab_idx,
            terminal_id,
            new_workspace,
        })
    }

    /// Put a missing transcript back from the backup store, type the native
    /// resume command into the new shell, and keep the session on the pane
    /// so it is persisted (and backed up) before the agent reports it.
    pub(crate) fn launch_closed_session_resume(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        session: PersistedAgentSession,
        plan: &AgentResumePlan,
    ) -> Result<(), String> {
        let mut input = self
            .resume_shell_command(plan)
            .ok_or_else(|| "the resume command is empty".to_string())?;
        input.push('\r');
        if self.terminal_runtimes.get(terminal_id).is_none() {
            return Err("the new tab has no shell".to_string());
        }
        self.restore_agent_transcript_before_resume(&session);
        // Fork: typed once the new shell reads (src/app/launch_gate.rs),
        // which also records the scripted write for the turn origin.
        self.type_launch_when_ready(terminal_id, input.into_bytes(), std::time::Instant::now())?;
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.set_persisted_agent_session(session);
        }
        Ok(())
    }
}

/// Where [`App::create_reopened_tab`] put the tab.
pub(crate) struct ReopenedTab {
    pub(crate) ws_idx: usize,
    pub(crate) tab_idx: usize,
    pub(crate) terminal_id: crate::terminal::TerminalId,
    pub(crate) new_workspace: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        EmptyParams, Method, PaneMoveDestination, PaneMoveParams, PaneTarget, Request, TabTarget,
        WorkspaceCloseParams,
    };
    use crate::workspace::Workspace;
    use std::path::Path;

    /// A private home and config directory, so the record, the transcript
    /// store and native transcripts never touch the user's. nextest runs
    /// each test in its own process, so the environment is not shared.
    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-closed-sessions-app-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("project")).unwrap();
            std::env::set_var("HOME", root.join("home"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::remove_var(crate::session::SESSION_ENV_VAR);
            Self { root }
        }

        fn project(&self) -> PathBuf {
            self.root.join("project")
        }

        fn entries(&self) -> Vec<ClosedSessionInfo> {
            assert!(closed_sessions::store_path().starts_with(&self.root));
            closed_sessions::load(&closed_sessions::store_path())
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn claude_session(id: &str) -> PersistedAgentSession {
        PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(id).unwrap(),
            transcript_path: None,
        }
    }

    /// Two spaces: the bucket and `group`, each with one tab, every pane a
    /// plain shell in `cwd`; session persistence on.
    fn app(cwd: &Path) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("bucket"), Workspace::test_new("group")];
        app.state.workspaces[1].custom_name = Some("group".into());
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        for terminal in app.state.terminals.values_mut() {
            terminal.cwd = cwd.to_path_buf();
        }
        app.policy.persist_session = true;
        app
    }

    fn terminal_mut(
        app: &mut App,
        ws_idx: usize,
        tab_idx: usize,
    ) -> &mut crate::terminal::TerminalState {
        let tab = &app.state.workspaces[ws_idx].tabs[tab_idx];
        let terminal_id = tab.terminal_id(tab.root_pane).unwrap().clone();
        app.state.terminals.get_mut(&terminal_id).unwrap()
    }

    fn host_session(app: &mut App, ws_idx: usize, tab_idx: usize, id: &str) {
        terminal_mut(app, ws_idx, tab_idx).set_persisted_agent_session(claude_session(id));
    }

    fn park_session(app: &mut App, ws_idx: usize, tab_idx: usize, id: &str) {
        terminal_mut(app, ws_idx, tab_idx).restore_suspended_agent(
            "claude".into(),
            Some("parked".into()),
            claude_session(id),
        );
    }

    fn request(app: &mut App, method: Method) -> serde_json::Value {
        let response = app.handle_api_request(Request {
            id: "req".into(),
            method,
        });
        serde_json::from_str(&response).unwrap()
    }

    fn close_tab(app: &mut App, ws_idx: usize, tab_idx: usize) -> serde_json::Value {
        let tab_id = app.public_tab_id(ws_idx, tab_idx).unwrap();
        request(app, Method::TabClose(TabTarget { tab_id }))
    }

    fn ids(entries: &[ClosedSessionInfo]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.session_id.as_str())
            .collect()
    }

    #[test]
    fn closing_tabs_records_live_and_suspended_agents_but_not_plain_shells() {
        let sandbox = Sandbox::new("tab-close");
        let mut app = app(&sandbox.project());
        let live = app.state.workspaces[1].test_add_tab(Some("review"));
        let parked = app.state.workspaces[1].test_add_tab(None);
        app.state.ensure_test_terminals();
        for terminal in app.state.terminals.values_mut() {
            terminal.cwd = sandbox.project();
        }
        host_session(&mut app, 1, live, "live-session");
        park_session(&mut app, 1, parked, "parked-session");
        {
            let tab = &mut app.state.workspaces[1].tabs[live];
            tab.color = Some(TabColor::Purple);
            tab.important = true;
            tab.remind_every = Some(TabRemindInterval::M30);
            tab.pinned = true;
            tab.muted = true;
        }
        let space_id = app.state.workspaces[1].id.clone();

        // The plain shell tab first: nothing to record.
        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        assert!(sandbox.entries().is_empty());

        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        let entries = sandbox.entries();
        assert_eq!(ids(&entries), ["parked-session", "live-session"]);
        let live = &entries[1];
        assert_eq!(live.agent, "claude");
        assert_eq!(live.source, "herdr:claude");
        assert_eq!(live.label.as_deref(), Some("review"));
        assert_eq!(live.color, Some(TabColor::Purple));
        assert!(live.important);
        assert_eq!(live.remind_every, Some(TabRemindInterval::M30));
        assert!(live.pinned, "the pin is recorded with the session");
        assert!(live.muted, "the mute is recorded with the session");
        assert_eq!(live.space_id, space_id);
        assert_eq!(live.space_name, "group");
        assert_eq!(live.cwd, sandbox.project().display().to_string());
        assert!(live.closed_at > 0);
        assert!(entries[0].label.is_none());
        assert!(!entries[0].important);
    }

    #[test]
    fn closing_a_space_records_each_agent_tab() {
        let sandbox = Sandbox::new("workspace-close");
        let mut app = app(&sandbox.project());
        app.state.workspaces[1].test_add_tab(Some("second"));
        app.state.workspaces[1].test_add_tab(Some("shell"));
        app.state.ensure_test_terminals();
        host_session(&mut app, 1, 0, "first");
        park_session(&mut app, 1, 1, "second");
        let workspace_id = app.public_workspace_id(1);

        let response = request(
            &mut app,
            Method::WorkspaceClose(WorkspaceCloseParams {
                workspace_id,
                close_group: false,
            }),
        );
        assert_eq!(response["result"]["type"], "ok", "{response}");
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(ids(&sandbox.entries()), ["first", "second"]);
    }

    #[test]
    fn closing_a_space_through_its_last_tab_or_pane_records_it() {
        let sandbox = Sandbox::new("last-tab");
        let mut app = app(&sandbox.project());
        host_session(&mut app, 1, 0, "last-tab");
        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(ids(&sandbox.entries()), ["last-tab"]);

        host_session(&mut app, 0, 0, "last-pane");
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, root).unwrap();
        let response = request(&mut app, Method::PaneClose(PaneTarget { pane_id }));
        assert_eq!(response["result"]["type"], "ok", "{response}");
        assert!(app.state.workspaces.is_empty());
        assert_eq!(ids(&sandbox.entries()), ["last-pane", "last-tab"]);
    }

    #[test]
    fn closing_one_pane_of_a_split_records_only_its_session() {
        let sandbox = Sandbox::new("pane-close");
        let mut app = app(&sandbox.project());
        let split = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        host_session(&mut app, 0, 0, "root");
        let split_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(split)
            .unwrap()
            .clone();
        app.state
            .terminals
            .get_mut(&split_terminal)
            .unwrap()
            .set_persisted_agent_session(claude_session("split"));
        let pane_id = app.public_pane_id(0, split).unwrap();

        let response = request(&mut app, Method::PaneClose(PaneTarget { pane_id }));
        assert_eq!(response["result"]["type"], "ok", "{response}");
        assert_eq!(ids(&sandbox.entries()), ["split"]);
    }

    #[test]
    fn nothing_is_recorded_while_shutting_down_or_without_persistence() {
        let sandbox = Sandbox::new("shutdown");
        let mut app = app(&sandbox.project());
        host_session(&mut app, 0, 0, "bucket");
        host_session(&mut app, 1, 0, "group");

        // The shutdown save never closes tabs or records anything.
        app.save_session_on_shutdown();
        assert!(sandbox.entries().is_empty());

        app.state.should_quit = true;
        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        assert!(sandbox.entries().is_empty());

        app.state.should_quit = false;
        app.policy.persist_session = false;
        assert_eq!(close_tab(&mut app, 0, 0)["result"]["type"], "ok");
        assert!(sandbox.entries().is_empty());
        assert!(!closed_sessions::store_path().exists());
    }

    #[test]
    fn a_tab_moved_to_another_group_is_not_recorded() {
        let sandbox = Sandbox::new("move");
        let mut app = app(&sandbox.project());
        app.state.workspaces[0].test_add_tab(Some("stays"));
        app.state.ensure_test_terminals();
        host_session(&mut app, 0, 0, "moving");
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let pane_id = app.public_pane_id(0, root).unwrap();
        let group_id = app.public_workspace_id(1);

        let response = request(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(group_id),
                    label: Some("moved".into()),
                },
                focus: true,
            }),
        );
        assert_eq!(
            response["result"]["move_result"]["changed"], true,
            "{response}"
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 1, "source tab removed");
        assert!(sandbox.entries().is_empty());
    }

    fn record_entry(app: &mut App, ws_idx: usize, id: &str) -> ClosedSessionInfo {
        host_session(app, ws_idx, 0, id);
        app.state.workspaces[ws_idx].tabs[0].custom_name = Some(format!("{id} tab"));
        let entries = app.closed_session_entries_for_tab(ws_idx, 0);
        assert_eq!(entries.len(), 1);
        app.record_closed_sessions(entries.clone());
        entries[0].clone()
    }

    fn reopen(app: &mut App, id: &str) -> serde_json::Value {
        request(
            app,
            Method::SessionClosedReopen(ClosedSessionTarget { id: id.into() }),
        )
    }

    fn long_running_shell(app: &mut App) {
        app.state.default_shell = "/bin/cat".into();
        app.state.shell_mode = crate::config::ShellModeConfig::NonLogin;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reopen_restores_the_tab_in_its_space_and_removes_the_entry() {
        let sandbox = Sandbox::new("reopen");
        let mut app = app(&sandbox.project());
        long_running_shell(&mut app);
        {
            let tab = &mut app.state.workspaces[1].tabs[0];
            tab.color = Some(TabColor::Red);
            tab.important = true;
            tab.remind_every = Some(TabRemindInterval::Daily);
            tab.pinned = true;
            tab.muted = true;
        }
        let entry = record_entry(&mut app, 1, "reopen-session");
        record_entry(&mut app, 0, "other-session");
        let tabs_before = app.state.workspaces[1].tabs.len();

        let listed = request(&mut app, Method::SessionClosedList(EmptyParams::default()));
        assert_eq!(listed["result"]["type"], "session_closed_list");
        assert_eq!(listed["result"]["sessions"].as_array().unwrap().len(), 2);

        let response = reopen(&mut app, &entry.id);
        assert_eq!(response["result"]["type"], "tab_created", "{response}");
        assert_eq!(response["result"]["tab"]["label"], "reopen-session tab");
        assert_eq!(response["result"]["tab"]["color"], "red");
        assert_eq!(response["result"]["tab"]["important"], true);
        assert_eq!(response["result"]["tab"]["remind_every"], "daily");
        assert_eq!(response["result"]["tab"]["pinned"], true);
        assert!(
            app.state.tab_pins_view_rev > 0,
            "the reopened pin reaches clients"
        );
        assert_eq!(response["result"]["tab"]["muted"], true);
        assert!(
            app.state.tab_mutes_view_rev > 0,
            "the reopened mute reaches clients"
        );
        assert_eq!(
            response["result"]["tab"]["workspace_id"],
            app.public_workspace_id(1)
        );
        assert!(response["result"]["root_pane"]["pane_id"].is_string());

        assert_eq!(app.state.workspaces[1].tabs.len(), tabs_before + 1);
        let tab_idx = tabs_before;
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab, tab_idx);
        let tab = &app.state.workspaces[1].tabs[tab_idx];
        let terminal = &app.state.terminals[tab.terminal_id(tab.root_pane).unwrap()];
        assert_eq!(terminal.cwd, sandbox.project());
        assert!(terminal
            .persistable_agent_session()
            .is_some_and(|session| session.same_session(&claude_session("reopen-session"))));
        assert!(app.terminal_runtimes.get(&terminal.id).is_some(), "running");
        assert!(terminal.suspended_agent.is_none(), "not suspended");
        assert_eq!(ids(&sandbox.entries()), ["other-session"]);

        // Gone now.
        let again = reopen(&mut app, &entry.id);
        assert_eq!(again["error"]["code"], "closed_session_not_found");
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reopen_falls_back_to_the_bucket_when_the_space_is_gone() {
        let sandbox = Sandbox::new("bucket");
        let mut app = app(&sandbox.project());
        long_running_shell(&mut app);
        let entry = record_entry(&mut app, 1, "orphan");
        app.state.workspaces.remove(1);

        let response = reopen(&mut app, &entry.id);
        assert_eq!(response["result"]["type"], "tab_created", "{response}");
        assert_eq!(
            response["result"]["tab"]["workspace_id"],
            app.public_workspace_id(0)
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(sandbox.entries().is_empty());

        // No space at all: a new one is created for the tab.
        let entry = record_entry(&mut app, 0, "last");
        app.state.workspaces.clear();
        app.state.active = None;
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
        let response = reopen(&mut app, &entry.id);
        assert_eq!(response["result"]["type"], "tab_created", "{response}");
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("last tab")
        );
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reopen_types_the_native_resume_command_into_the_new_shell() {
        let sandbox = Sandbox::new("command");
        let mut app = app(&sandbox.project());
        let entry = record_entry(&mut app, 1, "typed-session");
        let session = entry_session(&entry).unwrap();
        let plan = crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
            .unwrap();
        // The tab as reopen creates it, with a test runtime in place of the
        // spawned shell so the typed input can be read.
        long_running_shell(&mut app);
        let reopened = app
            .create_reopened_tab(&entry, sandbox.project())
            .expect("tab created");
        if let Some(runtime) = app.terminal_runtimes.remove(&reopened.terminal_id) {
            runtime.shutdown();
        }
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(reopened.terminal_id.clone(), runtime);

        app.launch_closed_session_resume(&reopened.terminal_id, session.clone(), &plan)
            .expect("resume sent");
        let input = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("pane input arrives")
            .expect("runtime channel open");
        let expected = format!(
            "{}\r",
            super::super::agent_resume::shell_command_from_argv(&plan.argv).unwrap()
        );
        assert_eq!(std::str::from_utf8(&input).unwrap(), expected);
        assert!(expected.contains("typed-session"));
        assert_eq!(
            app.state.terminals[&reopened.terminal_id]
                .persisted_agent_session
                .as_ref(),
            Some(&session)
        );
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reopen_types_the_resume_through_the_agent_wrap_while_it_is_on() {
        let sandbox = Sandbox::new("wrapped");
        let mut app = app(&sandbox.project());
        app.agents_config.wrap = Some(true);
        let entry = record_entry(&mut app, 1, "wrapped-session");
        let session = entry_session(&entry).unwrap();
        let plan = crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
            .unwrap();
        long_running_shell(&mut app);
        let reopened = app
            .create_reopened_tab(&entry, sandbox.project())
            .expect("tab created");
        if let Some(runtime) = app.terminal_runtimes.remove(&reopened.terminal_id) {
            runtime.shutdown();
        }
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(reopened.terminal_id.clone(), runtime);

        app.launch_closed_session_resume(&reopened.terminal_id, session.clone(), &plan)
            .expect("resume sent");
        let input = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("pane input arrives")
            .expect("runtime channel open");
        let herdr = crate::platform::launch_executable().unwrap();
        let wrapped: Vec<String> = vec![
            herdr.display().to_string(),
            "agent".into(),
            "wrap".into(),
            "claude".into(),
            "--".into(),
            "--resume".into(),
            "wrapped-session".into(),
        ];
        let expected = format!(
            "{}\r",
            super::super::agent_resume::shell_command_from_argv(&wrapped).unwrap()
        );
        assert_eq!(std::str::from_utf8(&input).unwrap(), expected);
        assert_eq!(
            app.state.terminals[&reopened.terminal_id]
                .persisted_agent_session
                .as_ref(),
            Some(&session)
        );
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reopen_puts_a_deleted_transcript_back_from_the_backup_store() {
        let sandbox = Sandbox::new("transcript");
        let mut app = app(&sandbox.project());
        long_running_shell(&mut app);
        let native = sandbox
            .root
            .join("home/.claude/projects/-tmp-project/restored-session.jsonl");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, "{\"type\":\"user\"}\n").unwrap();
        let session = PersistedAgentSession {
            transcript_path: Some(native.clone()),
            ..claude_session("restored-session")
        };
        let summary = crate::persist::agent_transcripts::sync_backups(
            &crate::persist::agent_transcripts::store_dir(),
            &[crate::persist::agent_transcripts::TranscriptBackupRequest {
                session: session.clone(),
                cwd: None,
                label: None,
            }],
        );
        assert_eq!(summary.updated, 1);
        terminal_mut(&mut app, 1, 0).set_persisted_agent_session(session);
        let entries = app.closed_session_entries_for_tab(1, 0);
        assert_eq!(
            entries[0].transcript_path.as_deref(),
            Some(native.display().to_string().as_str())
        );
        app.record_closed_sessions(entries.clone());
        std::fs::remove_file(&native).unwrap();

        let response = reopen(&mut app, &entries[0].id);
        assert_eq!(response["result"]["type"], "tab_created", "{response}");
        assert_eq!(
            std::fs::read_to_string(&native).unwrap(),
            "{\"type\":\"user\"}\n"
        );
        super::super::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn reopen_and_remove_report_unknown_and_unresumable_entries() {
        let sandbox = Sandbox::new("errors");
        let mut app = app(&sandbox.project());
        let missing = reopen(&mut app, "nope");
        assert_eq!(missing["error"]["code"], "closed_session_not_found");
        let missing = request(
            &mut app,
            Method::SessionClosedRemove(ClosedSessionTarget { id: "nope".into() }),
        );
        assert_eq!(missing["error"]["code"], "closed_session_not_found");

        // A record whose agent has no resume command, and one whose
        // directory is gone: both stay listed.
        let mut custom = record_entry(&mut app, 1, "custom");
        custom.id = "custom".into();
        custom.source = "custom:agent".into();
        custom.agent = "custom".into();
        let mut moved = record_entry(&mut app, 0, "moved");
        moved.id = "moved".into();
        moved.cwd = sandbox.root.join("gone").display().to_string();
        closed_sessions::save(
            &closed_sessions::store_path(),
            &[custom.clone(), moved.clone()],
        )
        .unwrap();
        for id in ["custom", "moved"] {
            let response = reopen(&mut app, id);
            assert_eq!(
                response["error"]["code"], "closed_session_not_resumable",
                "{response}"
            );
        }
        assert_eq!(app.state.workspaces[0].tabs.len(), 1, "no tab created");
        assert_eq!(sandbox.entries().len(), 2);

        let removed = request(
            &mut app,
            Method::SessionClosedRemove(ClosedSessionTarget {
                id: "custom".into(),
            }),
        );
        assert_eq!(removed["result"]["type"], "ok", "{removed}");
        assert_eq!(ids(&sandbox.entries()), ["moved"]);
    }

    #[test]
    fn sessions_without_a_resume_plan_are_recorded_but_not_reopened() {
        let sandbox = Sandbox::new("no-plan");
        let mut app = app(&sandbox.project());
        terminal_mut(&mut app, 1, 0).set_persisted_agent_session(PersistedAgentSession {
            source: "custom:agent".into(),
            agent: "custom".into(),
            session_ref: AgentSessionRef::id("custom").unwrap(),
            transcript_path: None,
        });
        assert_eq!(close_tab(&mut app, 1, 0)["result"]["type"], "ok");
        let entries = sandbox.entries();
        assert_eq!(ids(&entries), ["custom"]);
        let response = reopen(&mut app, &entries[0].id);
        assert_eq!(response["error"]["code"], "closed_session_not_resumable");
        assert_eq!(sandbox.entries().len(), 1);
    }
}
