//! Keep copies of native agent transcripts for open and suspended panes.
//!
//! The copies live under the session directory (see
//! [`crate::persist::agent_transcripts`]). A periodic pass and the shutdown
//! save back up every pane's session; suspending an agent queues that
//! session for a prompt pass; a native resume first puts a copy back when
//! the agent has deleted its own transcript.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use super::App;
use crate::agent_resume::PersistedAgentSession;
use crate::persist::agent_transcripts::{self, BackupSummary, TranscriptBackupRequest};

/// How often open and suspended agent transcripts are backed up.
pub(super) const AGENT_TRANSCRIPT_BACKUP_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How soon a pass is retried when the previous one is still copying.
const AGENT_TRANSCRIPT_BACKUP_RETRY: Duration = Duration::from_secs(1);

/// The outcome of one finished backup pass.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentTranscriptBackupPass {
    pub(crate) finished: SystemTime,
    pub(crate) duration: Duration,
    pub(crate) summary: BackupSummary,
}

/// What `agent.transcripts` reports about the schedule.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentTranscriptBackupSchedule {
    pub(crate) enabled: bool,
    pub(crate) last_pass: Option<AgentTranscriptBackupPass>,
    /// Time until the next periodic pass, when passes run.
    pub(crate) next_pass_in: Option<Duration>,
}

fn run_backup_pass(
    store_dir: &Path,
    requests: &[TranscriptBackupRequest],
) -> AgentTranscriptBackupPass {
    let started = Instant::now();
    let summary = agent_transcripts::sync_backups(store_dir, requests);
    AgentTranscriptBackupPass {
        finished: SystemTime::now(),
        duration: started.elapsed(),
        summary,
    }
}

impl App {
    fn agent_transcript_backups_enabled(&self) -> bool {
        self.backup_agent_transcripts && self.policy.persist_session
    }

