//! Autosuspend (fork): park Claude agents left idle, so long-idle sessions
//! stop holding processes (`[agents.autosuspend] after`, off by default).
//!
//! About once a minute ([`SCAN_EVERY`], a server-loop deadline only while
//! the setting is on) the scan suspends every Claude agent that has sat
//! idle and seen for `after`, like `agent.suspend`, and marks the record as
//! herdr's own (`SuspendedAgent.auto`, persisted as `auto_suspended`). The
//! idle time is the agent's state time (`agent_state_since_unix_ms`, which
//! survives restarts), counted from the later of that and the last scan
//! that saw the pane on an attached client's screen.
//!
//! Never suspended, cheapest check first: anything but a live Claude agent;
//! a status other than idle-and-seen (a blocked agent, or one that finished
//! and its user has not looked at yet: both wait on an answer, whatever
//! their idle time); a pane on screen in an attached client (the server
//! passes the panes of every client's shown tab); the coordinator; an
//! agent with subagents or background tasks running (shells, monitors,
//! session crons; the Claude integration v16 Stop snapshot), or one that has
//! not sent a snapshot since herdr started watching it (its background work
//! is unknown); messages queued or quiet messages stored for it; a turn its
//! hooks hold live; a handoff run; voice mode; a launch still starting; an
//! installed Claude integration older than v16; then the typing guard (the
//! user typed in it within 10 s, a draft in its input box, a dialog on its
//! screen) and every check `agent.suspend` makes. Only candidates reaching
//! the typing guard read anything beyond in-memory state.
//!
//! A message to an auto-suspended agent wakes it (src/app/message_queue.rs:
//! `App::wake_auto_suspended`, from the queue's delivery, its pass, the
//! urgent path and quiet messages; a quiet message whose wake failed is
//! retried by the scan); activating it clears the mark. A message to an
//! agent its user suspended waits as before. Nothing here runs in render
//! paths.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::App;
use crate::terminal::TerminalId;

/// How often the scan runs while autosuspend is on.
pub(crate) const SCAN_EVERY: Duration = Duration::from_secs(60);
/// The Claude integration version whose Stop hook reports background tasks.
// Only the non-test check reads it (tests set the answer).
#[cfg_attr(test, allow(dead_code))]
pub(crate) const AUTOSUSPEND_MIN_CLAUDE_INTEGRATION: u32 = 16;

/// The scan's runtime state (never persisted).
#[derive(Debug, Default)]
pub(crate) struct AutosuspendState {
    /// The next scan; `None` while the setting is off (the first scan after
    /// it is turned on runs at once).
    pub(crate) next_scan: Option<Instant>,
    /// When each terminal was last seen on an attached client's screen
    /// (wall-clock milliseconds), by the scans; pruned to live terminals.
    pub(crate) last_shown_ms: HashMap<TerminalId, u64>,
    /// Tests: the installed Claude integration's answer.
    #[cfg(test)]
    pub(crate) hook_current_for_test: Option<bool>,
}

impl App {
    /// Whether the scan has work: autosuspend is on, or (also while it is
    /// off) an auto-suspended agent has quiet messages whose wake failed.
    fn agent_autosuspend_active(&self) -> bool {
        self.agents_config.autosuspend.after().is_some() || self.quiet_wake_pending()
    }

