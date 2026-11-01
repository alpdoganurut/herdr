//! The notes API handlers (fork): `notes.get|set|append`. The
//! `checkpoints.*` methods stay advertised (their names and shapes are part
//! of the client-shell contract) but every one is refused with
//! `checkpoints_removed`: checkpoints are gone, the notes page is the
//! user's.
//!
//! Every request names its notes with a [`NotesTarget`]: `key` beats
//! `pane_id` beats `tab_id`. A pane running an agent with a known session
//! resolves to that session's key, any other pane to its tab's key; a tab
//! resolves through its focused pane, then the first pane in layout order
//! with a session. Resolution is O(panes in the tab) except for a bare `key`,
//! which looks for a live pane holding it.
//!
//! None of these methods changes the UI or tab geometry, so a client's poll
//! never triggers a server render.

use std::path::PathBuf;

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::agent_resume::{AgentSessionRefKind, PersistedAgentSession};
use crate::api::schema::notes::{
    error_code, NotesAppendParams, NotesAuthor, NotesGetParams, NotesInfo, NotesSetParams,
    NotesTarget, NotesWriteInfo,
};
use crate::api::schema::ResponseResult;
use crate::notes::{NotesError, NotesRuntime, NotesSnapshot, WriteBy};

/// Panes remembered for `previous` before dead ones are pruned.
const PANE_KEYS_PRUNE_AT: usize = 256;

/// The runtime `App::new` starts with.
pub(super) fn runtime_for(config: &crate::config::NotesConfig) -> NotesRuntime {
    NotesRuntime::new(config.enabled, default_notes_dir())
}

#[cfg(not(test))]
fn default_notes_dir() -> PathBuf {
    crate::notes::notes_dir()
}

/// Tests never write under the real config directory.
#[cfg(test)]
fn default_notes_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "herdr-app-notes-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// What a [`NotesTarget`] resolved to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedNotes {
    key: String,
    tab_id: Option<String>,
    pane_id: Option<String>,
    session: Option<PersistedAgentSession>,
    /// The tab's own key, when `key` is an agent session's key in a tab.
    tab_key: Option<String>,
    /// The key the pane used before its session changed.
    previous: Option<String>,
}

impl ResolvedNotes {
    fn agent(&self) -> Option<&str> {
        self.session
            .as_ref()
            .map(|session| session.agent.as_str())
            .or_else(|| crate::notes::key_agent_session(&self.key).map(|(agent, _)| agent))
    }

    /// The session id, when the session is named by id.
    fn session_id(&self) -> Option<&str> {
        match &self.session {
            Some(session) => (session.session_ref.kind == AgentSessionRefKind::Id)
                .then_some(session.session_ref.value.as_str()),
            None => crate::notes::key_agent_session(&self.key).map(|(_, id)| id),
        }
    }
}

