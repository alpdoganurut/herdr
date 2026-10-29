//! Fork: a pane whose saved working directory is gone (a removed git
//! worktree, typically) starts anyway. Where upstream would show "Saved
//! directory is unavailable" on a restore or a deferred agent resume, herdr
//! recreates the directory (an absolute path strictly under the user's home,
//! `create_dir_all`; on failure the card stays) at the same path, so a native
//! resume (`claude --resume`, `codex resume`) still finds its session.
//!
//! Each recreated pane is logged (tracing and an `actions.jsonl` line, action
//! `recreate_cwd`) and gets one agent card. An agent pane (a pending native
//! resume or a suspended agent) is also told once, through the message queue
//! (src/app/message_queue.rs), so every delivery gate applies: the notice
//! waits here until the agent runs (no resume pending, not suspended, its
//! runtime up, its agent detected), then goes into the queue, which types it
//! in once the agent is free. A plain shell gets no message. Pending notices
//! are saved with the queue (`message_queue.json`, keyed by the stable public
//! pane id: a restore allocates new terminal ids) and re-armed after a restart
//! or a live handoff; they are dropped when their pane is gone or after
//! [`GIVE_UP`] (counted from the activation of a suspended agent). Once
//! queued, a herdr notice whose agent is not detected (a slow resume) waits
//! in the queue while its pane is there, up to the user-blocked hold
//! ([`super::message_queue::MESSAGE_TTL_USER_BLOCKED_S`]), instead of the
//! queue's 60 s grace.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::App;
use crate::terminal::TerminalId;

/// What an agent pane is told; `{path}` is its working directory.
pub(crate) const MISSING_CWD_MESSAGE: &str = "herdr+: your working directory {path} was missing (a removed worktree?) and herdr recreated it empty so this session could resume. Files you expect there are gone. Before anything else, tell your user with agents_notify, and don't assume the old files exist.";

/// How often pending notices look at their pane while any wait.
const POLL: Duration = Duration::from_secs(3);
/// A notice whose agent never came up is dropped after this.
const GIVE_UP: Duration = Duration::from_secs(24 * 3600);

/// The notice text for `path`.
pub(crate) fn missing_cwd_message(path: &Path) -> String {
    MISSING_CWD_MESSAGE.replace("{path}", &path.display().to_string())
}

/// Agent panes waiting for their notice (in [`super::message_queue::MessageQueue`]).
#[derive(Default)]
pub(crate) struct MissingCwdNotices {
    pending: HashMap<TerminalId, Pending>,
    poll_at: Option<Instant>,
    /// Loaded from `message_queue.json`, not yet matched to a pane
    /// ([`App::rearm_missing_cwd_notices`]); saved again until then.
    loaded: Vec<SavedMissingCwdNotice>,
}

struct Pending {
    path: PathBuf,
    since: Instant,
    /// `since` as a Unix time, for the saved queue.
    since_unix: u64,
    /// The pane's public id (what is saved): taken when the notice is armed,
    /// followed when the pane moves ([`App::follow_missing_cwd_panes`]).
    pane: Option<String>,
    /// Its agent was suspended at the last pass: the [`GIVE_UP`] clock
    /// restarts on the first pass after its activation (saved, so a restart
    /// in between does not count the parked time).
    parked: bool,
}

/// A pending notice in `message_queue.json` (`missing_cwd`; older builds
/// ignore the field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedMissingCwdNotice {
    /// The pane's public id (`w1:p2`), stable across a restore.
    pub(crate) pane: String,
    pub(crate) path: PathBuf,
    pub(crate) since_unix: u64,
    /// Its agent was suspended: `since_unix` waits for the activation, so
    /// the notice's age does not count (older files: not parked).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) parked: bool,
}