    /// An auto-suspended agent has quiet messages stored for it.
    fn quiet_wake_pending(&self) -> bool {
        !self.quiet.is_empty()
            && self.state.terminals.values().any(|terminal| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .is_some_and(|record| record.auto)
                    && self.quiet.count_for(terminal.id.as_str()) > 0
            })
    }

    /// Whether the scan needs the panes clients show (autosuspend is on).
    pub(crate) fn agent_autosuspend_needs_shown(&self) -> bool {
        self.agents_config.autosuspend.after().is_some()
    }

    /// The next scan's deadline; `None` while autosuspend is off and no
    /// quiet wake waits.
    pub(crate) fn next_agent_autosuspend_deadline(&self) -> Option<Instant> {
        if !self.agent_autosuspend_active() {
            return None;
        }
        Some(
            self.agent_autosuspend
                .next_scan
                .unwrap_or_else(Instant::now),
        )
    }

    /// Whether a scan is due now (the server then gathers the shown panes).
    pub(crate) fn agent_autosuspend_due(&self, now: Instant) -> bool {
        self.agent_autosuspend_active()
            && self
                .agent_autosuspend
                .next_scan
                .is_none_or(|deadline| now >= deadline)
    }

    /// One scan, when due: `shown` are the terminals of the panes attached
    /// clients show. Returns whether anything was suspended.
    pub(crate) fn run_agent_autosuspend(
        &mut self,
        now: Instant,
        shown: &HashSet<TerminalId>,
    ) -> bool {
        if self.agent_autosuspend.next_scan.is_some_and(|at| now < at) {
            return false;
        }
        self.agent_autosuspend.next_scan = Some(now + SCAN_EVERY);
        // Quiet wakes retry whether or not autosuspend is still on.
        let mut changed = self.retry_quiet_wakes(now);
        let Some(after) = self.agents_config.autosuspend.after() else {
            // Off: nothing is suspended; the next scan (if any quiet wake
            // still waits) is a minute away, so this never spins.
            self.agent_autosuspend.last_shown_ms.clear();
            return changed;
        };
        let now_ms = crate::codex_sessions::now_unix_ms();
        self.note_shown_terminals(shown, now_ms);
        let candidates = self.autosuspend_candidates(after, now_ms, now, shown);
        if candidates.is_empty() || !self.autosuspend_hook_current() {
            return changed;
        }
        for target in candidates {
            match self.suspend_agent_auto(&target) {
                Ok(pane) => {
                    tracing::info!(
                        event = "agent.autosuspend",
                        pane = %pane,
                        after_s = after.as_secs(),
                        "suspended an idle agent"
                    );
                    changed = true;
                }
                Err(err) => {
                    let body = self.agent_suspend_error_body(err);
                    tracing::debug!(
                        event = "agent.autosuspend.skipped",
                        pane = %target,
                        code = %body.code,
                        "an idle agent was not suspended"
                    );
                }
            }
        }
        changed
    }

    /// Auto-suspended agents with quiet messages stored for them (their
    /// wake failed when the message came): try again. Queued messages retry
    /// in the queue pass instead. Returns whether one woke.
    fn retry_quiet_wakes(&mut self, now: Instant) -> bool {
        if self.quiet.is_empty() {
            return false;
        }
        let waiting: Vec<String> = self
            .state
            .terminals
            .values()
            .filter(|terminal| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .is_some_and(|record| record.auto)
                    && self.quiet.count_for(terminal.id.as_str()) > 0
            })
            .map(|terminal| terminal.id.to_string())
            .collect();
        let mut woke = false;
        for terminal_id in waiting {
            if let Some(target) = self.message_target(&terminal_id, None) {
                self.wake_auto_suspended(&target, now);
                woke |= !self.auto_suspended(&terminal_id);
            }
        }
        woke
    }

    /// Stamp `now_ms` on the shown terminals; forget terminals that are gone.
    fn note_shown_terminals(&mut self, shown: &HashSet<TerminalId>, now_ms: u64) {
        let live = &self.state.terminals;
        self.agent_autosuspend
            .last_shown_ms
            .retain(|terminal_id, _| live.contains_key(terminal_id));
        for terminal_id in shown {
            self.agent_autosuspend
                .last_shown_ms
                .insert(terminal_id.clone(), now_ms);
        }
    }

    /// The public pane ids of the agents to suspend now (see the module docs).
    fn autosuspend_candidates(
        &self,
        after: Duration,
        now_ms: u64,
        now: Instant,
        shown: &HashSet<TerminalId>,
    ) -> Vec<String> {
        let after_ms = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        let mut candidates = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (pane_id, pane) in workspace.tabs.iter().flat_map(|tab| tab.panes.iter()) {
                let terminal_id = &pane.attached_terminal_id;
                let Some(terminal) = self.state.terminals.get(terminal_id) else {
                    continue;
                };
                if !self.autosuspend_eligible(
                    ws_idx, *pane_id, pane.seen, terminal, after_ms, now_ms, now, shown,
                ) {
                    continue;
                }
                if let Some(public) = self.public_pane_id(ws_idx, *pane_id) {
                    candidates.push(public);
                }
            }
        }
        candidates
    }

    #[allow(clippy::too_many_arguments)] // One pane's facts, each from a different source.
    fn autosuspend_eligible(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        seen: bool,
        terminal: &crate::terminal::TerminalState,
        after_ms: u64,
        now_ms: u64,
        now: Instant,
        shown: &HashSet<TerminalId>,
    ) -> bool {
        let terminal_id = &terminal.id;
        if terminal.suspended_agent.is_some()
            || terminal.effective_known_agent() != Some(crate::detect::Agent::Claude)
        {
            return false;
        }
        // Idle and seen only: a blocked agent and a finished one its user
        // has not looked at both wait on an answer.
        if crate::workspace::agent_status(terminal.state, seen, false)
            != crate::api::schema::AgentStatus::Idle
        {
            return false;
        }
        if shown.contains(terminal_id) || self.is_coordinator_pane(ws_idx, pane_id) {
            return false;
        }
        let Some(since) = terminal.agent_state_since_unix_ms else {
            return false;
        };
        let last_shown = self
            .agent_autosuspend
            .last_shown_ms
            .get(terminal_id)
            .copied()
            .unwrap_or(0);
        if now_ms.saturating_sub(since.max(last_shown)) < after_ms {
            return false;
        }
        // Its background work must be known (a Stop snapshot since herdr
        // started watching it) and none may run.
        if !terminal.subagent_snapshot_seen()
            || terminal.active_subagent_count() > 0
            || terminal.active_background_task_count() > 0
        {
            return false;
        }
        let id = terminal_id.as_str();
        if self.message_queue.queued_for(id) || self.quiet.count_for(id) > 0 {
            return false;
        }
        if self.hook_live_until(id).is_some()
            || self.agent_handoff.runs.contains_key(terminal_id)
            || terminal.agent_voice().active()
            || terminal.managed_agent_launch_pending()
        {
            return false;
        }
        let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
            return false;
        };
        if runtime.program_status_blocked() {
            return false;
        }
        crate::app::typing_guard::runtime_typing_block(
            Some(crate::detect::Agent::Claude),
            runtime,
            now,
        )
        .is_none()
    }

    /// Whether the installed Claude integration reports background tasks
    /// (v16 or newer); read at most once per scan, only with a candidate.
    fn autosuspend_hook_current(&self) -> bool {
        #[cfg(test)]
        {
            self.agent_autosuspend.hook_current_for_test.unwrap_or(true)
        }
        #[cfg(not(test))]
        {
            let current = crate::integration::installed_integration_statuses()
                .iter()
                .any(|status| {
                    status.target == crate::api::schema::IntegrationTarget::Claude
                        && status
                            .installed_version
                            .is_some_and(|version| version >= AUTOSUSPEND_MIN_CLAUDE_INTEGRATION)
                });
            if !current {
                tracing::debug!(
                    event = "agent.autosuspend.skipped",
                    "the installed Claude integration is older than v16; reinstall it for autosuspend"
                );
            }
            current
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    const SESSION: &str = "6f1c7a8e-0d1b-4c4e-9a52-3c1f0b2d4e5f";
    const HOUR_MS: u64 = 3_600_000;

    fn root_terminal_id(app: &App) -> TerminalId {
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone()
    }

    fn terminal(app: &mut App) -> &mut crate::terminal::TerminalState {
        let terminal_id = root_terminal_id(app);
        app.state.terminals.get_mut(&terminal_id).unwrap()
    }

    fn set_seen(app: &mut App, seen: bool) {
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&root)
            .unwrap()
            .seen = seen;
    }

    /// A live Claude agent, idle and seen for two hours, its Stop snapshot
    /// in (nothing running), its input box free, with `after = "1h"`.
    fn idle_claude(screen: &[u8]) -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("autosuspend")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = crate::app::Mode::Terminal;
        app.agents_config.autosuspend.after = "1h".into();
        let now_ms = crate::codex_sessions::now_unix_ms();
        {
            let terminal = terminal(&mut app);
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            terminal
                .set_agent_session_ref(
                    "herdr:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id(SESSION),
                    Some(1),
                )
                .expect("session ref accepted");
            terminal.set_agent_name("reviewer".into());
            terminal.replace_subagents([]);
            terminal.agent_state_since_unix_ms = Some(now_ms - 2 * HOUR_MS);
        }
        set_seen(&mut app, true);
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.test_process_pty_bytes(screen);
        let terminal_id = root_terminal_id(&app);
        app.terminal_runtimes.insert(terminal_id, runtime);
        (app, rx)
    }

    fn free() -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        idle_claude(crate::app::typing_guard::TEST_FREE_INPUT_BOX)
    }

    /// One scan now, due or not, with `shown` on screen.
    fn scan_with(app: &mut App, shown: &HashSet<TerminalId>) -> bool {
        app.agent_autosuspend.next_scan = None;
        app.run_agent_autosuspend(Instant::now(), shown)
    }

    fn scan(app: &mut App) -> bool {
        scan_with(app, &HashSet::new())
    }

    fn auto_suspended(app: &mut App) -> bool {
        terminal(app)
            .suspended_agent
            .as_ref()
            .is_some_and(|record| record.auto)
    }

    fn parked(app: &mut App) -> bool {
        terminal(app).suspended_agent.is_some()
    }

    #[tokio::test]
    async fn an_idle_seen_claude_past_the_time_is_suspended_and_marked() {
        let (mut app, mut rx) = free();
        assert!(app.agent_autosuspend_due(Instant::now()), "on: due at once");
        assert!(scan(&mut app));
        assert!(auto_suspended(&mut app), "herdr's own mark");
        let input = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the exit input arrives")
            .expect("runtime channel open");
        assert_eq!(input, bytes::Bytes::from_static(b"/exit"));
        // The next scan waits a minute.
        let deadline = app.next_agent_autosuspend_deadline().expect("on");
        assert!(deadline > Instant::now() + SCAN_EVERY - Duration::from_secs(5));
        assert!(!app.agent_autosuspend_due(Instant::now()));
        // agent.get / agent.list and the agents directory say so.
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let json = serde_json::to_value(app.agent_info(0, pane).unwrap()).unwrap();
        assert_eq!(json["auto_suspended"], true, "{json}");
        assert_eq!(json["agent_status"], "suspended", "{json}");
        let times = app.agent_times();
        assert!(
            times.is_empty() || times.iter().all(|pane| pane.auto_suspended),
            "the push carries the mark"
        );
        // The session file keeps it.
        let snapshot = crate::persist::capture(
            &app.state.workspaces,
            &app.state.terminals,
            &app.terminal_runtimes,
            app.state.active,
            app.state.selected,
        );
        let saved = snapshot.workspaces[0].tabs[0]
            .panes
            .values()
            .find_map(|pane| pane.suspended_agent.as_ref())
            .expect("a suspended record is saved");
        assert!(saved.auto_suspended);
        // A manual suspend is never marked.
        let (mut app, _rx) = free();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(0, pane).unwrap();
        assert!(app.suspend_agent(&public).is_ok());
        assert!(parked(&mut app) && !auto_suspended(&mut app));
        let json = serde_json::to_value(app.agent_info(0, pane).unwrap()).unwrap();
        assert!(json.get("auto_suspended").is_none(), "{json}");
    }

    #[tokio::test]
    async fn off_by_default_and_off_again_schedules_nothing() {
        let (mut app, _rx) = free();
        app.agents_config.autosuspend = Default::default();
        assert_eq!(app.next_agent_autosuspend_deadline(), None);
        assert!(!app.agent_autosuspend_due(Instant::now()));
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
        app.agents_config.autosuspend.after = "1w".into();
        assert_eq!(app.next_agent_autosuspend_deadline(), None, "invalid = off");
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
    }

    #[tokio::test]
    async fn an_agent_idle_for_less_than_the_time_stays() {
        let (mut app, _rx) = free();
        let now_ms = crate::codex_sessions::now_unix_ms();
        terminal(&mut app).agent_state_since_unix_ms = Some(now_ms - HOUR_MS / 2);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
        // Without a known state time it stays too.
        terminal(&mut app).agent_state_since_unix_ms = None;
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
    }

    #[tokio::test]
    async fn blocked_and_done_but_unseen_agents_are_never_suspended() {
        let (mut app, _rx) = free();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Blocked);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "blocked");
        let (mut app, _rx) = free();
        set_seen(&mut app, false);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "done, its user has not looked");
        let (mut app, _rx) = free();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Working);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "working");
    }

    #[tokio::test]
    async fn the_coordinator_is_never_suspended() {
        let (mut app, _rx) = free();
        app.state.coordinator_terminal_id = Some(root_terminal_id(&app));
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
    }

    #[tokio::test]
    async fn an_agent_on_screen_stays_and_its_clock_restarts_when_it_leaves() {
        let (mut app, _rx) = free();
        let shown: HashSet<TerminalId> = [root_terminal_id(&app)].into_iter().collect();
        assert!(!scan_with(&mut app, &shown));
        assert!(!parked(&mut app), "on screen");
        // Off screen again: the time counts from the last scan that saw it.
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "it was on screen a moment ago");
        let now_ms = crate::codex_sessions::now_unix_ms();
        let terminal_id = root_terminal_id(&app);
        app.agent_autosuspend
            .last_shown_ms
            .insert(terminal_id, now_ms - 2 * HOUR_MS);
        assert!(scan(&mut app));
        assert!(auto_suspended(&mut app));
    }

    #[tokio::test]
    async fn subagents_background_tasks_or_unknown_background_work_keep_it() {
        let (mut app, _rx) = free();
        terminal(&mut app).replace_subagents(["a1"]);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "a subagent runs");
        let (mut app, _rx) = free();
        terminal(&mut app).replace_background_tasks(["shell-1"]);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "a background shell runs");
        let (mut app, _rx) = free();
        terminal(&mut app).replace_background_tasks(["cron:loop"]);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "a /loop is scheduled");
        let (mut app, _rx) = free();
        terminal(&mut app).forget_subagents();
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "no Stop snapshot yet: unknown");
        let (mut app, _rx) = free();
        app.agent_autosuspend.hook_current_for_test = Some(false);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "an integration older than v16");
    }

    #[tokio::test]
    async fn queued_or_quiet_messages_keep_it() {
        let (mut app, _rx) = free();
        let terminal_id = root_terminal_id(&app).to_string();
        let target = app.message_target(&terminal_id, None).expect("target");
        app.queue_herdr_notice(
            &target,
            "hello".into(),
            "a test",
            crate::coordinator::now_unix(),
        );
        assert!(app.message_queue.queued_for(&terminal_id));
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "a queued message");

        let (mut app, _rx) = free();
        let terminal_id = root_terminal_id(&app).to_string();
        let now_unix = crate::coordinator::now_unix();
        app.quiet.push(
            crate::app::quiet_messages::QuietMessage {
                message: crate::coordinator::messages::AgentMessage {
                    unix: now_unix,
                    text: "fyi".into(),
                    id: Some(crate::coordinator::messages::new_id()),
                    ..Default::default()
                },
                terminal_id,
                session: None,
                expires_unix: now_unix + 3600,
                dnd: false,
                offered_unix: None,
                offers: 0,
                release: None,
            },
            false,
        );
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "a quiet message");
    }

    #[tokio::test]
    async fn a_draft_or_a_dialog_keeps_it() {
        let (mut app, _rx) = idle_claude("\r\n─────\r\n❯ half a thought\r\n─────\r\n› ".as_bytes());
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "an unsent draft");
        let (mut app, _rx) = idle_claude(b"\r\nDo you want to proceed?\r\n 1. Yes\r\n 2. No\r\n");
        assert!(!scan(&mut app));
        assert!(!parked(&mut app), "no input box: a dialog");
    }

    #[tokio::test]
    async fn an_agent_suspend_would_refuse_is_skipped() {
        // Codex is not Claude: never a candidate.
        let (mut app, _rx) = free();
        terminal(&mut app).set_detected_state(Some(Agent::Codex), AgentState::Idle);
        assert!(!scan(&mut app));
        assert!(!parked(&mut app));
        // Already suspended (by its user): left as it is.
        let (mut app, _rx) = free();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(0, pane).unwrap();
        assert!(app.suspend_agent(&public).is_ok());
        assert!(!scan(&mut app));
        assert!(parked(&mut app) && !auto_suspended(&mut app));
    }

    /// The parked agent's process leaves the pane (the exit report, then a
    /// bare shell), so activation may relaunch it.
    fn observe_exit(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let observed_at = Instant::now();
        for (agent, state, process_exited, at) in [
            (Some(Agent::Claude), AgentState::Idle, true, observed_at),
            (
                None,
                AgentState::Unknown,
                false,
                observed_at + Duration::from_millis(10),
            ),
        ] {
            app.handle_internal_event(crate::events::AppEvent::StateChanged {
                pane_id,
                agent,
                state,
                visible_blocker: false,
                visible_working: false,
                voice: crate::detect::AgentVoice::Off,
                process_exited,
                observed_at: at,
            });
        }
    }

    fn typed(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> String {
        std::thread::sleep(Duration::from_millis(150));
        let mut out = String::new();
        while let Ok(bytes) = rx.try_recv() {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        out
    }

    fn message(id: &str, body: &str) -> crate::api::schema::AgentMessageSendParams {
        crate::api::schema::AgentMessageSendParams {
            target: "reviewer".into(),
            id: id.into(),
            envelope: format!(
                "[herdr+ message {id} from lead (w9:p1) 10:00 — another agent]\n{body}"
            ),
            text: body.into(),
            unix: 0,
            from_pane: Some("w9:p1".into()),
            from_name: Some("lead".into()),
            ..Default::default()
        }
    }

    fn send(app: &mut App, id: &str, body: &str) -> serde_json::Value {
        let response = app.handle_agent_message_send("req".into(), message(id, body));
        serde_json::from_str(&response).unwrap()
    }

    /// An agent herdr suspended for idling, its process gone.
    async fn parked_auto() -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, mut rx) = free();
        assert!(scan(&mut app));
        let input = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("the exit input arrives")
            .expect("runtime channel open");
        assert_eq!(input, bytes::Bytes::from_static(b"/exit"));
        observe_exit(&mut app);
        assert!(auto_suspended(&mut app));
        (app, rx)
    }

    #[tokio::test]
    async fn a_message_wakes_an_auto_suspended_agent_and_goes_in_once_it_is_ready() {
        let (mut app, mut rx) = parked_auto().await;
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
        assert_eq!(
            reply["result"]["reason"],
            crate::app::message_queue::WAKING_REASON,
            "{reply}"
        );
        assert!(
            !parked(&mut app),
            "activated: the mark went with the record"
        );
        let resume = typed(&mut rx);
        assert!(resume.contains(SESSION), "the resume command: {resume:?}");
        assert!(
            !resume.contains("please review"),
            "not typed into the shell"
        );
        assert_eq!(app.message_queue.entries.len(), 1, "it waits for the agent");

        // The agent comes up: the queue types the message in as for any
        // agent herdr started.
        let now = Instant::now();
        terminal(&mut app).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        for step in 0..8 {
            terminal(&mut app)
                .reconcile_managed_agent_at(now + Duration::from_secs(5 * step), false);
        }
        assert!(
            terminal(&mut app).managed_agent_interactive_ready()
                || !terminal(&mut app).managed_agent_launch_pending()
        );
        for step in 0..4 {
            app.message_queue.mark_due();
            app.message_queue_pass(
                now + crate::app::message_queue::SETTLE * step,
                crate::coordinator::now_unix(),
            );
        }
        let text = typed(&mut rx);
        assert!(
            text.contains("please review") || text.contains("ma1"),
            "{text:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_wake_keeps_the_message_queued_and_tells_the_sender_once() {
        // Suspended, its process not yet gone: activation must wait.
        let (mut app, mut rx) = free();
        assert!(scan(&mut app));
        typed(&mut rx);
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
        assert_eq!(
            reply["result"]["reason"],
            crate::app::message_queue::WAKE_PENDING_REASON,
            "still exiting: the normal wait, {reply}"
        );
        assert!(auto_suspended(&mut app), "still parked, still marked");
        assert_eq!(app.message_queue.entries.len(), 1);
        let terminal_id = root_terminal_id(&app).to_string();
        assert!(app.message_queue.wake_failed.contains_key(&terminal_id));

        // The queue keeps trying; past WAKE_NOTICE_AFTER its sender hears once.
        let since = app.message_queue.wake_failed[&terminal_id].since;
        let later = since + crate::app::message_queue::WAKE_NOTICE_AFTER;
        app.message_queue.mark_due();
        app.message_queue_pass(later, crate::coordinator::now_unix());
        let notices = |app: &App| {
            app.message_queue
                .entries
                .iter()
                .filter(|entry| entry.herdr_notice)
                .count()
        };
        // The sender (w9:p1) is not a live pane here, so its notice cannot be
        // queued; the failure is still marked as told.
        assert!(app.message_queue.wake_failed[&terminal_id].notified);
        let told = notices(&app);
        app.message_queue.mark_due();
        app.message_queue_pass(
            later + Duration::from_secs(30),
            crate::coordinator::now_unix(),
        );
        assert_eq!(notices(&app), told, "once");
        assert!(auto_suspended(&mut app));

        // Once the exit is seen the next try wakes it.
        observe_exit(&mut app);
        app.message_queue.mark_due();
        app.message_queue_pass(
            later + crate::app::message_queue::WAKE_RETRY * 4,
            crate::coordinator::now_unix(),
        );
        assert!(!parked(&mut app), "woken on a retry");
        assert!(!app.message_queue.wake_failed.contains_key(&terminal_id));
        assert!(typed(&mut rx).contains(SESSION));
    }

    #[tokio::test]
    async fn the_scan_retries_waking_an_auto_suspended_agent_with_quiet_messages() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        let terminal_id = root_terminal_id(&app).to_string();
        let now_unix = crate::coordinator::now_unix();
        app.quiet.push(
            crate::app::quiet_messages::QuietMessage {
                message: crate::coordinator::messages::AgentMessage {
                    unix: now_unix,
                    text: "fyi".into(),
                    id: Some(crate::coordinator::messages::new_id()),
                    ..Default::default()
                },
                terminal_id,
                session: None,
                expires_unix: now_unix + 3600,
                dnd: false,
                offered_unix: None,
                offers: 0,
                release: None,
            },
            false,
        );
        assert!(scan(&mut app), "the wake is a change");
        assert!(!parked(&mut app), "woken for its quiet message");
        assert!(typed(&mut rx).contains(SESSION));
    }

    #[tokio::test]
    async fn a_wake_waits_while_the_user_types_in_the_pane() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        let terminal_id = root_terminal_id(&app);
        let typed_at = Instant::now();
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .note_user_input(typed_at);
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
        let reason = reply["result"]["reason"].as_str().unwrap_or_default();
        assert!(reason.contains("its user is typing in it"), "{reply}");
        assert!(auto_suspended(&mut app), "not relaunched over the typing");
        assert!(!typed(&mut rx).contains(SESSION));
        let failure = &app.message_queue.wake_failed[terminal_id.as_str()];
        assert_eq!(
            failure.reason,
            crate::app::message_queue::USER_TYPING_WAKE_REASON
        );
        // Quiet again, the queue's retry wakes it.
        app.message_queue.mark_due();
        app.message_queue_pass(
            typed_at + crate::app::typing_guard::USER_INPUT_QUIET + Duration::from_secs(1),
            crate::coordinator::now_unix(),
        );
        assert!(!parked(&mut app), "woken on the retry");
        assert!(typed(&mut rx).contains(SESSION));
    }

    #[tokio::test]
    async fn a_full_queue_refuses_without_waking() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        let terminal_id = root_terminal_id(&app).to_string();
        let target = app.message_target(&terminal_id, None).expect("target");
        let now_unix = crate::coordinator::now_unix();
        while app.message_queue.entries.len() < crate::app::message_queue::MAX_QUEUED {
            app.queue_herdr_notice(&target, "filler".into(), "a test", now_unix);
        }
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(reply["error"]["code"], "queue_full", "{reply}");
        assert!(auto_suspended(&mut app), "not woken for a refused message");
        assert!(!typed(&mut rx).contains(SESSION));
    }

    #[tokio::test]
    async fn a_wake_that_cannot_find_a_shell_says_so() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        // The pane runs something else now (no available shell).
        let terminal_id = root_terminal_id(&app);
        app.terminal_runtimes
            .get(&terminal_id)
            .expect("runtime")
            .test_set_child_pid(u32::MAX);
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(
            reply["result"]["reason"],
            crate::app::message_queue::WAKE_NO_SHELL_REASON,
            "{reply}"
        );
        assert!(auto_suspended(&mut app));
    }

    #[tokio::test]
    async fn quiet_wakes_keep_retrying_after_autosuspend_is_turned_off() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        app.agents_config.autosuspend = Default::default();
        assert_eq!(app.next_agent_autosuspend_deadline(), None, "nothing waits");
        let terminal_id = root_terminal_id(&app).to_string();
        let now_unix = crate::coordinator::now_unix();
        app.quiet.push(
            crate::app::quiet_messages::QuietMessage {
                message: crate::coordinator::messages::AgentMessage {
                    unix: now_unix,
                    text: "fyi".into(),
                    id: Some(crate::coordinator::messages::new_id()),
                    ..Default::default()
                },
                terminal_id,
                session: None,
                expires_unix: now_unix + 3600,
                dnd: false,
                offered_unix: None,
                offers: 0,
                release: None,
            },
            false,
        );
        assert!(
            app.next_agent_autosuspend_deadline().is_some(),
            "a quiet wake waits"
        );
        // The minute since the scan that suspended it has passed.
        app.agent_autosuspend.next_scan = None;
        assert!(app.agent_autosuspend_due(Instant::now()));
        let now = Instant::now();
        assert!(app.run_agent_autosuspend(now, &HashSet::new()), "woken");
        assert!(!parked(&mut app));
        assert!(typed(&mut rx).contains(SESSION));
        // Off and nothing waiting: no deadline, and a due scan cannot spin.
        assert_eq!(app.next_agent_autosuspend_deadline(), None);
        assert_eq!(app.agent_autosuspend.next_scan, Some(now + SCAN_EVERY));
    }

    #[tokio::test]
    async fn a_manually_suspended_agent_is_not_woken_by_a_message() {
        let (mut app, mut rx) = free();
        let pane = app.state.workspaces[0].tabs[0].root_pane;
        let public = app.public_pane_id(0, pane).unwrap();
        assert!(app.suspend_agent(&public).is_ok());
        typed(&mut rx);
        observe_exit(&mut app);
        let reply = send(&mut app, "ma1", "please review");
        assert_eq!(reply["result"]["outcome"], "queued", "{reply}");
        assert_eq!(reply["result"]["reason"], "suspended", "as before");
        for step in 0..3 {
            app.message_queue.mark_due();
            app.message_queue_pass(
                Instant::now() + crate::app::message_queue::WAKE_RETRY * step,
                crate::coordinator::now_unix(),
            );
        }
        assert!(parked(&mut app), "it waits for its user");
        assert!(!typed(&mut rx).contains(SESSION), "no relaunch");
    }

    /// Fork (restart screen): the pane's resume marks as the agent times push
    /// carries them.
    fn pushed_marks(app: &mut App) -> (Option<String>, Option<String>) {
        terminal(app).last_agent_state_change_seq.get_or_insert(1);
        let pane = app.agent_times().into_iter().next().expect("listed");
        (pane.resuming, pane.resume_failed)
    }

    #[tokio::test]
    async fn a_retried_wake_is_pushed_as_waking_until_the_relaunch_or_it_gives_up() {
        // Still exiting: the wake is retried, the pane shows waking.
        let (mut app, mut rx) = free();
        assert!(scan(&mut app));
        typed(&mut rx);
        assert_eq!(pushed_marks(&mut app), (None, None), "a plain auto-suspend");
        let rev = app.state.agent_times_view_rev;
        let sent_at = Instant::now();
        send(&mut app, "ma1", "please review");
        assert!(app.state.agent_times_view_rev > rev, "clients hear of it");
        assert_eq!(pushed_marks(&mut app), (Some("wake".into()), None));

        // Once the exit is seen the retry relaunches it: the mark goes.
        observe_exit(&mut app);
        app.message_queue.mark_due();
        app.message_queue_pass(
            sent_at + crate::app::message_queue::WAKE_RETRY * 2,
            crate::coordinator::now_unix(),
        );
        assert!(!parked(&mut app), "woken on the retry");
        assert_eq!(
            pushed_marks(&mut app),
            (Some("wake".into()), None),
            "waking until the agent is detected"
        );
        report_back(&mut app);
        assert_eq!(pushed_marks(&mut app), (None, None), "detected");

        // Retries that stop leave the mark to lapse on the scheduler.
        let (mut app, mut rx) = free();
        assert!(scan(&mut app));
        typed(&mut rx);
        send(&mut app, "ma1", "please review");
        let lapse = app
            .state
            .next_suspended_agent_resume_deadline()
            .expect("the mark's lapse is scheduled");
        let rev = app.state.agent_times_view_rev;
        assert!(app.start_pending_agent_restarts(lapse));
        assert!(app.state.agent_times_view_rev > rev);
        assert_eq!(pushed_marks(&mut app), (None, None), "lapsed");

        // A wake failing past WAKE_NOTICE_AFTER gave up.
        let (mut app, mut rx) = free();
        assert!(scan(&mut app));
        typed(&mut rx);
        send(&mut app, "ma1", "please review");
        let terminal_id = root_terminal_id(&app).to_string();
        let since = app.message_queue.wake_failed[&terminal_id].since;
        app.message_queue.mark_due();
        app.message_queue_pass(
            since + crate::app::message_queue::WAKE_NOTICE_AFTER,
            crate::coordinator::now_unix(),
        );
        assert!(app.message_queue.wake_failed[&terminal_id].notified);
        assert_eq!(pushed_marks(&mut app), (None, Some("wake".into())));
        assert!(auto_suspended(&mut app), "still parked");

        // Later retries neither push again nor turn it back to waking, and
        // the failure does not lapse while the record stays.
        let rev = app.state.agent_times_view_rev;
        for step in 1..=4 {
            app.message_queue.mark_due();
            app.message_queue_pass(
                since
                    + crate::app::message_queue::WAKE_NOTICE_AFTER
                    + crate::app::message_queue::WAKE_RETRY * (2 * step),
                crate::coordinator::now_unix(),
            );
        }
        assert!(!app.start_pending_agent_restarts(
            Instant::now() + crate::app::agent_suspend::RESUME_FAILED_MARK_TTL * 2
        ));
        assert_eq!(app.state.agent_times_view_rev, rev, "no push per retry");
        assert_eq!(pushed_marks(&mut app), (None, Some("wake".into())));
        assert!(auto_suspended(&mut app), "still parked");
    }

    /// Detection reports the relaunched Claude.
    fn report_back(app: &mut App) {
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            voice: crate::detect::AgentVoice::Off,
            process_exited: false,
            observed_at: Instant::now() + Duration::from_millis(50),
        });
    }

    #[tokio::test]
    async fn a_wake_that_fails_outright_is_marked_once() {
        let (mut app, mut rx) = parked_auto().await;
        typed(&mut rx);
        // The parked session's resume plan is gone: activation fails for good.
        let record = terminal(&mut app).suspended_agent.as_mut().expect("parked");
        record.session.source = "nobody:knows".into();
        send(&mut app, "ma1", "please review");
        assert!(auto_suspended(&mut app));
        assert_eq!(pushed_marks(&mut app), (None, Some("wake".into())));
        let rev = app.state.agent_times_view_rev;
        let terminal_id = root_terminal_id(&app).to_string();
        let since = app.message_queue.wake_failed[&terminal_id].since;
        app.message_queue.mark_due();
        app.message_queue_pass(
            since + crate::app::message_queue::WAKE_RETRY * 2,
            crate::coordinator::now_unix(),
        );
        assert!(
            app.message_queue.wake_failed[&terminal_id].last_attempt > since,
            "retried"
        );
        assert_eq!(
            app.state.agent_times_view_rev, rev,
            "marked once, pushed once"
        );
        assert_eq!(pushed_marks(&mut app), (None, Some("wake".into())));
    }
}
