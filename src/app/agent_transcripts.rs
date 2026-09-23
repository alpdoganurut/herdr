//! Keep copies of native agent transcripts for open and suspended panes.
//!
//! The copies live under the session directory (see
//! [`crate::persist::agent_transcripts`]). A periodic pass and the shutdown
//! save back up every pane's session; suspending an agent backs up that
//! session right away; a native resume first puts a copy back when the agent
//! has deleted its own transcript.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::App;
use crate::agent_resume::PersistedAgentSession;
use crate::persist::agent_transcripts::{self, TranscriptBackupRequest};

/// How often open and suspended agent transcripts are backed up.
pub(super) const AGENT_TRANSCRIPT_BACKUP_INTERVAL: Duration = Duration::from_secs(5 * 60);

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
                    let key = crate::agent_resume::dedupe_key(
                        &request.session.source,
                        &request.session.agent,
                        &request.session.session_ref,
                    );
                    if seen.insert(key) {
                        requests.push(request);
                    }
                }
            }
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
                let _ = thread.join();
            }
        }
    }

    /// The periodic pass: copy every pane's native transcript on a
    /// background thread so file I/O never stalls the app loop.
    pub(crate) fn sync_agent_transcript_backups(&mut self) {
        self.agent_transcript_backup_deadline = self
            .policy
            .persist_session
            .then_some(Instant::now() + AGENT_TRANSCRIPT_BACKUP_INTERVAL);
        if !self.agent_transcript_backups_enabled() {
            return;
        }
        self.reap_finished_agent_transcript_backup();
        if self.agent_transcript_backup_thread.is_some() {
            // The previous pass is still copying; the next tick retries.
            return;
        }
        let requests = self.agent_transcript_backup_requests();
        if requests.is_empty() {
            return;
        }
        let store_dir = agent_transcripts::store_dir();
        let thread_store_dir = store_dir.clone();
        match std::thread::Builder::new()
            .name("herdr-agent-transcripts".into())
            .spawn(move || {
                agent_transcripts::sync_backups(&thread_store_dir, &requests);
            }) {
            Ok(thread) => self.agent_transcript_backup_thread = Some(thread),
            Err(err) => {
                tracing::warn!(
                    err = %err,
                    "failed to spawn agent transcript backup thread; copying inline"
                );
                let requests = self.agent_transcript_backup_requests();
                agent_transcripts::sync_backups(&store_dir, &requests);
            }
        }
    }

    /// Back up one pane's session immediately (used when an agent is
    /// suspended, whose session must survive even if Herdr stops soon).
    pub(crate) fn backup_agent_transcript_now(
        &self,
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
        agent_transcripts::sync_backups(
            &agent_transcripts::store_dir(),
            std::slice::from_ref(&request),
        );
    }

    /// The shutdown pass: wait for a running background pass, then copy
    /// everything inline so nothing is lost when the process exits.
    pub(crate) fn backup_agent_transcripts_on_shutdown(&mut self) {
        if let Some(thread) = self.agent_transcript_backup_thread.take() {
            let _ = thread.join();
        }
        if !self.agent_transcript_backups_enabled() {
            return;
        }
        let requests = self.agent_transcript_backup_requests();
        if requests.is_empty() {
            return;
        }
        agent_transcripts::sync_backups(&agent_transcripts::store_dir(), &requests);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

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
            transcript_path: Some(std::path::PathBuf::from(format!("/tmp/{id}.jsonl"))),
        }
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
        terminal.cwd = std::path::PathBuf::from("/tmp/live");
        terminal.set_persisted_agent_session(claude_session("live-session"));
        let terminal = app.state.terminals.get_mut(&parked_terminal).unwrap();
        terminal.cwd = std::path::PathBuf::from("/tmp/parked");
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
            Some(std::path::Path::new("/tmp/live-session.jsonl"))
        );
        assert_eq!(
            requests[0].cwd.as_deref(),
            Some(std::path::Path::new("/tmp/live"))
        );
        assert_eq!(requests[0].label.as_deref(), Some("review"));
        assert_eq!(requests[1].session.session_ref.value, "parked-session");
        assert_eq!(
            requests[1].cwd.as_deref(),
            Some(std::path::Path::new("/tmp/parked"))
        );
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
}