    /// Every session to back up right now, with the pane context recorded
    /// in its metadata. Uses the same session selection as the snapshot
    /// writer so the backup matches what a restore would resume.
    pub(crate) fn agent_transcript_backup_requests(&self) -> Vec<TranscriptBackupRequest> {
        let mut seen = HashSet::new();
        let mut requests = Vec::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for pane_id in tab.panes.keys() {
                    let Some(request) = self.agent_transcript_backup_request(tab, *pane_id) else {
                        continue;
                    };
                    if seen.insert(request_key(&request)) {
                        requests.push(request);
                    }
                }
            }
        }
        requests
    }

    /// The pane sessions plus everything queued ahead of the interval,
    /// draining the queue. A queued session is kept even when its pane has
    /// closed since: it was queued because it must survive.
    fn take_agent_transcript_backup_requests(&mut self) -> Vec<TranscriptBackupRequest> {
        let mut requests = self.agent_transcript_backup_requests();
        let pending = std::mem::take(&mut self.agent_transcript_backup_pending);
        if !pending.is_empty() {
            let present: HashSet<String> = requests.iter().map(request_key).collect();
            requests.extend(
                pending
                    .into_iter()
                    .filter(|(key, _)| !present.contains(key))
                    .map(|(_, request)| request),
            );
        }
        requests
    }

    fn agent_transcript_backup_request(
        &self,
        tab: &crate::workspace::Tab,
        pane_id: crate::layout::PaneId,
    ) -> Option<TranscriptBackupRequest> {
        let terminal_id = tab.terminal_id(pane_id)?;
        let terminal = self.state.terminals.get(terminal_id)?;
        let session = terminal.persistable_agent_session()?;
        Some(self.agent_transcript_backup_request_for(tab, terminal_id, session))
    }

    fn agent_transcript_backup_request_for(
        &self,
        tab: &crate::workspace::Tab,
        terminal_id: &crate::terminal::TerminalId,
        session: PersistedAgentSession,
    ) -> TranscriptBackupRequest {
        let cwd = self
            .terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.cwd_for_persistence())
            .or_else(|| {
                self.state
                    .terminals
                    .get(terminal_id)
                    .map(|terminal| terminal.cwd.clone())
            });
        TranscriptBackupRequest {
            session,
            cwd,
            label: tab.custom_name.clone(),
        }
    }

    pub(crate) fn agent_transcript_backup_due(&self, now: Instant) -> bool {
        self.agent_transcript_backup_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    fn reap_finished_agent_transcript_backup(&mut self) {
        if self
            .agent_transcript_backup_thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            if let Some(thread) = self.agent_transcript_backup_thread.take() {
                self.record_agent_transcript_backup_pass(thread.join());
            }
        }
    }

    fn record_agent_transcript_backup_pass(
        &mut self,
        pass: std::thread::Result<AgentTranscriptBackupPass>,
    ) {
        match pass {
            Ok(pass) => self.agent_transcript_backup_last = Some(pass),
            Err(_) => tracing::warn!(
                event = "agent.transcript.backup.pass",
                outcome = "error",
                "native agent transcript backup thread panicked"
            ),
        }
    }

    /// The backup schedule for `agent.transcripts`: whether passes run, the
    /// most recent finished pass, and the time until the next one.
    pub(crate) fn agent_transcript_backup_schedule(&mut self) -> AgentTranscriptBackupSchedule {
        self.reap_finished_agent_transcript_backup();
        let enabled = self.agent_transcript_backups_enabled();
        AgentTranscriptBackupSchedule {
            enabled,
            last_pass: self.agent_transcript_backup_last,
            next_pass_in: enabled
                .then_some(self.agent_transcript_backup_deadline)
                .flatten()
                .map(|deadline| deadline.saturating_duration_since(Instant::now())),
        }
    }

    /// The periodic pass: copy every pane's native transcript on a
    /// background thread so file I/O never stalls the app loop. Queued
    /// sessions ride along.
    pub(crate) fn sync_agent_transcript_backups(&mut self) {
        self.agent_transcript_backup_deadline = self
            .policy
            .persist_session
            .then_some(Instant::now() + AGENT_TRANSCRIPT_BACKUP_INTERVAL);
        if !self.agent_transcript_backups_enabled() {
            self.agent_transcript_backup_pending.clear();
            return;
        }
        self.reap_finished_agent_transcript_backup();
        if self.agent_transcript_backup_thread.is_some() {
            // The previous pass is still copying; retry shortly so queued
            // sessions do not wait a whole interval.
            tracing::debug!(
                event = "agent.transcript.backup.pass",
                outcome = "skipped",
                pending = self.agent_transcript_backup_pending.len(),
                "previous native agent transcript backup pass is still running"
            );
            self.agent_transcript_backup_deadline =
                Some(Instant::now() + AGENT_TRANSCRIPT_BACKUP_RETRY);
            return;
        }
        let requests = self.take_agent_transcript_backup_requests();
        if requests.is_empty() {
            return;
        }
        let store_dir = agent_transcripts::store_dir();
        let thread_store_dir = store_dir.clone();
        // Shared so the inline fallback copies the same list, queue included.
        let requests = std::sync::Arc::new(requests);
        let thread_requests = std::sync::Arc::clone(&requests);
        match std::thread::Builder::new()
            .name("herdr-agent-transcripts".into())
            .spawn(move || run_backup_pass(&thread_store_dir, &thread_requests))
        {
            Ok(thread) => self.agent_transcript_backup_thread = Some(thread),
            Err(err) => {
                tracing::warn!(
                    err = %err,
                    "failed to spawn agent transcript backup thread; copying inline"
                );
                self.agent_transcript_backup_last = Some(run_backup_pass(&store_dir, &requests));
            }
        }
    }

    /// Queue one pane's session for the next backup pass and bring that
    /// pass forward (used when an agent is suspended, whose session must
    /// survive even if Herdr stops soon; the shutdown pass drains the queue
    /// too). The copy itself stays off the app loop.
    pub(crate) fn queue_agent_transcript_backup(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        session: PersistedAgentSession,
    ) {
        if !self.agent_transcript_backups_enabled() {
            return;
        }
        let Some((tab, terminal_id)) = self.state.workspaces.get(ws_idx).and_then(|workspace| {
            workspace
                .tabs
                .iter()
                .find(|tab| tab.panes.contains_key(&pane_id))
                .and_then(|tab| tab.terminal_id(pane_id).map(|id| (tab, id)))
        }) else {
            return;
        };
        let request = self.agent_transcript_backup_request_for(tab, terminal_id, session);
        self.agent_transcript_backup_pending
            .insert(request_key(&request), request);
        self.agent_transcript_backup_deadline = Some(Instant::now());
    }

    /// The shutdown pass: wait for a running background pass, then copy
    /// everything inline so nothing is lost when the process exits.
    pub(crate) fn backup_agent_transcripts_on_shutdown(&mut self) {
        if let Some(thread) = self.agent_transcript_backup_thread.take() {
            self.record_agent_transcript_backup_pass(thread.join());
        }
        if !self.agent_transcript_backups_enabled() {
            self.agent_transcript_backup_pending.clear();
            return;
        }
        let requests = self.take_agent_transcript_backup_requests();
        if requests.is_empty() {
            return;
        }
        self.agent_transcript_backup_last =
            Some(run_backup_pass(&agent_transcripts::store_dir(), &requests));
    }

    /// Put a backed-up transcript back before the native resume command
    /// runs, when the agent has deleted its own. Existing native files are
    /// never touched, and a failure only logs: the resume still runs.
    pub(crate) fn restore_agent_transcript_before_resume(&self, session: &PersistedAgentSession) {
        if !self.policy.persist_session {
            return;
        }
        if let Err(err) =
            agent_transcripts::restore_if_missing(session, &agent_transcripts::store_dir())
        {
            tracing::warn!(
                event = "agent.transcript.restore",
                outcome = "error",
                agent = %session.agent,
                session_id = %session.session_ref.value,
                err = %err,
                "failed to restore native agent transcript from backup"
            );
        }
    }
}

