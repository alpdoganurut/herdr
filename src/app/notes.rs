//! The notes and checkpoints API handlers (fork): `notes.get|set|append`
//! and `checkpoints.list|add|update|remove|context`, and the notes worker's
//! results (`AppEvent::NotesWorkerFinished`).
//!
//! Every request names its notes with a [`NotesTarget`]: `key` beats
//! `pane_id` beats `tab_id`. A pane running an agent with a known session
//! resolves to that session's key, any other pane to its tab's key; a tab
//! resolves through its focused pane, then the first pane in layout order
//! with a session. Resolution is O(panes in the tab) except for a bare `key`,
//! which looks for a live pane holding it so its transcript can be read.
//!
//! None of these methods changes the UI or tab geometry, so a client's poll
//! never triggers a server render.

use std::path::PathBuf;
use std::time::Instant;

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind, PersistedAgentSession};
use crate::api::schema::notes::{
    error_code, CheckpointContextInfo, CheckpointContextSource, CheckpointKind, CheckpointTarget,
    CheckpointsAddParams, CheckpointsContextParams, CheckpointsListParams, CheckpointsUpdateParams,
    NotesAppendParams, NotesAuthor, NotesGetParams, NotesInfo, NotesSetParams, NotesTarget,
    NotesWriteInfo,
};
use crate::api::schema::ResponseResult;
use crate::notes::checkpoints::{AnchorRecord, CheckpointPatch, NewCheckpoint};
use crate::notes::transcript::Flavor;
use crate::notes::worker::{self, ContextJob, Locate, NotesJob, NotesWorkerResult};
use crate::notes::{NotesError, NotesRuntime, NotesSnapshot, WriteBy};

/// Panes remembered for `previous` before dead ones are pruned.
const PANE_KEYS_PRUNE_AT: usize = 256;

