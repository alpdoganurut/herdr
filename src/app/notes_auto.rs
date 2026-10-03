//! Automatic checkpoints (fork, `[notes] auto_checkpoints`): the app side
//! of `crate::notes::auto`. Fed from `emit_pane_state_update` (status edges
//! only, never a render path); git reads go to the notes worker and come
//! back as `NotesWorkerResult::GitHead`.

use std::time::Instant;

use super::App;
use crate::agents_model::turn::{EdgeStatus, TurnEdge};
use crate::app::actions::PaneStateUpdate;
use crate::notes::auto::{EdgeFacts, GitProbe};
use crate::notes::worker::NotesJob;

impl App {
    /// One pane state update's automatic checkpoints. O(panes in the tab)
    /// for the notes key, O(1) otherwise; returns at once when the feature
    /// is off or nothing about the agent's status changed.
    pub(crate) fn note_auto_checkpoints(&mut self, update: &PaneStateUpdate, turn: TurnEdge) {
        if !self.notes.enabled || !self.notes.auto {
            return;
        }
        if update.previous_state == update.state && !update.agent_released {
            return;
        }
        let agent = update
            .agent_label
            .clone()
            .or_else(|| update.previous_agent_label.clone());
        if agent.is_none() {
            return;
        }
        let Some(pane) = self.public_pane_id(update.ws_idx, update.pane_id) else {
            return;
        };
        let key = if update.agent_released {
            None
        } else {
            self.auto_pane_key(update.ws_idx, update.pane_id)
        };
        let cwd = match turn {
            TurnEdge::Started { .. } => self
                .model_terminal(update.ws_idx, update.pane_id)
                .map(|terminal| terminal.cwd.clone()),
            _ => None,
        };
        let released_mid_turn = released_mid_turn(update);
        let facts = EdgeFacts {
            previous: EdgeStatus::from(update.previous_state),
            next: EdgeStatus::from(update.state),
            turn,
            released: update.agent_released,
            released_mid_turn,
            parked: update.suspended || update.previous_suspended,
            key,
            cwd,
            agent,
            now_unix: crate::notes::now_unix(),
        };
        // Only a user turn's bookmark needs the local time.
        let hhmm = match turn {
            TurnEdge::Started { user: true } => super::notes::stamp_now(),
            _ => String::new(),
        };
        let actions = self
            .notes
            .auto_tracker
            .on_edge(&pane, facts, &hhmm, Instant::now());
        for checkpoint in actions.checkpoints {
            self.add_auto_checkpoint(checkpoint, Some((update.ws_idx, update.pane_id)));
        }
        if let Some(probe) = actions.probe {
            // A full queue skips this turn's commit check.
            self.submit_notes_job(NotesJob::GitHead(probe));
        }
    }

    /// A HEAD read came back from the notes worker.
    pub(super) fn handle_auto_git(
        &mut self,
        probe: GitProbe,
        head: Option<String>,
        commits: Vec<String>,
    ) {
        if !self.notes.enabled || !self.notes.auto {
            return;
        }
        let Some(checkpoint) = self.notes.auto_tracker.on_git(&probe, head, &commits) else {
            return;
        };
        let pane = self.parse_pane_id(&probe.pane);
        self.add_auto_checkpoint(checkpoint, pane);
    }
}