fn request_key(request: &TranscriptBackupRequest) -> String {
    crate::agent_resume::dedupe_key(
        &request.session.source,
        &request.session.agent,
        &request.session.session_ref,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::path::{Path, PathBuf};

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn claude_session(id: &str) -> PersistedAgentSession {
        PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id(id).unwrap(),
            transcript_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
        }
    }

    /// A private home and config directory for tests that run a real pass,
    /// so neither the user's transcripts nor their store are touched.
    /// nextest runs each test in its own process, so the environment is
    /// not shared.
    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-app-transcripts-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::env::set_var("HOME", root.join("home"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            Self { root }
        }

        /// A native Claude transcript for `id`, with the session naming it.
        fn native_session(&self, id: &str) -> PersistedAgentSession {
            let project = self
                .root
                .join("home")
                .join(".claude")
                .join("projects")
                .join("-tmp-project");
            std::fs::create_dir_all(&project).unwrap();
            let file = project.join(format!("{id}.jsonl"));
            std::fs::write(&file, "{\"type\":\"user\"}\n").unwrap();
            PersistedAgentSession {
                transcript_path: Some(file),
                ..claude_session(id)
            }
        }

        fn backup_of(&self, id: &str) -> Option<PathBuf> {
            fn find(dir: &Path, id: &str) -> Option<PathBuf> {
                for entry in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
                    let path = entry.path();
                    if path.is_dir() {
                        if path.file_name().is_some_and(|name| name == id)
                            && path.join("transcript.jsonl").is_file()
                        {
                            return Some(path.join("transcript.jsonl"));
                        }
                        if let Some(found) = find(&path, id) {
                            return Some(found);
                        }
                    }
                }
                None
            }
            find(&self.root.join("config"), id)
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn app_with_agent_pane(session: PersistedAgentSession) -> (App, crate::layout::PaneId) {
        let mut app = test_app();
        let workspace = Workspace::test_new("transcripts");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(session);
        app.policy.persist_session = true;
        app.backup_agent_transcripts = true;
        (app, pane)
    }

    #[test]
    fn backup_requests_cover_open_and_suspended_panes_with_pane_context() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("transcripts");
        workspace.tabs[0].custom_name = Some("review".into());
        let live_pane = workspace.tabs[0].root_pane;
        let parked_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let live_terminal = workspace.terminal_id(live_pane).unwrap().clone();
        let parked_terminal = workspace.terminal_id(parked_pane).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();

        let terminal = app.state.terminals.get_mut(&live_terminal).unwrap();
        terminal.cwd = PathBuf::from("/tmp/live");
        terminal.set_persisted_agent_session(claude_session("live-session"));
        let terminal = app.state.terminals.get_mut(&parked_terminal).unwrap();
        terminal.cwd = PathBuf::from("/tmp/parked");
        terminal.restore_suspended_agent(
            "claude".into(),
            Some("reviewer".into()),
            claude_session("parked-session"),
        );

        let mut requests = app.agent_transcript_backup_requests();
        requests.sort_by(|a, b| {
            a.session
                .session_ref
                .value
                .cmp(&b.session.session_ref.value)
        });
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].session.session_ref.value, "live-session");
        assert_eq!(
            requests[0].session.transcript_path.as_deref(),
            Some(Path::new("/tmp/live-session.jsonl"))
        );
        assert_eq!(requests[0].cwd.as_deref(), Some(Path::new("/tmp/live")));
        assert_eq!(requests[0].label.as_deref(), Some("review"));
        assert_eq!(requests[1].session.session_ref.value, "parked-session");
        assert_eq!(requests[1].cwd.as_deref(), Some(Path::new("/tmp/parked")));
    }

    #[test]
    fn backup_requests_dedupe_sessions_shared_by_panes_and_skip_plain_shells() {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("transcripts");
        let first = workspace.tabs[0].root_pane;
        let second = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let plain = workspace.test_split(ratatui::layout::Direction::Vertical);
        let first_terminal = workspace.terminal_id(first).unwrap().clone();
        let second_terminal = workspace.terminal_id(second).unwrap().clone();
        let plain_terminal = workspace.terminal_id(plain).unwrap().clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for terminal_id in [&first_terminal, &second_terminal] {
            app.state
                .terminals
                .get_mut(terminal_id)
                .unwrap()
                .set_persisted_agent_session(claude_session("shared"));
        }
        assert!(app.state.terminals[&plain_terminal]
            .persistable_agent_session()
            .is_none());

        let requests = app.agent_transcript_backup_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].session.session_ref.value, "shared");
    }

    #[test]
    fn periodic_pass_is_inert_without_session_persistence() {
        let mut app = test_app();
        assert!(app.agent_transcript_backup_deadline.is_none());
        assert!(!app.agent_transcript_backup_due(Instant::now()));
        app.sync_agent_transcript_backups();
        assert!(app.agent_transcript_backup_deadline.is_none());
        assert!(app.agent_transcript_backup_thread.is_none());
    }

    #[test]
    fn periodic_pass_reschedules_and_honours_the_config_flag() {
        let mut app = test_app();
        app.policy.persist_session = true;
        app.backup_agent_transcripts = false;
        let before = Instant::now();
        app.sync_agent_transcript_backups();
        let deadline = app
            .agent_transcript_backup_deadline
            .expect("next pass is scheduled");
        assert!(deadline >= before + AGENT_TRANSCRIPT_BACKUP_INTERVAL);
        assert!(app.agent_transcript_backup_thread.is_none());
    }

    #[test]
    fn queued_session_brings_the_next_pass_forward_and_rides_along() {
        let sandbox = Sandbox::new("queue");
        let session = sandbox.native_session("queued-session");
        let (mut app, pane) = app_with_agent_pane(session.clone());
        assert!(!app.agent_transcript_backup_due(Instant::now()));

        app.queue_agent_transcript_backup(0, pane, session.clone());
        app.queue_agent_transcript_backup(0, pane, session.clone());
        assert_eq!(app.agent_transcript_backup_pending.len(), 1);
        assert!(app.agent_transcript_backup_due(Instant::now()));
        assert!(sandbox.backup_of("queued-session").is_none());

        // The pass runs off the loop and drains the queue; a closed pane's
        // queued session is still copied.
        app.state.workspaces.clear();
        app.sync_agent_transcript_backups();
        assert!(app.agent_transcript_backup_pending.is_empty());
        let thread = app
            .agent_transcript_backup_thread
            .take()
            .expect("pass runs on a thread");
        thread.join().unwrap();
        let backup = sandbox.backup_of("queued-session").expect("backup written");
        assert_eq!(
            std::fs::read_to_string(backup).unwrap(),
            "{\"type\":\"user\"}\n"
        );
        assert!(!app.agent_transcript_backup_due(Instant::now()));
    }

    #[test]
    fn queue_is_inert_when_backups_are_off_and_cleared_by_a_disabled_pass() {
        let (mut app, pane) = app_with_agent_pane(claude_session("off"));
        app.backup_agent_transcripts = false;
        app.agent_transcript_backup_deadline = None;
        app.queue_agent_transcript_backup(0, pane, claude_session("off"));
        assert!(app.agent_transcript_backup_pending.is_empty());
        assert!(app.agent_transcript_backup_deadline.is_none());

        app.backup_agent_transcripts = true;
        app.queue_agent_transcript_backup(0, pane, claude_session("off"));
        assert_eq!(app.agent_transcript_backup_pending.len(), 1);
        app.backup_agent_transcripts = false;
        app.sync_agent_transcript_backups();
        assert!(app.agent_transcript_backup_pending.is_empty());
        assert!(app.agent_transcript_backup_thread.is_none());
    }

    #[test]
    fn a_pass_behind_a_running_one_retries_soon_and_keeps_the_queue() {
        let (mut app, pane) = app_with_agent_pane(claude_session("busy"));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        app.agent_transcript_backup_thread = Some(std::thread::spawn(move || {
            let _ = release_rx.recv();
            AgentTranscriptBackupPass {
                finished: SystemTime::now(),
                duration: Duration::ZERO,
                summary: BackupSummary::default(),
            }
        }));
        app.queue_agent_transcript_backup(0, pane, claude_session("busy"));
        let before = Instant::now();
        app.sync_agent_transcript_backups();
        assert_eq!(app.agent_transcript_backup_pending.len(), 1);
        let deadline = app
            .agent_transcript_backup_deadline
            .expect("retry scheduled");
        assert!(deadline <= before + AGENT_TRANSCRIPT_BACKUP_RETRY + Duration::from_secs(1));
        assert!(deadline < before + AGENT_TRANSCRIPT_BACKUP_INTERVAL);
        release_tx.send(()).unwrap();
        app.agent_transcript_backup_thread
            .take()
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn shutdown_pass_drains_the_queue_even_when_the_snapshot_is_current() {
        let sandbox = Sandbox::new("shutdown");
        let session = sandbox.native_session("shutdown-session");
        let (mut app, pane) = app_with_agent_pane(session.clone());
        app.queue_agent_transcript_backup(0, pane, session);
        // The early-skip branch of the shutdown save: nothing to snapshot.
        app.pane_exit_checkpoint_pending = true;
        app.state.session_dirty = false;

        app.save_session_on_shutdown();
        assert!(app.agent_transcript_backup_pending.is_empty());
        assert!(app.agent_transcript_backup_thread.is_none());
        assert!(app.session_save_deadline.is_none());
        assert!(sandbox.backup_of("shutdown-session").is_some());
    }

    fn transcripts_status(app: &mut App) -> serde_json::Value {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::AgentTranscripts(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn transcripts_status_reports_the_store_and_the_last_pass() {
        let sandbox = Sandbox::new("status");
        let session = sandbox.native_session("status-session");
        let (mut app, _pane) = app_with_agent_pane(session);

        // Before any pass: an empty store, a scheduled pass, no history.
        let idle = transcripts_status(&mut app);
        assert_eq!(idle["result"]["type"], "agent_transcripts");
        assert_eq!(idle["result"]["enabled"], true);
        assert_eq!(idle["result"]["sessions"], 0);
        assert!(idle["result"].get("last_pass").is_none());
        assert!(idle["result"]["store_dir"]
            .as_str()
            .unwrap()
            .ends_with(agent_transcripts::STORE_DIR_NAME));

        // A finished background pass is picked up when the status is read.
        app.sync_agent_transcript_backups();
        while app
            .agent_transcript_backup_thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let after = transcripts_status(&mut app);
        assert!(app.agent_transcript_backup_thread.is_none());
        assert_eq!(after["result"]["sessions"], 1);
        assert_eq!(after["result"]["native_missing"], 0);
        assert_eq!(
            after["result"]["transcript_bytes"],
            "{\"type\":\"user\"}\n".len() as u64
        );
        assert!(after["result"]["disk_bytes"].as_u64().unwrap() > 0);
        assert_eq!(after["result"]["last_pass"]["updated"], 1);
        assert_eq!(after["result"]["last_pass"]["failed"], 0);
        assert!(
            after["result"]["last_pass"]["finished_unix"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            after["result"]["next_pass_in_ms"].as_u64().unwrap()
                <= AGENT_TRANSCRIPT_BACKUP_INTERVAL.as_millis() as u64
        );

        // Backups off: no schedule, the store is still reported.
        app.backup_agent_transcripts = false;
        let off = transcripts_status(&mut app);
        assert_eq!(off["result"]["enabled"], false);
        assert!(off["result"].get("next_pass_in_ms").is_none());
        assert_eq!(off["result"]["sessions"], 1);
    }
}