impl MissingCwdNotices {
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        if self.pending.is_empty() {
            None
        } else {
            self.poll_at
        }
    }

    /// An agent event: look at the pending panes on the next pass.
    pub(crate) fn mark_due(&mut self, now: Instant) {
        if !self.pending.is_empty() {
            self.poll_at = Some(now);
        }
    }

    /// What `message_queue.json` keeps: every pending notice with a pane id,
    /// and the loaded ones not re-armed yet.
    pub(crate) fn saved(&self) -> Vec<SavedMissingCwdNotice> {
        let mut saved: Vec<SavedMissingCwdNotice> = self
            .pending
            .values()
            .filter_map(|pending| {
                Some(SavedMissingCwdNotice {
                    pane: pending.pane.clone()?,
                    path: pending.path.clone(),
                    since_unix: pending.since_unix,
                    parked: pending.parked,
                })
            })
            .collect();
        saved.sort_by(|left, right| left.pane.cmp(&right.pane));
        saved.extend(self.loaded.iter().cloned());
        saved
    }

    /// The notices `message_queue.json` kept (re-armed by
    /// [`App::rearm_missing_cwd_notices`]).
    pub(crate) fn load_saved_notices(&mut self, saved: Vec<SavedMissingCwdNotice>) {
        self.loaded = saved;
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// Recreate the missing directory `cwd` when it is an absolute path strictly
/// under the user's home (a home with a named component; resolved through
/// symlinks, the existing part of `cwd` stays under it). Returns whether it
/// is a directory now.
pub(crate) fn recreate_missing_cwd(cwd: &Path) -> bool {
    match creation_root() {
        Some(home) => recreate_missing_cwd_under(cwd, &home),
        None => false,
    }
}

/// [`recreate_missing_cwd`] with the home given.
pub(crate) fn recreate_missing_cwd_under(cwd: &Path, home: &Path) -> bool {
    let plain = |path: &Path| {
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    };
    if !plain(cwd)
        || !plain(home)
        || !home_is_creation_root(home)
        || cwd == home
        || !cwd.starts_with(home)
    {
        return false;
    }
    // A symlinked directory under the home must not lead the creation out of
    // it: the nearest existing ancestor, resolved, stays under the resolved
    // home.
    let Ok(real_home) = home.canonicalize() else {
        return false;
    };
    let Some(real_ancestor) = cwd
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .and_then(|ancestor| ancestor.canonicalize().ok())
    else {
        return false;
    };
    if !real_ancestor.starts_with(&real_home) {
        tracing::warn!(cwd = %cwd.display(), resolved = %real_ancestor.display(), "a missing pane directory resolves outside the home; not recreated");
        return false;
    }
    match std::fs::create_dir_all(cwd) {
        Ok(()) => cwd.is_dir(),
        Err(err) => {
            tracing::warn!(cwd = %cwd.display(), err = %err, "cannot recreate a missing pane directory");
            false
        }
    }
}

/// A home that can hold recreated directories: at least one named component
/// (`/root` is one, `HOME=/` is none and would let every path in).
fn home_is_creation_root(home: &Path) -> bool {
    home.components()
        .any(|part| matches!(part, Component::Normal(_)))
}

#[cfg(not(test))]
fn creation_root() -> Option<PathBuf> {
    crate::integration::home_dir().ok()
}

// Tests never create directories under the real home: a test opts in with
// [`set_test_home`] (per thread); without it nothing is recreated, so the
// upstream tests of the unavailable-directory card keep their behavior.
#[cfg(test)]
thread_local! {
    static TEST_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn creation_root() -> Option<PathBuf> {
    TEST_HOME.with(|home| home.borrow().clone())
}

#[cfg(test)]
pub(crate) fn set_test_home(home: Option<PathBuf>) {
    TEST_HOME.with(|slot| *slot.borrow_mut() = home);
}

enum Gate {
    Gone,
    Wait,
    /// Suspended: waits for its activation (an agent event wakes it; no
    /// poll, and the [`GIVE_UP`] clock restarts).
    Parked,
    Ready(super::terminal_targets::TerminalTarget),
}

impl App {
    /// After a restore: the panes whose missing directory restore recreated
    /// (`TerminalState.cwd_recreated`).
    pub(crate) fn note_recreated_cwds(&mut self) {
        let recreated: Vec<(TerminalId, bool)> = self
            .state
            .terminals
            .values_mut()
            .filter(|terminal| terminal.cwd_recreated)
            .map(|terminal| {
                terminal.cwd_recreated = false;
                let agent = terminal.pending_agent_resume_plan.is_some()
                    || terminal.suspended_agent.is_some();
                (terminal.id.clone(), agent)
            })
            .collect();
        for (terminal_id, agent) in recreated {
            self.note_recreated_cwd(&terminal_id, agent);
        }
    }

    /// `terminal_id`'s directory was recreated: log it, show one card, and
    /// for an agent pane wait to tell its agent.
    pub(crate) fn note_recreated_cwd(&mut self, terminal_id: &TerminalId, agent: bool) {
        let Some((path, parked)) = self
            .state
            .terminals
            .get(terminal_id)
            .map(|terminal| (terminal.cwd.clone(), terminal.suspended_agent.is_some()))
        else {
            return;
        };
        tracing::warn!(
            terminal = %terminal_id,
            cwd = %path.display(),
            agent,
            "the pane's saved directory was missing; recreated it empty"
        );
        self.log_recreated_cwd(terminal_id, &path);
        self.show_recreated_cwd_card(terminal_id, &path);
        if agent {
            let now = Instant::now();
            let pane = self
                .resolve_terminal_target(&terminal_id.to_string())
                .ok()
                .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id));
            let notices = &mut self.message_queue.missing_cwd;
            notices
                .pending
                .entry(terminal_id.clone())
                .or_insert(Pending {
                    path,
                    since: now,
                    since_unix: crate::coordinator::now_unix(),
                    pane,
                    parked,
                });
            notices.poll_at = Some(now);
            self.message_queue.save();
        }
    }

    /// After a restart or a live handoff: the notices `message_queue.json`
    /// kept wait again for the pane with the same public id (one saved parked,
    /// or whose agent is suspended, stays parked, so its age does not count).
    /// A notice whose pane is gone, or past [`GIVE_UP`] for a running pane,
    /// is dropped.
    pub(crate) fn rearm_missing_cwd_notices(&mut self, now: Instant, now_unix: u64) {
        let saved = std::mem::take(&mut self.message_queue.missing_cwd.loaded);
        if saved.is_empty() {
            return;
        }
        let mut armed = 0usize;
        for notice in saved {
            let Some(target) = self.resolve_terminal_target(&notice.pane).ok() else {
                continue;
            };
            let Some(terminal) = self.state.terminals.get(target.terminal_id.as_str()) else {
                continue;
            };
            let terminal_id = terminal.id.clone();
            let parked = notice.parked || terminal.suspended_agent.is_some();
            let age = now_unix.saturating_sub(notice.since_unix);
            if !parked && age > GIVE_UP.as_secs() {
                continue;
            }
            let pane = self.public_pane_id(target.ws_idx, target.pane_id);
            let since = now.checked_sub(Duration::from_secs(age)).unwrap_or(now);
            self.message_queue
                .missing_cwd
                .pending
                .entry(terminal_id)
                .or_insert(Pending {
                    path: notice.path,
                    since,
                    since_unix: notice.since_unix,
                    pane,
                    parked,
                });
            armed += 1;
        }
        if armed > 0 {
            self.message_queue.missing_cwd.poll_at = Some(now);
            tracing::info!(armed, "re-armed saved recreated-directory notices");
        }
        self.message_queue.save();
    }

    /// Queue the notice of every pending agent pane whose agent is up. Runs
    /// with the message queue's pass, at most every [`POLL`] or on an agent
    /// event. Returns whether a notice was queued.
    pub(crate) fn deliver_missing_cwd_notices(&mut self, now: Instant, now_unix: u64) -> bool {
        let notices = &mut self.message_queue.missing_cwd;
        if notices.pending.is_empty() || notices.poll_at.is_none_or(|at| now < at) {
            return false;
        }
        notices.poll_at = Some(now + POLL);
        let ids: Vec<TerminalId> = notices.pending.keys().cloned().collect();
        let mut queued = false;
        let mut polled = false;
        let mut changed = false;
        for terminal_id in ids {
            // Activated since the last pass: the GIVE_UP clock starts now,
            // however long it was parked.
            let suspended = self
                .state
                .terminals
                .get(&terminal_id)
                .is_some_and(|terminal| terminal.suspended_agent.is_some());
            if let Some(pending) = self.message_queue.missing_cwd.pending.get_mut(&terminal_id) {
                if pending.parked && !suspended {
                    pending.parked = false;
                    pending.since = now;
                    pending.since_unix = now_unix;
                    changed = true;
                }
            }
            match self.missing_cwd_gate(&terminal_id, now) {
                Gate::Wait => polled = true,
                Gate::Parked => {
                    if let Some(pending) =
                        self.message_queue.missing_cwd.pending.get_mut(&terminal_id)
                    {
                        // Saved once, when it parks (`since_unix` stays).
                        if !pending.parked {
                            pending.parked = true;
                            changed = true;
                        }
                        pending.since = now;
                    }
                }
                Gate::Gone => {
                    self.message_queue.missing_cwd.pending.remove(&terminal_id);
                    changed = true;
                }
                Gate::Ready(target) => {
                    let Some(pending) = self.message_queue.missing_cwd.pending.remove(&terminal_id)
                    else {
                        continue;
                    };
                    let id = self.queue_herdr_notice(
                        &target,
                        missing_cwd_message(&pending.path),
                        "your working directory",
                        now_unix,
                    );
                    tracing::info!(
                        terminal = %terminal_id,
                        message = %id,
                        "queued the recreated-directory notice for the pane's agent"
                    );
                    queued = true;
                }
            }
        }
        if !polled {
            self.message_queue.missing_cwd.poll_at = None;
        }
        if changed {
            // (queueing a notice saved the queue, but maybe before a later
            // change of this pass)
            self.message_queue.save();
        }
        queued
    }

    /// A pane moved (to another tab or space): a pending notice saves its
    /// pane's new public id, so a restart re-arms it on that pane. O(pending
    /// notices), nothing without one.
    pub(crate) fn follow_missing_cwd_panes(&mut self) {
        if self.message_queue.missing_cwd.pending.is_empty() {
            return;
        }
        let ids: Vec<TerminalId> = self
            .message_queue
            .missing_cwd
            .pending
            .keys()
            .cloned()
            .collect();
        let mut changed = false;
        for terminal_id in ids {
            let id = terminal_id.to_string();
            // A pane that is gone keeps its id: the next pass drops it.
            let Some(pane) = self
                .resolve_terminal_target(&id)
                .ok()
                .filter(|target| target.terminal_id == id)
                .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
            else {
                continue;
            };
            if let Some(pending) = self.message_queue.missing_cwd.pending.get_mut(&terminal_id) {
                if pending.pane.as_deref() != Some(pane.as_str()) {
                    pending.pane = Some(pane);
                    changed = true;
                }
            }
        }
        if changed {
            self.message_queue.save();
        }
    }

    fn missing_cwd_gate(&self, terminal_id: &TerminalId, now: Instant) -> Gate {
        let Some(pending) = self.message_queue.missing_cwd.pending.get(terminal_id) else {
            return Gate::Gone;
        };
        let Some(terminal) = self.state.terminals.get(terminal_id) else {
            return Gate::Gone;
        };
        if terminal.restore_error.is_some() {
            return Gate::Gone;
        }
        if terminal.suspended_agent.is_some() {
            return Gate::Parked;
        }
        if now.saturating_duration_since(pending.since) > GIVE_UP {
            return Gate::Gone;
        }
        if terminal.pending_agent_resume_plan.is_some()
            || self.terminal_runtimes.get(terminal_id).is_none()
        {
            return Gate::Wait;
        }
        let id = terminal_id.to_string();
        let Some(target) = self
            .resolve_terminal_target(&id)
            .ok()
            .filter(|target| target.terminal_id == id)
        else {
            return Gate::Gone;
        };
        match self.message_check(&target) {
            super::message_queue::Check::Missing => Gate::Wait,
            _ => Gate::Ready(target),
        }
    }

    /// One `actions.jsonl` line (action `recreate_cwd`), as herdr.
    fn log_recreated_cwd(&mut self, terminal_id: &TerminalId, path: &Path) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        let target = self.resolve_terminal_target(&terminal_id.to_string()).ok();
        let line = super::agents_model::LogLine {
            action: "recreate_cwd",
            target_tab: target
                .as_ref()
                .and_then(|target| self.public_tab_id(target.ws_idx, target.tab_idx)),
            target_pane: target
                .as_ref()
                .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id)),
            target_name: self
                .state
                .terminals
                .get(terminal_id)
                .and_then(|terminal| terminal.agent_name.clone()),
            detail: Some(format!(
                "{} was missing; recreated it empty",
                path.display()
            )),
            ..super::agents_model::LogLine::default()
        };
        let mut entry = self.action_entry(
            None,
            crate::api::schema::agents_model::AgentsActionOutcome::Ok,
            line,
        );
        entry.actor_name = Some("herdr".into());
        if let Err(err) = crate::agents_model::actions_log::append(&dir, &entry) {
            tracing::warn!(err = %err, "missing directory: cannot append to the action log");
        }
    }

    /// One agent card for the pane (a duplicate is deduped; notices off
    /// shows none).
    fn show_recreated_cwd_card(&mut self, terminal_id: &TerminalId, path: &Path) {
        let Some(pane) = self
            .resolve_terminal_target(&terminal_id.to_string())
            .ok()
            .and_then(|target| self.public_pane_id(target.ws_idx, target.pane_id))
        else {
            return;
        };
        let Some(sender) = self.notice_sender(&pane) else {
            return;
        };
        let title =
            super::agent_notices::sanitize_title("Missing directory recreated").unwrap_or_default();
        let body = super::agent_notices::sanitize_body(&format!(
            "{} was missing (a removed worktree?); herdr recreated it empty so this pane could start.",
            path.display()
        ));
        match self.agent_notices.notify(
            sender,
            crate::api::schema::AgentNoticeKind::Warning,
            title,
            body,
            crate::coordinator::now_unix(),
        ) {
            Ok((_, outcome)) => {
                if outcome == crate::api::schema::AgentNotifyOutcome::Shown {
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                }
            }
            Err(refused) => {
                tracing::debug!(code = %refused.code, "missing directory: the card was not shown");
            }
        }
    }
}

#[cfg(test)]
mod tests;