/// Whether a released agent went while it was working or blocked. The
/// release update carries the status from before the release in
/// `previous_state`; `state` and `agent_release_status` already describe the
/// pane after it (a working agent's exit reads `Idle` / `Done`).
fn released_mid_turn(update: &PaneStateUpdate) -> bool {
    update.agent_released
        && matches!(
            EdgeStatus::from(update.previous_state),
            EdgeStatus::Working | EdgeStatus::Blocked
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_model::turn::TurnEdge;
    use crate::api::schema::notes::{CheckpointKind, NotesAuthor};
    use crate::api::schema::AgentStatus;
    use crate::notes::auto::AutoCheckpoint;

    /// A test app (its notes directory is a fresh temp directory) and a
    /// guard that removes that directory.
    fn test_app() -> (App, DirGuard) {
        let app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        let dir = app
            .notes
            .checkpoints
            .path("x")
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        (app, DirGuard(dir))
    }

    struct DirGuard(std::path::PathBuf);

    impl Drop for DirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn list(app: &mut App, key: &str) -> Vec<crate::api::schema::notes::CheckpointInfo> {
        app.notes
            .checkpoints
            .list(key, &[], None, None)
            .unwrap()
            .checkpoints
    }

    #[test]
    fn automatic_checkpoints_are_tagged_and_the_api_cannot_forge_the_tag() {
        let (mut app, _dir) = test_app();
        app.add_auto_checkpoint(
            AutoCheckpoint {
                key: "claude-s1".into(),
                kind: CheckpointKind::Milestone,
                title: "commit: ship it".into(),
                detail: Some("abc ship it".into()),
            },
            None,
        );
        let all = list(&mut app, "claude-s1");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].tags, [crate::notes::recall::AUTO_TAG]);
        assert_eq!(all[0].author, NotesAuthor::Agent);
        assert!(crate::notes::recall::is_auto(&all[0]));
        // an agent's add through the API never carries the mark
        let request = crate::api::schema::notes::CheckpointsAddParams {
            target: crate::api::schema::notes::NotesTarget {
                key: Some("claude-s1".into()),
                ..Default::default()
            },
            kind: CheckpointKind::Note,
            title: "mine".into(),
            detail: None,
            tags: vec!["auto".into(), "x".into()],
            author: NotesAuthor::Agent,
        };
        let _ = app.handle_checkpoints_add("t".into(), request);
        let all = list(&mut app, "claude-s1");
        let mine = all.iter().find(|cp| cp.title == "mine").unwrap().clone();
        assert_eq!(mine.tags, ["x"]);
        // nor through an edit; and an edit never drops herdr's mark
        let auto_id = all
            .iter()
            .find(|cp| cp.title == "commit: ship it")
            .unwrap()
            .id
            .clone();
        let target = crate::api::schema::notes::NotesTarget {
            key: Some("claude-s1".into()),
            ..Default::default()
        };
        for (id, tags) in [(&mine.id, vec!["auto".to_string()]), (&auto_id, vec![])] {
            let _ = app.handle_checkpoints_update(
                "t".into(),
                crate::api::schema::notes::CheckpointsUpdateParams {
                    target: target.clone(),
                    id: id.clone(),
                    kind: None,
                    title: None,
                    detail: None,
                    tags: Some(tags),
                },
            );
        }
        let all = list(&mut app, "claude-s1");
        let tags_of = |id: &str| all.iter().find(|cp| cp.id == id).unwrap().tags.clone();
        assert!(tags_of(&mine.id).is_empty());
        assert_eq!(tags_of(&auto_id), [crate::notes::recall::AUTO_TAG]);
    }

    fn presentation() -> crate::terminal::EffectivePresentation {
        crate::terminal::EffectivePresentation {
            title: None,
            display_agent: None,
            state_labels: Default::default(),
        }
    }

    #[test]
    fn the_switch_and_a_disabled_store_stop_everything() {
        let (mut app, _dir) = test_app();
        app.notes.auto = false;
        let update = PaneStateUpdate {
            pane_id: crate::layout::PaneId::from_raw(1),
            ws_idx: 0,
            previous_agent_label: Some("claude".into()),
            previous_known_agent: None,
            previous_state: crate::detect::AgentState::Working,
            previous_seen: false,
            previous_presentation: presentation(),
            agent_label: None,
            known_agent: None,
            state: crate::detect::AgentState::Idle,
            seen: false,
            presentation: presentation(),
            agent_name_changed: false,
            agent_released: true,
            agent_release_status: Some(AgentStatus::Done),
            previous_suspended: false,
            suspended: false,
            suppress_completion: false,
        };
        app.note_auto_checkpoints(&update, TurnEdge::Ended);
        assert_eq!(app.notes.auto_tracker.len(), 0);
        app.notes.auto = true;
        app.notes.enabled = false;
        app.note_auto_checkpoints(&update, TurnEdge::Ended);
        assert_eq!(app.notes.auto_tracker.len(), 0);
        // on, but the pane does not exist: nothing tracked either
        app.notes.enabled = true;
        app.note_auto_checkpoints(&update, TurnEdge::Ended);
        assert_eq!(app.notes.auto_tracker.len(), 0);
    }

    #[test]
    fn a_commit_read_for_a_known_turn_adds_the_milestone() {
        let (mut app, _dir) = test_app();
        let now = Instant::now();
        let start = app
            .notes
            .auto_tracker
            .on_edge(
                "w9:p9",
                EdgeFacts {
                    previous: EdgeStatus::Idle,
                    next: EdgeStatus::Working,
                    turn: TurnEdge::Started { user: false },
                    released: false,
                    released_mid_turn: false,
                    parked: false,
                    key: Some("codex-t1".into()),
                    cwd: Some(std::env::temp_dir()),
                    agent: Some("codex".into()),
                    now_unix: 1,
                },
                "10:00",
                now,
            )
            .probe
            .unwrap();
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        app.handle_auto_git(start, Some(a.clone()), Vec::new());
        let end = app
            .notes
            .auto_tracker
            .on_edge(
                "w9:p9",
                EdgeFacts {
                    previous: EdgeStatus::Working,
                    next: EdgeStatus::Idle,
                    turn: TurnEdge::Ended,
                    released: false,
                    released_mid_turn: false,
                    parked: false,
                    key: Some("codex-t1".into()),
                    cwd: None,
                    agent: Some("codex".into()),
                    now_unix: 2,
                },
                "10:05",
                now,
            )
            .probe
            .unwrap();
        assert_eq!(end.since.as_deref(), Some(a.as_str()));
        app.handle_auto_git(end, Some(b), vec!["bbbbbbb\tfix it".into()]);
        let all = list(&mut app, "codex-t1");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, CheckpointKind::Milestone);
        assert_eq!(all[0].title, "commit: fix it");
    }

    #[test]
    fn an_agent_exiting_while_working_is_released_mid_turn() {
        let (mut app, _dir) = test_app();
        let workspace = crate::workspace::Workspace::test_new("auto");
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let terminal_id = app
            .state
            .terminal_id_for_pane(0, pane_id)
            .expect("root pane terminal");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Working,
            );
        // the real release path: after it the pane reads idle / done
        let update = app
            .state
            .publish_pane_process_exit_if_agent(pane_id, false)
            .expect("process exit update");
        assert!(update.agent_released);
        // the post-release status never says working
        assert!(matches!(
            update.agent_release_status,
            Some(AgentStatus::Done | AgentStatus::Idle)
        ));
        assert!(released_mid_turn(&update));

        let idle = PaneStateUpdate {
            previous_state: crate::detect::AgentState::Idle,
            ..update.clone()
        };
        assert!(!released_mid_turn(&idle));
        let not_released = PaneStateUpdate {
            agent_released: false,
            ..update
        };
        assert!(!released_mid_turn(&not_released));
    }
}