/// The runtime `App::new` starts with.
pub(super) fn runtime_for(config: &crate::config::NotesConfig) -> NotesRuntime {
    let mut runtime = NotesRuntime::new(
        config.enabled,
        default_notes_dir(),
        crate::persist::agent_transcripts::store_dir(),
    );
    runtime.auto = config.auto_checkpoints;
    runtime
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

fn flavor_of(agent: &str) -> Option<Flavor> {
    match agent {
        "claude" => Some(Flavor::Claude),
        "codex" => Some(Flavor::Codex),
        _ => None,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// The local `HH:MM`, else UTC.
pub(super) fn stamp_now() -> String {
    crate::notes::local_hhmm().unwrap_or_else(|| {
        let now = crate::notes::now_unix();
        format!("{:02}:{:02}", now / 3600 % 24, now / 60 % 60)
    })
}

/// `$CODEX_HOME`, else `~/.codex`.
fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            crate::integration::home_dir()
                .ok()
                .map(|home| home.join(".codex"))
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

    pub(super) fn handle_checkpoints_list(
        &mut self,
        id: String,
        params: CheckpointsListParams,
    ) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            let checkpoints = self
                .notes
                .checkpoints
                .list(&resolved.key, &params.kinds, params.since_seq, params.limit)
                .map_err(rejection)?;
            Ok(ResponseResult::CheckpointsList { checkpoints })
        });
        Self::notes_reply(id, result)
    }

    /// Queue a job on the notes worker, starting it on first use. `false`
    /// when the queue is full or the worker cannot start.
    pub(super) fn submit_notes_job(&mut self, job: NotesJob) -> bool {
        if self.notes.worker.is_none() {
            let tx = self.event_tx.clone();
            let sink: worker::ResultSink = Box::new(move |result: NotesWorkerResult| {
                let event = crate::events::AppEvent::NotesWorkerFinished(Box::new(result));
                if tx.blocking_send(event).is_err() {
                    tracing::debug!("notes worker: the app is gone; result dropped");
                }
            });
            match worker::NotesWorker::spawn(sink) {
                Ok(spawned) => self.notes.worker = Some(spawned),
                Err(err) => {
                    tracing::warn!(err = %err, "notes: cannot start the notes worker");
                    return false;
                }
            }
        }
        let Some(notes_worker) = self.notes.worker.as_ref() else {
            return false;
        };
        let queued = notes_worker.submit(job);
        if !queued {
            tracing::debug!("notes worker: queue full; the caller will ask again");
        }
        queued
    }

    /// The anchor of a checkpoint added now: captured here when the live
    /// transcript is known (a bounded tail read), else a job that locates
    /// it off the app thread. Tab notes get no anchor.
    fn checkpoint_anchor_now(
        &mut self,
        resolved: &ResolvedNotes,
    ) -> (Option<AnchorRecord>, Option<Locate>) {
        let Some(session) = resolved.session.as_ref() else {
            return (None, None);
        };
        match flavor_of(&session.agent) {
            Some(Flavor::Claude) => match session
                .transcript_path
                .as_deref()
                .filter(|path| path.is_file())
            {
                Some(path) => (
                    worker::anchor_at_end(path, Flavor::Claude)
                        .map(|anchor| AnchorRecord::from_anchor(&anchor)),
                    None,
                ),
                None => (None, Some(Locate::Claude(session.clone()))),
            },
            Some(Flavor::Codex) => {
                if session.session_ref.kind != AgentSessionRefKind::Id {
                    return (None, None);
                }
                let session_id = session.session_ref.value.clone();
                let memo_key = format!("codex:{session_id}");
                match self.notes.located(&memo_key, Instant::now()) {
                    Some(Some(path)) => (
                        worker::anchor_at_end(&path, Flavor::Codex)
                            .map(|anchor| AnchorRecord::from_anchor(&anchor)),
                        None,
                    ),
                    Some(None) => (None, None),
                    None => match codex_home() {
                        Some(codex_home) => (
                            None,
                            Some(Locate::Codex {
                                codex_home,
                                session_id,
                            }),
                        ),
                        None => (None, None),
                    },
                }
            }
            None => (None, None),
        }
    }

    pub(super) fn handle_checkpoints_add(
        &mut self,
        id: String,
        params: CheckpointsAddParams,
    ) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            if params.kind == CheckpointKind::Unknown {
                return Err((error_code::INVALID_PARAMS, "unknown checkpoint kind".into()));
            }
            // The `auto` tag is herdr's own mark (`[notes] auto_checkpoints`).
            let tags = params
                .tags
                .into_iter()
                .filter(|tag| tag != crate::notes::recall::AUTO_TAG)
                .collect();
            let checkpoint = self.add_checkpoint_now(
                &resolved,
                NewCheckpoint {
                    kind: params.kind,
                    author: author_or_agent(params.author),
                    title: params.title,
                    detail: params.detail,
                    tags,
                    anchor: None,
                },
            )?;
            Ok(ResponseResult::CheckpointWrite { checkpoint })
        });
        Self::notes_reply(id, result)
    }

    /// Add a checkpoint anchored at the transcript's current end (captured
    /// now when the live transcript is known, else located on the worker).
    fn add_checkpoint_now(
        &mut self,
        resolved: &ResolvedNotes,
        new: NewCheckpoint,
    ) -> Result<crate::api::schema::notes::CheckpointWriteInfo, Rejection> {
        let (anchor, locate) = self.checkpoint_anchor_now(resolved);
        let checkpoint = self
            .notes
            .checkpoints
            .add(&resolved.key, NewCheckpoint { anchor, ..new }, now_ms())
            .map_err(rejection)?;
        let needs_anchor = checkpoint
            .checkpoint
            .as_ref()
            .is_some_and(|info| !info.has_context);
        if let (Some(locate), true, Some(info)) =
            (locate, needs_anchor, checkpoint.checkpoint.as_ref())
        {
            // A full queue leaves the checkpoint without context.
            self.submit_notes_job(NotesJob::Locate {
                locate,
                anchor_for: Some((resolved.key.clone(), info.id.clone())),
            });
        }
        Ok(checkpoint)
    }

    /// The notes key of the pane's agent session (`None` without one),
    /// noted in the pane → key memory so a later `/clear` finds it as
    /// `previous`. O(panes in the tab).
    pub(super) fn auto_pane_key(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<String> {
        let resolved = self.resolve_pane_notes(ws_idx, pane_id)?;
        resolved.session.as_ref()?;
        if let Some(public) = resolved.pane_id.as_deref() {
            let _ = self.notes.panes.observe(public, &resolved.key);
        }
        Some(resolved.key)
    }

    /// Add one of herdr's automatic checkpoints: tagged `auto`, author
    /// agent, anchored like an agent's own when `pane` still holds the
    /// session. A failure (the hourly cap, I/O) is logged, never surfaced.
    pub(super) fn add_auto_checkpoint(
        &mut self,
        checkpoint: crate::notes::auto::AutoCheckpoint,
        pane: Option<(usize, crate::layout::PaneId)>,
    ) {
        let resolved = pane
            .and_then(|(ws_idx, pane_id)| self.resolve_pane_notes(ws_idx, pane_id))
            .filter(|resolved| resolved.key == checkpoint.key)
            .unwrap_or_else(|| ResolvedNotes {
                key: checkpoint.key.clone(),
                ..ResolvedNotes::default()
            });
        let new = NewCheckpoint {
            kind: checkpoint.kind,
            author: NotesAuthor::Agent,
            title: checkpoint.title,
            detail: checkpoint.detail,
            tags: vec![crate::notes::recall::AUTO_TAG.to_string()],
            anchor: None,
        };
        if let Err((code, message)) = self.add_checkpoint_now(&resolved, new) {
            tracing::debug!(
                key = %resolved.key,
                code,
                %message,
                "notes: automatic checkpoint skipped"
            );
        }
    }

    pub(super) fn handle_checkpoints_update(
        &mut self,
        id: String,
        params: CheckpointsUpdateParams,
    ) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            // The `auto` mark stays herdr's: never added by an edit, never
            // lost by one.
            let tags = match params.tags {
                Some(tags) => {
                    let was_auto = self
                        .notes
                        .checkpoints
                        .find(&resolved.key, &params.id)
                        .ok()
                        .flatten()
                        .is_some_and(|cp| {
                            cp.tags.iter().any(|t| t == crate::notes::recall::AUTO_TAG)
                        });
                    let mut tags: Vec<String> = tags
                        .into_iter()
                        .filter(|tag| tag != crate::notes::recall::AUTO_TAG)
                        .collect();
                    if was_auto {
                        tags.push(crate::notes::recall::AUTO_TAG.to_string());
                    }
                    Some(tags)
                }
                None => None,
            };
            let checkpoint = self
                .notes
                .checkpoints
                .update(
                    &resolved.key,
                    &params.id,
                    CheckpointPatch {
                        kind: params.kind,
                        title: params.title,
                        detail: params.detail,
                        tags,
                    },
                    crate::notes::now_unix(),
                )
                .map_err(rejection)?;
            Ok(ResponseResult::CheckpointWrite { checkpoint })
        });
        Self::notes_reply(id, result)
    }

    pub(super) fn handle_checkpoints_remove(
        &mut self,
        id: String,
        params: CheckpointTarget,
    ) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            let checkpoint = self
                .notes
                .checkpoints
                .remove(&resolved.key, &params.id, crate::notes::now_unix())
                .map_err(rejection)?;
            self.notes.forget_contexts(&resolved.key, &params.id);
            Ok(ResponseResult::CheckpointWrite { checkpoint })
        });
        Self::notes_reply(id, result)
    }

    pub(super) fn handle_checkpoints_context(
        &mut self,
        id: String,
        params: CheckpointsContextParams,
    ) -> String {
        let result = self.notes_enabled().and_then(|()| {
            let resolved = self.resolve_notes_target(&params.target)?;
            let checkpoint = self
                .notes
                .checkpoints
                .find(&resolved.key, &params.id)
                .map_err(rejection)?
                .ok_or_else(|| {
                    (
                        error_code::NOT_FOUND,
                        format!("no checkpoint {:?}", params.id),
                    )
                })?;
            let context = self.checkpoint_context(&resolved, &params, checkpoint.anchor);
            Ok(ResponseResult::CheckpointContext { context })
        });
        Self::notes_reply(id, result)
    }

    /// A cached context, else `pending` while the worker reads it.
    fn checkpoint_context(
        &mut self,
        resolved: &ResolvedNotes,
        params: &CheckpointsContextParams,
        anchor: Option<AnchorRecord>,
    ) -> CheckpointContextInfo {
        let reply = |source| CheckpointContextInfo {
            id: params.id.clone(),
            source,
            prompt: None,
            reply: None,
            prompt_ts: None,
            reply_ts: None,
            continued: false,
            truncated: false,
        };
        let flavor = resolved.agent().and_then(flavor_of);
        let (Some(flavor), Some(anchor)) = (flavor, anchor.filter(AnchorRecord::is_usable)) else {
            return reply(CheckpointContextSource::Unsupported);
        };
        let chars = params
            .chars
            .unwrap_or(worker::DEFAULT_CONTEXT_CHARS)
            .clamp(1, worker::MAX_CONTEXT_CHARS);
        let cache_key = NotesRuntime::context_key(&resolved.key, &params.id, chars);
        if let Some(cached) = self.notes.cached_context(&cache_key) {
            return cached;
        }
        let now = Instant::now();
        if self.notes.context_pending(&cache_key, now) {
            return reply(CheckpointContextSource::Pending);
        }
        let session_id = resolved.session_id().map(str::to_owned);
        let (native, locate) = match flavor {
            Flavor::Claude => {
                let session = resolved.session.clone().or_else(|| {
                    Some(PersistedAgentSession {
                        source: "herdr:claude".into(),
                        agent: "claude".into(),
                        session_ref: AgentSessionRef::id(session_id.clone()?)?,
                        transcript_path: None,
                    })
                });
                let native = session
                    .as_ref()
                    .and_then(|session| session.transcript_path.clone());
                let locate = match (&native, session) {
                    (None, Some(session)) => Some(Locate::Claude(session)),
                    _ => None,
                };
                (native, locate)
            }
            Flavor::Codex => match session_id.as_deref() {
                Some(session_id) => match self.notes.located(&format!("codex:{session_id}"), now) {
                    Some(path) => (path, None),
                    None => (
                        None,
                        codex_home().map(|codex_home| Locate::Codex {
                            codex_home,
                            session_id: session_id.to_owned(),
                        }),
                    ),
                },
                None => (None, None),
            },
        };
        let backup_dir = match (resolved.agent(), session_id.as_deref()) {
            (Some(agent), Some(session_id)) => crate::persist::agent_transcripts::backup_dir(
                &self.notes.store_dir,
                agent,
                session_id,
            ),
            _ => None,
        };
        let job = NotesJob::Context(ContextJob {
            key: resolved.key.clone(),
            id: params.id.clone(),
            chars,
            flavor,
            anchor: anchor.to_anchor(),
            native,
            locate,
            backup_dir,
        });
        if self.submit_notes_job(job) {
            self.notes.mark_context_pending(cache_key, now);
        }
        reply(CheckpointContextSource::Pending)
    }

    /// `AppEvent::NotesWorkerFinished`: fill the caches, and record an
    /// anchor captured for a checkpoint.
    pub(super) fn handle_notes_worker_finished(&mut self, result: NotesWorkerResult) {
        if let NotesWorkerResult::GitHead {
            probe,
            head,
            commits,
        } = result
        {
            self.handle_auto_git(probe, head, commits);
            return;
        }
        let Some((key, id, anchor)) = self.notes.apply_worker_result(result, Instant::now()) else {
            return;
        };
        match self.notes.checkpoints.set_anchor(
            &key,
            &id,
            AnchorRecord::from_anchor(&anchor),
            crate::notes::now_unix(),
        ) {
            Ok(_) => self.notes.forget_contexts(&key, &id),
            Err(err) => tracing::warn!(
                key,
                id,
                err = %err.message(),
                "notes: cannot record a checkpoint anchor"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::notes::{CheckpointsListParams, NotesWriteOutcome};
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
        let list = app.handle_checkpoints_list(
            "t".into(),
            CheckpointsListParams {
                target: tab(&tab_id),
                ..CheckpointsListParams::default()
            },
        );
        assert!(list.contains("notes_disabled"));
    }

    #[test]
    fn writes_and_checkpoints_round_trip_through_the_handlers() {
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

        let added = app.handle_checkpoints_add(
            "t".into(),
            CheckpointsAddParams {
                target: tab(&tab_id),
                kind: CheckpointKind::Bookmark,
                title: "look here".into(),
                detail: None,
                tags: Vec::new(),
                author: NotesAuthor::User,
            },
        );
        let added: serde_json::Value = serde_json::from_str(&added).unwrap();
        assert_eq!(added["result"]["type"], "checkpoint_write");
        let cp_id = added["result"]["checkpoint"]["checkpoint"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            added["result"]["checkpoint"]["checkpoint"]["has_context"],
            false
        );

        // Tab notes have no transcript.
        let context = app.handle_checkpoints_context(
            "t".into(),
            CheckpointsContextParams {
                target: tab(&tab_id),
                id: cp_id.clone(),
                chars: None,
            },
        );
        assert!(context.contains("\"source\":\"unsupported\""), "{context}");

        let removed = app.handle_checkpoints_remove(
            "t".into(),
            CheckpointTarget {
                target: tab(&tab_id),
                id: cp_id.clone(),
            },
        );
        assert!(removed.contains("\"removed\":true"));
        let missing = app.handle_checkpoints_update(
            "t".into(),
            CheckpointsUpdateParams {
                target: tab(&tab_id),
                id: cp_id,
                kind: None,
                title: Some("x".into()),
                detail: None,
                tags: None,
            },
        );
        assert!(missing.contains("not_found"));
    }

    #[test]
    fn checkpoint_context_is_pending_until_the_worker_result_is_applied() {
        let (mut app, _, root, _, _guard) = app_with_two_panes();
        let transcript = app.notes.store.notes_path("x").with_file_name("t.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"why\"}}\n",
        )
        .unwrap();
        set_session(&mut app, &root, claude("ctx", Some(transcript.clone())));

        let added = app.handle_checkpoints_add(
            "t".into(),
            CheckpointsAddParams {
                target: pane(&root),
                kind: CheckpointKind::Decision,
                title: "use jiff".into(),
                detail: None,
                tags: Vec::new(),
                author: NotesAuthor::Agent,
            },
        );
        let added: serde_json::Value = serde_json::from_str(&added).unwrap();
        let checkpoint = &added["result"]["checkpoint"]["checkpoint"];
        assert_eq!(checkpoint["has_context"], true, "{added}");
        let cp_id = checkpoint["id"].as_str().unwrap().to_owned();

        let params = CheckpointsContextParams {
            target: pane(&root),
            id: cp_id.clone(),
            chars: None,
        };
        let first = app.handle_checkpoints_context("t".into(), params.clone());
        assert!(first.contains("\"source\":\"pending\""), "{first}");
        // Asked again while queued: still pending, no second job.
        let again = app.handle_checkpoints_context("t".into(), params.clone());
        assert!(again.contains("\"source\":\"pending\""));

        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            match app.event_rx.try_recv() {
                Ok(crate::events::AppEvent::NotesWorkerFinished(result)) => break *result,
                Ok(_) => {}
                Err(_) => {
                    assert!(Instant::now() < deadline, "the worker never answered");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };
        app.handle_notes_worker_finished(result);
        let done = app.handle_checkpoints_context("t".into(), params);
        let done: serde_json::Value = serde_json::from_str(&done).unwrap();
        assert_eq!(done["result"]["type"], "checkpoint_context");
        assert_eq!(done["result"]["context"]["id"], cp_id.as_str());
        assert_ne!(done["result"]["context"]["source"], "pending");
    }
}
