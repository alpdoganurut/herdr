//! The `managed.json` migration (fork, agents v2): roles and notes of the
//! v1 registry move onto the pane meta, idempotently.
//!
//! - `managed.json` is read only (never written, deleted or renamed): it
//!   stays the rollback source, and old MCP processes keep writing it until
//!   they are cycled.
//! - An entry matches a live terminal by its native session, else by pane
//!   and kind, but only when the entry is newer than the terminal (a reused
//!   pane id must not inherit a stale entry).
//! - Each entry's fingerprint is kept in `agents-v2.json`; only new or
//!   changed entries, and entries still unmatched, are applied. A meta field is written only when it is
//!   empty and was not cleared on purpose (the tombstone), so a re-run never
//!   brings a cleared value back. Migration writes the meta directly, never
//!   through the team rename path; a member's mirror follows the meta.
//! - Unmatched entries are surfaced: one notice card, and the list in
//!   `agents-v2.json` (`herdr coordinator status` reads it).
//!
//! It runs on the first check after start and again whenever `managed.json`
//! is newer than the marker's `source_mtime` (one `metadata()` per minute),
//! or while unmatched entries remain: those are retried every minute (an
//! agent restored at start is detected only after the first check); a retry
//! that matches nothing new writes nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::App;

/// How often the tick looks at `managed.json`'s mtime.
pub(crate) const MIGRATION_POLL: Duration = Duration::from_secs(60);
/// The marker file next to `managed.json`.
pub(crate) const MARKER_FILE: &str = "agents-v2.json";

/// Migration bookkeeping on the App.
#[derive(Debug, Default)]
pub(crate) struct MigrationState {
    /// The next mtime check.
    pub(crate) next_check: Option<Instant>,
}

/// `agents-v2.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MigrationMarker {
    #[serde(default)]
    pub(crate) migrated_unix: u64,
    /// `managed.json`'s mtime (seconds) at the last run.
    #[serde(default)]
    pub(crate) source_mtime: u64,
    /// Entry key → fingerprint of the applied entry.
    #[serde(default)]
    pub(crate) fingerprints: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) matched: u32,
    #[serde(default)]
    pub(crate) unmatched: Vec<UnmatchedEntry>,
}

/// An entry the migration could not place on a pane.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UnmatchedEntry {
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    #[serde(default)]
    pub(crate) reason: String,
}

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    pub(crate) applied: usize,
    pub(crate) skipped_unchanged: usize,
    pub(crate) unmatched: usize,
    /// Unmatched entries that were new or changed in this run.
    pub(crate) new_unmatched: usize,
}

pub(crate) fn marker_path(dir: &Path) -> PathBuf {
    dir.join(MARKER_FILE)
}