type Rejection = (&'static str, String);

fn rejection(err: NotesError) -> Rejection {
    (err.code(), err.message())
}

/// The local `HH:MM`, else UTC.
pub(super) fn stamp_now() -> String {
    crate::notes::local_hhmm().unwrap_or_else(|| {
        let now = crate::notes::now_unix();
        format!("{:02}:{:02}", now / 3600 % 24, now / 60 % 60)
    })
}

fn author_or_agent(author: NotesAuthor) -> NotesAuthor {
    match author {
        NotesAuthor::Unknown => NotesAuthor::Agent,
        author => author,
    }
}

impl App {
    fn notes_reply(id: String, result: Result<ResponseResult, Rejection>) -> String {
        match result {
            Ok(result) => encode_success(id, result),
            Err((code, message)) => encode_error(id, code, message),
        }
    }

    fn notes_enabled(&self) -> Result<(), Rejection> {
        if self.notes.enabled {
            Ok(())
        } else {
            Err((
                error_code::DISABLED,
                "notes are turned off ([notes] enabled = false)".into(),
            ))
        }
    }

    /// The agent session a pane holds, if any.
    fn pane_agent_session(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PersistedAgentSession> {
        let pane = self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
        self.state
            .terminals
            .get(&pane.attached_terminal_id)?
            .persistable_agent_session()
    }

    /// One pane's notes: its session's key, else its tab's key.
    fn resolve_pane_notes(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<ResolvedNotes> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let tab_id = self.public_tab_id(ws_idx, tab_idx)?;
        let public_pane = self.public_pane_id(ws_idx, pane_id)?;
        let session = self.pane_agent_session(ws_idx, pane_id);
        let tab_key = crate::notes::tab_key(&tab_id);
        let (key, tab_key) = match &session {
            Some(session) => (
                crate::notes::session_key(&session.agent, &session.session_ref),
                Some(tab_key),
            ),
            None => (tab_key, None),
        };
        Some(ResolvedNotes {
            key,
            tab_id: Some(tab_id),
            pane_id: Some(public_pane),
            session,
            tab_key,
            previous: None,
        })
    }

    fn resolve_notes_target(&mut self, target: &NotesTarget) -> Result<ResolvedNotes, Rejection> {
        let mut resolved = if let Some(key) = target.key.as_deref() {
            if !crate::notes::is_valid_key(key) {
                return Err((
                    error_code::INVALID_PARAMS,
                    format!("invalid notes key {key:?}"),
                ));
            }
            self.find_live_notes_key(key)
                .unwrap_or_else(|| ResolvedNotes {
                    key: key.to_owned(),
                    ..ResolvedNotes::default()
                })
        } else if let Some(pane_id) = target.pane_id.as_deref() {
            let (ws_idx, pane) = self
                .parse_pane_id(pane_id)
                .ok_or_else(|| (error_code::NOT_FOUND, format!("pane {pane_id} not found")))?;
            self.resolve_pane_notes(ws_idx, pane)
                .ok_or_else(|| (error_code::NOT_FOUND, format!("pane {pane_id} not found")))?
        } else if let Some(tab_id) = target.tab_id.as_deref() {
            let (ws_idx, tab_idx) = self
                .parse_tab_id(tab_id)
                .ok_or_else(|| (error_code::NOT_FOUND, format!("tab {tab_id} not found")))?;
            let tab = &self.state.workspaces[ws_idx].tabs[tab_idx];
            let focused = tab.layout.focused();
            let pane = std::iter::once(focused)
                .chain(
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter(|pane| *pane != focused),
                )
                .find(|pane| self.pane_agent_session(ws_idx, *pane).is_some())
                .unwrap_or(focused);
            self.resolve_pane_notes(ws_idx, pane)
                .ok_or_else(|| (error_code::NOT_FOUND, format!("tab {tab_id} not found")))?
        } else {
            return Err((
                error_code::INVALID_PARAMS,
                "name the notes with key, pane_id or tab_id".into(),
            ));
        };
        if let Some(pane_id) = resolved.pane_id.clone() {
            resolved.previous = self.notes.panes.observe(&pane_id, &resolved.key);
            if self.notes.panes.len() > PANE_KEYS_PRUNE_AT {
                let live: std::collections::HashSet<String> = self
                    .terminal_targets()
                    .into_iter()
                    .filter_map(|target| self.public_pane_id(target.ws_idx, target.pane_id))
                    .collect();
                self.notes.panes.retain(|pane_id| live.contains(pane_id));
            }
        }
        Ok(resolved)
    }

    /// A live pane whose session has `key`.
    fn find_live_notes_key(&self, key: &str) -> Option<ResolvedNotes> {
        self.terminal_targets().into_iter().find_map(|target| {
            let session = self.pane_agent_session(target.ws_idx, target.pane_id)?;
            (crate::notes::session_key(&session.agent, &session.session_ref) == key)
                .then(|| self.resolve_pane_notes(target.ws_idx, target.pane_id))
                .flatten()
        })
    }

    /// Copy the tab's notes to the agent session's notes the first time the
    /// session's notes are touched. A pane whose earlier session had notes
    /// of its own (after `/clear`) keeps them apart: its notes report that
    /// key as `previous` instead.
    fn migrate_tab_notes(&mut self, resolved: &ResolvedNotes) {
        let Some(tab_key) = resolved.tab_key.as_deref() else {
            return;
        };
        if resolved
            .previous
            .as_deref()
            .is_some_and(|previous| previous != tab_key)
        {
            return;
        }
        if let Err(err) = self.notes.store.migrate(tab_key, &resolved.key) {
            tracing::warn!(
                from = tab_key,
                to = %resolved.key,
                err = %err.message(),
                "notes: cannot copy the tab's notes"
            );
        }
    }

    fn notes_info(resolved: &ResolvedNotes, snapshot: NotesSnapshot) -> NotesInfo {
        NotesInfo {
            key: resolved.key.clone(),
            path: snapshot.path.display().to_string(),
            revision: snapshot.revision,
            exists: snapshot.exists,
            unchanged: snapshot.text.is_none(),
            text: snapshot.text,
            bytes: snapshot.bytes,
            updated_at: snapshot.meta.updated_at,
            updated_by: snapshot.meta.updated_by,
            agent: resolved.agent().map(str::to_owned),
            session_id: resolved.session_id().map(str::to_owned),
            tab_id: resolved.tab_id.clone(),
            pane_id: resolved.pane_id.clone(),
            previous: resolved.previous.clone().or(snapshot.meta.previous),
        }
    }

    pub(super) fn handle_notes_get(&mut self, id: String, params: NotesGetParams) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            self.migrate_tab_notes(&resolved);
            let snapshot = self
                .notes
                .store
                .get(&resolved.key, params.known_revision.as_deref())
                .map_err(rejection)?;
            Ok(ResponseResult::NotesGet {
                notes: Self::notes_info(&resolved, snapshot),
            })
        });
        Self::notes_reply(id, result)
    }

    pub(super) fn handle_notes_set(&mut self, id: String, params: NotesSetParams) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            self.migrate_tab_notes(&resolved);
            let (outcome, snapshot) = self
                .notes
                .store
                .set(
                    &resolved.key,
                    &params.text,
                    params.base_revision.as_deref(),
                    WriteBy {
                        author: author_or_agent(params.author),
                        previous_key: resolved.previous.as_deref(),
                        now: crate::notes::now_unix(),
                    },
                )
                .map_err(rejection)?;
            Ok(ResponseResult::NotesWrite {
                write: NotesWriteInfo {
                    outcome,
                    notes: Self::notes_info(&resolved, snapshot),
                },
            })
        });
        Self::notes_reply(id, result)
    }

    pub(super) fn handle_notes_append(&mut self, id: String, params: NotesAppendParams) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            self.migrate_tab_notes(&resolved);
            let stamp = params.stamp.then(stamp_now);
            let (outcome, snapshot) = self
                .notes
                .store
                .append(
                    &resolved.key,
                    &params.text,
                    params.section.as_deref(),
                    stamp.as_deref(),
                    WriteBy {
                        author: author_or_agent(params.author),
                        previous_key: resolved.previous.as_deref(),
                        now: crate::notes::now_unix(),
                    },
                )
                .map_err(rejection)?;
            Ok(ResponseResult::NotesWrite {
                write: NotesWriteInfo {
                    outcome,
                    notes: Self::notes_info(&resolved, snapshot),
                },
            })
        });
        Self::notes_reply(id, result)
    }

    /// Every `checkpoints.*` method, `agents.checkpoint` and
    /// `agents.notes_append`: refused.
    /// The method names stay so an older client gets a clear answer
    /// instead of an unknown method.
    pub(super) fn handle_checkpoints_removed(id: String) -> String {
        encode_error(
            id,
            error_code::CHECKPOINTS_REMOVED,
            "checkpoints were removed from herdr+; the notes page (notes.get / notes.set) is the user's page",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_resume::AgentSessionRef;
    use crate::api::schema::notes::NotesWriteOutcome;
    use crate::api::schema::Request;
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

    /// Removes a test app's notes directory.
    struct NotesDirGuard(PathBuf);

    impl Drop for NotesDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An app with one tab of two panes (the root, then a split that is
    /// not focused); returns the app, the tab's and the panes' public ids
    /// and a guard that removes the notes directory.
    fn app_with_two_panes() -> (App, String, String, String, NotesDirGuard) {
        let mut app = test_app();
        let mut workspace = Workspace::test_new("notes");
        let root = workspace.tabs[0].root_pane;
        let split = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let root_id = app.public_pane_id(0, root).unwrap();
        let split_id = app.public_pane_id(0, split).unwrap();
        let tab_id = app.public_tab_id(0, 0).unwrap();
        let dir = app
            .notes
            .store
            .notes_path("x")
            .parent()
            .unwrap()
            .to_path_buf();
        (app, tab_id, root_id, split_id, NotesDirGuard(dir))
    }

    fn set_session(app: &mut App, public_pane: &str, session: PersistedAgentSession) {
        let (ws_idx, pane) = app.parse_pane_id(public_pane).unwrap();
        let terminal_id = app.state.workspaces[ws_idx]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(session);
    }

    fn claude(id: &str, transcript: Option<PathBuf>) -> PersistedAgentSession {
        PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: AgentSessionRef::id(id).unwrap(),
            transcript_path: transcript,
        }
    }

    fn get(app: &mut App, target: NotesTarget) -> serde_json::Value {
        let reply = app.handle_notes_get(
            "t".into(),
            NotesGetParams {
                target,
                known_revision: None,
            },
        );
        serde_json::from_str(&reply).unwrap()
    }

    fn tab(tab_id: &str) -> NotesTarget {
        NotesTarget {
            tab_id: Some(tab_id.into()),
            ..NotesTarget::default()
        }
    }

    fn pane(pane_id: &str) -> NotesTarget {
        NotesTarget {
            pane_id: Some(pane_id.into()),
            ..NotesTarget::default()
        }
    }

    #[test]
    fn no_notes_method_changes_the_ui() {
        let methods = crate::api::schema::notes::sample_methods();
        assert_eq!(methods.len(), crate::api::schema::notes::method::ALL.len());
        for method in methods {
            let name = crate::api::api_method_name(&method);
            let request = Request {
                id: "req".into(),
                method,
            };
            assert!(
                !crate::api::request_changes_ui(&request),
                "{name} must not trigger a UI recompute"
            );
        }
    }

    #[test]
    fn targets_resolve_by_precedence_and_an_empty_target_is_invalid() {
        let (mut app, tab_id, root, split, _guard) = app_with_two_panes();

        let empty = get(&mut app, NotesTarget::default());
        assert_eq!(empty["error"]["code"], "invalid_params");

        // A tab with no agent uses the tab key, through the focused pane.
        let by_tab = get(&mut app, tab(&tab_id));
        assert_eq!(by_tab["result"]["type"], "notes_get");
        assert_eq!(
            by_tab["result"]["notes"]["key"],
            crate::notes::tab_key(&tab_id)
        );
        assert_eq!(by_tab["result"]["notes"]["pane_id"], root.as_str());
        assert_eq!(by_tab["result"]["notes"]["exists"], false);

        // The first pane with a session wins when the focused one has none.
        set_session(&mut app, &split, claude("s-split", None));
        let by_tab = get(&mut app, tab(&tab_id));
        assert_eq!(by_tab["result"]["notes"]["key"], "claude-s-split");
        assert_eq!(by_tab["result"]["notes"]["agent"], "claude");
        assert_eq!(by_tab["result"]["notes"]["session_id"], "s-split");
        assert_eq!(by_tab["result"]["notes"]["pane_id"], split.as_str());

        // The focused pane's session beats it.
        set_session(&mut app, &root, claude("s-root", None));
        let by_tab = get(&mut app, tab(&tab_id));
        assert_eq!(by_tab["result"]["notes"]["key"], "claude-s-root");

        // pane_id beats tab_id, key beats both.
        let mixed = get(
            &mut app,
            NotesTarget {
                tab_id: Some(tab_id.clone()),
                pane_id: Some(split.clone()),
                key: None,
            },
        );
        assert_eq!(mixed["result"]["notes"]["key"], "claude-s-split");
        let keyed = get(
            &mut app,
            NotesTarget {
                tab_id: Some(tab_id.clone()),
                pane_id: Some(split.clone()),
                key: Some("claude-s-root".into()),
            },
        );
        assert_eq!(keyed["result"]["notes"]["key"], "claude-s-root");
        assert_eq!(keyed["result"]["notes"]["pane_id"], root.as_str());

        let offline = get(
            &mut app,
            NotesTarget {
                key: Some("tab-elsewhere".into()),
                ..NotesTarget::default()
            },
        );
        assert_eq!(offline["result"]["notes"]["key"], "tab-elsewhere");
        assert!(offline["result"]["notes"].get("pane_id").is_none());

        let bad = get(
            &mut app,
            NotesTarget {
                key: Some("../etc".into()),
                ..NotesTarget::default()
            },
        );
        assert_eq!(bad["error"]["code"], "invalid_params");
        assert_eq!(get(&mut app, pane("w9:p9"))["error"]["code"], "not_found");
        assert_eq!(get(&mut app, tab("w9:t9"))["error"]["code"], "not_found");
    }

    #[test]
    fn previous_is_reported_after_the_session_changes_and_tab_notes_migrate() {
        let (mut app, tab_id, root, _, _guard) = app_with_two_panes();
        let written = app.handle_notes_set(
            "t".into(),
            NotesSetParams {
                target: tab(&tab_id),
                text: "tab plan\n".into(),
                base_revision: None,
                author: NotesAuthor::User,
            },
        );
        let written: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(written["result"]["write"]["outcome"], "written");

        set_session(&mut app, &root, claude("first", None));
        let first = get(&mut app, pane(&root));
        assert_eq!(first["result"]["notes"]["key"], "claude-first");
        assert_eq!(first["result"]["notes"]["text"], "tab plan\n");
        assert_eq!(
            first["result"]["notes"]["previous"],
            crate::notes::tab_key(&tab_id)
        );

        set_session(&mut app, &root, claude("second", None));
        let second = get(&mut app, pane(&root));
        assert_eq!(second["result"]["notes"]["key"], "claude-second");
        assert_eq!(second["result"]["notes"]["previous"], "claude-first");
        assert_eq!(
            second["result"]["notes"]["exists"], false,
            "a new session after /clear does not copy the tab's notes again"
        );
    }

    #[test]
    fn disabled_notes_answer_notes_disabled() {
        let (mut app, tab_id, _, _, _guard) = app_with_two_panes();
        app.notes.enabled = false;
        assert_eq!(
            get(&mut app, tab(&tab_id))["error"]["code"],
            "notes_disabled"
        );
    }

    #[test]
    fn writes_round_trip_through_the_handlers() {
        let (mut app, tab_id, _, _, _guard) = app_with_two_panes();
        let appended = app.handle_notes_append(
            "t".into(),
            NotesAppendParams {
                target: tab(&tab_id),
                text: "found it".into(),
                section: Some("Findings".into()),
                stamp: true,
                author: NotesAuthor::Agent,
            },
        );
        let appended: serde_json::Value = serde_json::from_str(&appended).unwrap();
        let text = appended["result"]["write"]["notes"]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.starts_with("## Findings\n- "), "{text}");
        assert!(text.ends_with(" found it\n"));
        assert_eq!(
            appended["result"]["write"]["notes"]["revision"],
            crate::notes::revision_of(&text)
        );

        let stale = app.handle_notes_set(
            "t".into(),
            NotesSetParams {
                target: tab(&tab_id),
                text: "overwrite\n".into(),
                base_revision: Some("sha256:000000000000".into()),
                author: NotesAuthor::User,
            },
        );
        let stale: serde_json::Value = serde_json::from_str(&stale).unwrap();
        let outcome: NotesWriteOutcome =
            serde_json::from_value(stale["result"]["write"]["outcome"].clone()).unwrap();
        assert_eq!(outcome, NotesWriteOutcome::Conflict);
        assert_eq!(stale["result"]["write"]["notes"]["text"], text.as_str());
    }

    #[test]
    fn the_dispatcher_refuses_the_removed_agents_methods() {
        use crate::api::schema::agents_model::{AgentsCheckpointParams, AgentsNotesAppendParams};
        use crate::api::schema::Method;
        let (mut app, _, root, _, _guard) = app_with_two_panes();
        for method in [
            Method::AgentsCheckpoint(AgentsCheckpointParams {
                caller_pane: root.clone(),
                target: root.clone(),
                kind: "milestone".into(),
                title: "done".into(),
                ..AgentsCheckpointParams::default()
            }),
            Method::AgentsNotesAppend(AgentsNotesAppendParams {
                caller_pane: root.clone(),
                target: root.clone(),
                text: "- a log line".into(),
                ..AgentsNotesAppendParams::default()
            }),
            Method::CheckpointsList(crate::api::schema::notes::CheckpointsListParams::default()),
        ] {
            let name = crate::api::api_method_name(&method);
            let reply = app.handle_api_request(Request {
                id: "r".into(),
                method,
            });
            let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(
                reply["error"]["code"], "checkpoints_removed",
                "{name}: {reply}"
            );
        }
    }

    #[test]
    fn every_checkpoint_method_is_refused_as_removed() {
        let reply = App::handle_checkpoints_removed("t".into());
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["code"], "checkpoints_removed");
        assert!(reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains("checkpoints were removed"));
    }
}