fn load_marker(dir: &Path) -> MigrationMarker {
    std::fs::read_to_string(marker_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_marker(dir: &Path, marker: &MigrationMarker) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(marker).map_err(std::io::Error::other)?;
    let path = marker_path(dir);
    let tmp = path.with_file_name(format!(".{MARKER_FILE}.tmp-{}", std::process::id()));
    let result = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn mtime_secs(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// An entry's stable key: its session, else its pane and kind.
fn entry_key(entry: &crate::coordinator::registry::ManagedAgent) -> String {
    match entry.session.as_deref().filter(|s| !s.is_empty()) {
        Some(session) => format!("session:{session}"),
        None => format!(
            "pane:{}:{}",
            entry.pane_id.as_deref().unwrap_or(""),
            entry.agent.as_deref().unwrap_or("")
        ),
    }
}

/// The fingerprint of what an entry would write.
fn fingerprint(entry: &crate::coordinator::registry::ManagedAgent) -> String {
    use sha2::{Digest, Sha256};
    let text = serde_json::json!([
        entry.role,
        entry.project,
        entry.note,
        entry.session,
        entry.pane_id,
        entry.agent,
    ])
    .to_string();
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// The note an entry carries over: its note, plus its project (when it is
/// not just the group's label) folded in as `project: X` (U3).
fn migrated_note(
    entry: &crate::coordinator::registry::ManagedAgent,
    group_label: &str,
) -> Option<String> {
    let note = entry
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty());
    let project = entry
        .project
        .as_deref()
        .map(str::trim)
        .filter(|project| !project.is_empty() && *project != group_label);
    match (note, project) {
        (Some(note), Some(project)) => Some(format!("{note} (project: {project})")),
        (Some(note), None) => Some(note.to_string()),
        (None, Some(project)) => Some(format!("project: {project}")),
        (None, None) => None,
    }
    .map(|note| super::agents_model::one_line(&note, super::agents_model::NOTE_MAX_CHARS))
}

fn entry_matches_unmatched(
    entry: &crate::coordinator::registry::ManagedAgent,
    previous: &UnmatchedEntry,
) -> bool {
    previous.session == entry.session && previous.pane == entry.pane_id
}

impl App {
    /// The scheduler's migration check: at most once a minute, one
    /// `metadata()` call; a run only when `managed.json` changed or
    /// unmatched entries remain.
    pub(crate) fn maybe_run_agents_migration(&mut self, now: Instant) -> bool {
        if self.agents_model.dir.is_none() {
            return false;
        }
        if self
            .agents_model
            .migration
            .next_check
            .is_some_and(|at| now < at)
        {
            return false;
        }
        self.agents_model.migration.next_check = Some(now + MIGRATION_POLL);
        self.run_agents_migration()
            .is_some_and(|report| report.applied > 0)
    }

    /// Run the migration when `managed.json` is newer than the marker, or
    /// retry its unmatched entries.
    /// `None` when there was nothing to do.
    pub(crate) fn run_agents_migration(&mut self) -> Option<MigrationReport> {
        let dir = self.agents_model.dir.clone()?;
        let source = crate::coordinator::registry_path(&dir);
        let source_mtime = mtime_secs(&source)?;
        let mut marker = load_marker(&dir);
        // Unmatched entries are tried again on every check (their agent may
        // not be detected yet, e.g. right after a server start), not only
        // when managed.json changes.
        let source_changed = marker.migrated_unix == 0 || source_mtime > marker.source_mtime;
        if !source_changed && marker.unmatched.is_empty() {
            return None;
        }
        let registry = match crate::coordinator::registry::load_strict(&dir) {
            Ok(registry) => registry,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "agents migration: managed.json is unreadable; nothing migrated"
                );
                marker.source_mtime = source_mtime;
                marker.migrated_unix = super::agents_model::now_unix();
                let _ = save_marker(&dir, &marker);
                return None;
            }
        };
        let unmatched_before = marker.unmatched.len();
        let report = self.apply_registry(&registry, &mut marker);
        if !source_changed && report.applied == 0 && report.unmatched == unmatched_before {
            // A retry that found nothing new: no write, no log line.
            return Some(report);
        }
        marker.source_mtime = source_mtime;
        marker.migrated_unix = super::agents_model::now_unix();
        if let Err(err) = save_marker(&dir, &marker) {
            tracing::warn!(err = %err, "agents migration: cannot write agents-v2.json");
        }
        tracing::info!(
            event = "agents.migrate",
            applied = report.applied,
            unchanged = report.skipped_unchanged,
            unmatched = report.unmatched,
            "managed.json migrated into pane meta"
        );
        if report.new_unmatched > 0 {
            self.notify_unmatched_migration(report.unmatched);
        }
        Some(report)
    }

    /// Apply the registry's new or changed entries.
    pub(crate) fn apply_registry(
        &mut self,
        registry: &crate::coordinator::registry::Registry,
        marker: &mut MigrationMarker,
    ) -> MigrationReport {
        let mut report = MigrationReport::default();
        let mut unmatched = Vec::new();
        let mut matched = 0;
        for entry in &registry.agents {
            if entry.is_coordinator() {
                continue;
            }
            let key = entry_key(entry);
            let print = fingerprint(entry);
            let unchanged = marker.fingerprints.get(&key) == Some(&print);
            let was_unmatched = marker
                .unmatched
                .iter()
                .any(|previous| entry_matches_unmatched(entry, previous));
            if unchanged && !was_unmatched {
                report.skipped_unchanged += 1;
                matched += 1;
                continue;
            }
            // An unchanged entry that was unmatched is tried again; only a
            // new or changed one counts as newly unmatched (the notice).
            marker.fingerprints.insert(key, print);
            match self.migration_match(entry) {
                Some((ws_idx, pane)) => {
                    matched += 1;
                    report.applied += 1;
                    let role = entry
                        .role
                        .as_deref()
                        .and_then(crate::workspace::team::sanitize_role);
                    let note = migrated_note(entry, &self.group_label(ws_idx));
                    self.write_meta(ws_idx, pane, |meta| {
                        if meta.role.is_none() && !meta.role_cleared {
                            meta.role = role;
                        }
                        if meta.note.is_none() && !meta.note_cleared {
                            meta.note = note;
                        }
                    });
                    self.mirror_member_role(ws_idx, pane);
                }
                None => {
                    if !unchanged {
                        report.new_unmatched += 1;
                    }
                    unmatched.push(UnmatchedEntry {
                        name: entry
                            .role
                            .clone()
                            .or_else(|| entry.agent.clone())
                            .unwrap_or_else(|| "agent".into()),
                        agent: entry.agent.clone(),
                        pane: entry.pane_id.clone(),
                        session: entry.session.clone(),
                        reason: if entry.session.is_some() {
                            "its session is not running in any pane".into()
                        } else {
                            "no session id and no pane newer than the entry".into()
                        },
                    })
                }
            }
        }
        report.unmatched = unmatched.len();
        marker.matched = matched;
        marker.unmatched = unmatched;
        report
    }

    /// The live terminal an entry names: by session, else by pane and kind
    /// when the entry is newer than the terminal.
    fn migration_match(
        &self,
        entry: &crate::coordinator::registry::ManagedAgent,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let session = entry.session.as_deref().filter(|s| !s.is_empty());
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if !terminal.is_agent_terminal() {
                        continue;
                    }
                    if let Some(session) = session {
                        let live = terminal
                            .persistable_agent_session()
                            .map(|session| session.session_ref.value);
                        if live.as_deref() == Some(session) {
                            return Some((ws_idx, *pane_id));
                        }
                        continue;
                    }
                    let public = self.public_pane_id(ws_idx, *pane_id);
                    let kind = self.model_agent_kind(ws_idx, *pane_id);
                    let same_pane = public.is_some() && entry.pane_id == public;
                    let same_kind = entry.agent.is_none() || kind == entry.agent;
                    let fresh = entry.added_unix > terminal.created_unix();
                    if same_pane && same_kind && fresh {
                        return Some((ws_idx, *pane_id));
                    }
                }
            }
        }
        None
    }

    /// A member's mirror role follows its meta (never through a rename).
    fn mirror_member_role(&mut self, ws_idx: usize, pane: crate::layout::PaneId) {
        let role = self
            .model_terminal(ws_idx, pane)
            .and_then(|terminal| terminal.agent_meta().role.clone());
        if let Some(member) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.team.as_mut())
            .and_then(|team| team.member_mut(pane))
        {
            if member.role.is_none() {
                member.role = role;
            }
        }
    }

    /// One notice card for the unmatched entries.
    fn notify_unmatched_migration(&mut self, count: usize) {
        let Some(pane_id) = self
            .state
            .workspaces
            .first()
            .and_then(|ws| ws.tabs.first())
            .map(|tab| tab.root_pane)
            .and_then(|pane| self.public_pane_id(0, pane))
        else {
            return;
        };
        let sender = super::agent_notices::NoticeSender {
            key: "herdr:agents-v2-migration".into(),
            pane_id,
            agent: None,
            name: "herdr+".into(),
        };
        let Some(title) = super::agent_notices::sanitize_title(&format!(
            "{count} herdr+ entries could not be matched (roles not carried over)"
        )) else {
            return;
        };
        let body = super::agent_notices::sanitize_body("see herdr coordinator status");
        if self
            .agent_notices
            .notify(
                sender,
                crate::api::schema::AgentNoticeKind::Warning,
                title,
                body,
                super::agents_model::now_unix(),
            )
            .is_ok()
        {
            self.render_dirty.request_generic();
            self.render_notify.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::registry::{ManagedAgent, Registry};

    fn entry(session: Option<&str>, pane: Option<&str>, role: &str, added: u64) -> ManagedAgent {
        ManagedAgent {
            session: session.map(str::to_string),
            pane_id: pane.map(str::to_string),
            agent: Some("claude".into()),
            role: Some(role.into()),
            project: None,
            note: None,
            added_unix: added,
        }
    }

    #[test]
    fn notes_fold_the_project_unless_it_is_the_group_label() {
        let mut e = entry(None, None, "lead", 1);
        e.project = Some("search it".into());
        assert_eq!(migrated_note(&e, "search it"), None);
        assert_eq!(
            migrated_note(&e, "other"),
            Some("project: search it".into())
        );
        e.note = Some("owns the API".into());
        assert_eq!(
            migrated_note(&e, "other"),
            Some("owns the API (project: search it)".into())
        );
    }

    #[test]
    fn keys_and_fingerprints_follow_the_entry() {
        let a = entry(Some("s1"), Some("w1:p1"), "lead", 1);
        let mut b = a.clone();
        assert_eq!(entry_key(&a), "session:s1");
        assert_eq!(fingerprint(&a), fingerprint(&b));
        b.role = Some("fixer".into());
        assert_ne!(fingerprint(&a), fingerprint(&b));
        let c = entry(None, Some("w1:p1"), "lead", 1);
        assert_eq!(entry_key(&c), "pane:w1:p1:claude");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-agents-migrate-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_registry(dir: &Path, registry: &Registry) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(registry).unwrap();
        std::fs::write(crate::coordinator::registry_path(dir), &bytes).unwrap();
        bytes
    }

    #[test]
    fn migration_matches_by_session_then_fresh_pane_and_never_resurrects_or_writes_the_source() {
        use crate::app::agents_model::tests::{model_app, pane, public, terminal_mut};
        let mut app = model_app();
        let dir = temp_dir("run");
        app.agents_model.dir = Some(dir.clone());
        // The plain group's agent has a native session.
        let plain_agent = pane(&app, 2, 0);
        terminal_mut(&mut app, plain_agent).set_persisted_agent_session(
            crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("sess-1").unwrap(),
                transcript_path: None,
            },
        );
        let fixer = pane(&app, 1, 1);
        let fixer_public = public(&app, 1, 1);
        let created = terminal_mut(&mut app, fixer).created_unix();
        let mut coordinator = entry(Some("coord"), None, "coordinator", 1);
        coordinator.role = Some(crate::coordinator::COORDINATOR_ROLE.into());
        let mut by_session = entry(Some("sess-1"), Some("w9:p9"), "researcher", 1);
        by_session.project = Some("papers".into());
        let registry = Registry {
            agents: vec![
                coordinator,
                by_session,
                // Older than the terminal: a reused pane id never matches.
                entry(
                    None,
                    Some(&fixer_public),
                    "stale",
                    created.saturating_sub(10),
                ),
                entry(Some("gone"), None, "ghost", 1),
            ],
        };
        let bytes = write_registry(&dir, &registry);
        let report = app.run_agents_migration().expect("ran");
        assert_eq!(report.applied, 1);
        assert_eq!(report.unmatched, 2);
        let meta = terminal_mut(&mut app, plain_agent).agent_meta().clone();
        assert_eq!(meta.role.as_deref(), Some("researcher"));
        assert_eq!(meta.note.as_deref(), Some("project: papers"));
        assert_eq!(terminal_mut(&mut app, fixer).agent_meta().role, None);
        let marker = load_marker(&dir);
        assert_eq!(marker.unmatched.len(), 2);
        // Unchanged: the unmatched entries are retried; nothing new, so no
        // write and no notice.
        let written = std::fs::read(marker_path(&dir)).unwrap();
        let retry = app.run_agents_migration().expect("retried");
        assert_eq!(
            (retry.applied, retry.new_unmatched, retry.unmatched),
            (0, 0, 2)
        );
        assert_eq!(std::fs::read(marker_path(&dir)).unwrap(), written);
        // The ghost's session shows up later (an agent detected only after
        // the first run): it is matched then, without a notice.
        terminal_mut(&mut app, fixer).set_persisted_agent_session(
            crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("gone").unwrap(),
                transcript_path: None,
            },
        );
        let retry = app.run_agents_migration().expect("retried");
        assert_eq!(
            (retry.applied, retry.new_unmatched, retry.unmatched),
            (1, 0, 1)
        );
        assert_eq!(
            terminal_mut(&mut app, fixer).agent_meta().role.as_deref(),
            Some("ghost")
        );
        assert_eq!(load_marker(&dir).unmatched.len(), 1);
        // A cleared role is never brought back, even when the source moves.
        app.write_meta(2, plain_agent, |meta| {
            meta.role = None;
            meta.role_cleared = true;
        });
        let mut marker = load_marker(&dir);
        marker.source_mtime = 0;
        marker.fingerprints.clear();
        save_marker(&dir, &marker).unwrap();
        app.run_agents_migration().expect("ran again");
        assert_eq!(terminal_mut(&mut app, plain_agent).agent_meta().role, None);
        // managed.json is never written.
        assert_eq!(
            std::fs::read(crate::coordinator::registry_path(&dir)).unwrap(),
            bytes
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_corrupt_registry_is_a_no_op() {
        use crate::app::agents_model::tests::model_app;
        let mut app = model_app();
        let dir = temp_dir("corrupt");
        app.agents_model.dir = Some(dir.clone());
        std::fs::write(crate::coordinator::registry_path(&dir), b"{not json").unwrap();
        assert!(app.run_agents_migration().is_none());
        assert_eq!(
            std::fs::read(crate::coordinator::registry_path(&dir)).unwrap(),
            b"{not json"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
