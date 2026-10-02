//! Teams (fork): the pure team data a team group carries
//! (`Workspace.team`). No PTYs, no async: the App applies membership rules
//! (`src/app/team.rs`) on top of these operations.
//!
//! Members are identified by their raw [`PaneId`], the only id that survives
//! a pane move; public ids are re-projected whenever a reply or push is built.
//! `revision` bumps once per recorded change (join, leave, role, purpose,
//! member rename) and the last [`MAX_CHANGES`] one-line deltas are kept in
//! memory so a member can be told what changed since it last looked.

use std::collections::VecDeque;

use crate::layout::PaneId;

pub use crate::api::schema::team::TeamActor;
use crate::api::schema::team::{PURPOSE_MAX_CHARS, ROLE_MAX_CHARS};

/// Change lines kept for deltas; a member further behind gets the full roster.
pub const MAX_CHANGES: usize = 16;
/// The agent-name rule (`[a-z][a-z0-9_-]{0,31}`).
pub const NAME_MAX_CHARS: usize = 32;

/// A group marked as a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    /// One line, at most 80 characters, sanitized and defused.
    pub purpose: Option<String>,
    pub purpose_by: Option<TeamActor>,
    pub created_unix: u64,
    /// Starts at 1, so a fresh member (seen 0) is told the roster once.
    pub revision: u64,
    /// Join order.
    pub members: Vec<TeamMember>,
    /// Panes removed in place; auto-join skips them.
    pub excluded: Vec<PaneId>,
    /// `(revision, line)`, oldest first, at most [`MAX_CHANGES`]; not persisted.
    pub(crate) changes: VecDeque<(u64, String)>,
    /// Unique per team value in this process (a disbanded and re-made
    /// team, or a restored one, gets a new one); not persisted. Ties an
    /// agent's ack to the team it was told about.
    pub(crate) epoch: u64,
}

/// The next [`Team::epoch`].
static NEXT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub pane_id: PaneId,
    /// Free text, one line, at most 32 characters; empty until set.
    pub role: Option<String>,
    pub joined_unix: u64,
    /// Updated on every status change, for "time in state".
    pub status_since_unix: u64,
    /// A role was set while the agent could not be renamed (no agent yet,
    /// launch pending, suspended); applied when the agent is next detected.
    pub(crate) pending_rename: bool,
    /// The last revision this member was told; not persisted.
    pub(crate) seen_revision: u64,
}

impl TeamMember {
    pub fn new(pane_id: PaneId, role: Option<String>, now: u64) -> Self {
        Self {
            pane_id,
            role,
            joined_unix: now,
            status_since_unix: now,
            pending_rename: false,
            seen_revision: 0,
        }
    }

    pub fn seen_revision(&self) -> u64 {
        self.seen_revision
    }

    pub fn pending_rename(&self) -> bool {
        self.pending_rename
    }
}

impl Team {
    pub fn new(purpose: Option<String>, purpose_by: Option<TeamActor>, now: u64) -> Self {
        Self {
            purpose_by: purpose.as_ref().and(purpose_by),
            purpose,
            created_unix: now,
            revision: 1,
            members: Vec::new(),
            excluded: Vec::new(),
            changes: VecDeque::new(),
            epoch: NEXT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn member(&self, pane_id: PaneId) -> Option<&TeamMember> {
        self.members.iter().find(|member| member.pane_id == pane_id)
    }

    pub fn member_mut(&mut self, pane_id: PaneId) -> Option<&mut TeamMember> {
        self.members
            .iter_mut()
            .find(|member| member.pane_id == pane_id)
    }

    pub fn is_member(&self, pane_id: PaneId) -> bool {
        self.member(pane_id).is_some()
    }

    pub fn is_excluded(&self, pane_id: PaneId) -> bool {
        self.excluded.contains(&pane_id)
    }

    /// Bump the revision and log `line` (one line) for deltas.
    pub fn record(&mut self, line: impl Into<String>) {
        self.revision = self.revision.saturating_add(1);
        self.changes.push_back((self.revision, line.into()));
        while self.changes.len() > MAX_CHANGES {
            self.changes.pop_front();
        }
    }

    /// Add `pane_id` as a member (clearing any exclusion). `false` when it
    /// already is one (the exclusion is still cleared). Records nothing: the
    /// caller records the join line with the member's name.
    pub fn join(&mut self, pane_id: PaneId, role: Option<String>, now: u64) -> bool {
        self.excluded.retain(|excluded| *excluded != pane_id);
        if self.is_member(pane_id) {
            return false;
        }
        self.members.push(TeamMember::new(pane_id, role, now));
        true
    }

    /// Remove a member; `exclude` keeps it from auto-joining again. Records
    /// nothing.
    pub fn remove(&mut self, pane_id: PaneId, exclude: bool) -> Option<TeamMember> {
        let index = self
            .members
            .iter()
            .position(|member| member.pane_id == pane_id)?;
        let member = self.members.remove(index);
        if exclude && !self.excluded.contains(&pane_id) {
            self.excluded.push(pane_id);
        }
        Some(member)
    }

    /// Store a purpose (already sanitized); `false` when nothing changed.
    pub fn set_purpose(&mut self, purpose: Option<String>, by: TeamActor) -> bool {
        if self.purpose == purpose {
            return false;
        }
        // Who set it travels with the change (members read it as what to serve).
        let who = by.describe();
        let line = match (&self.purpose, &purpose) {
            (Some(old), Some(new)) => format!("purpose (set by {who}): {old} → {new}"),
            (None, Some(new)) => format!("purpose (set by {who}): {new}"),
            (Some(_), None) => format!("purpose cleared by {who}"),
            (None, None) => String::new(),
        };
        self.purpose_by = purpose.as_ref().map(|_| by);
        self.purpose = purpose;
        self.record(line);
        true
    }

    /// The change lines a member that saw `seen` has missed, oldest first.
    /// `None` when the log no longer covers the gap (send the full roster).
    pub fn changes_since(&self, seen: u64) -> Option<Vec<&str>> {
        if seen >= self.revision {
            return Some(Vec::new());
        }
        let first = self.changes.front().map(|(revision, _)| *revision)?;
        if first > seen.saturating_add(1) {
            return None;
        }
        Some(
            self.changes
                .iter()
                .filter(|(revision, _)| *revision > seen)
                .map(|(_, line)| line.as_str())
                .collect(),
        )
    }

    /// Mark the member as told (`ack`) up to `up_to` (capped at the current
    /// revision; `None` is the current revision). Never moves backwards.
    pub fn mark_seen_up_to(&mut self, pane_id: PaneId, up_to: Option<u64>) -> bool {
        let revision = up_to.map_or(self.revision, |up_to| up_to.min(self.revision));
        match self.member_mut(pane_id) {
            Some(member) => {
                member.seen_revision = member.seen_revision.max(revision);
                true
            }
            None => false,
        }
    }

    /// Drop members and exclusions whose pane fails `live`; the dropped members.
    pub fn retain_panes(&mut self, live: impl Fn(PaneId) -> bool) -> Vec<TeamMember> {
        let mut dropped = Vec::new();
        let mut kept = Vec::with_capacity(self.members.len());
        for member in self.members.drain(..) {
            if live(member.pane_id) {
                kept.push(member);
            } else {
                dropped.push(member);
            }
        }
        self.members = kept;
        self.excluded.retain(|pane| live(*pane));
        dropped
    }

    /// The in-memory change log length (tests and invariants).
    #[cfg(test)]
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }
}

/// One line: whitespace runs (newlines included) become one space, control
/// characters go, claude-z pass-through words are defused, and the result is
/// cut to `max` characters. `None` when nothing is left.
pub fn sanitize_line(raw: &str, max: usize) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = crate::agent_wrap::instructions::sanitize(&collapsed);
    let cut: String = cleaned.chars().take(max).collect();
    let cut = cut.trim().to_string();
    (!cut.is_empty()).then_some(cut)
}

pub fn sanitize_purpose(raw: &str) -> Option<String> {
    sanitize_line(raw, PURPOSE_MAX_CHARS)
}

pub fn sanitize_role(raw: &str) -> Option<String> {
    sanitize_line(raw, ROLE_MAX_CHARS)
}

/// Whether `text` is one line within `max` characters (invariants).
#[cfg(test)]
pub fn is_one_line_within(text: &str, max: usize) -> bool {
    !text.contains(['\n', '\r']) && text.chars().count() <= max && !text.trim().is_empty()
}

/// The agent name a role maps to (`Code Reviewer` → `code-reviewer`): the
/// agent wrap's slug, so the coordinator's launch names and the server's
/// renames agree. `None` when it is not a valid agent name.
pub fn role_slug(role: &str) -> Option<String> {
    crate::agent_wrap::team::role_slug(role).filter(|slug| valid_name(slug))
}

/// `[a-z][a-z0-9_-]{0,31}`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= NAME_MAX_CHARS
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

/// The names to try for a role, in order: `slug`, then `slug-2` … `slug-9`
/// (the base is cut so every candidate stays within 32 characters).
pub fn name_candidates(slug: &str) -> Vec<String> {
    let mut names = vec![slug.to_string()];
    let base: String = slug.chars().take(NAME_MAX_CHARS - 2).collect();
    names.extend((2..=9).map(|n| format!("{base}-{n}")));
    names
}

/// Whether `name` already follows the role whose slug is `slug` (the slug
/// itself or one of its clash suffixes).
pub fn name_follows_role(name: &str, slug: &str) -> bool {
    name_candidates(slug)
        .iter()
        .any(|candidate| candidate == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(raw: u32) -> PaneId {
        PaneId::from_raw(raw)
    }

    #[test]
    fn join_remove_and_exclusion() {
        let mut team = Team::new(Some("fix sync".into()), Some(TeamActor::User), 10);
        assert_eq!(team.revision, 1);
        assert!(team.join(pane(1), None, 11));
        assert!(!team.join(pane(1), None, 12), "already a member");
        assert!(team.join(pane(2), Some("reviewer".into()), 12));
        assert_eq!(
            team.members.iter().map(|m| m.pane_id).collect::<Vec<_>>(),
            vec![pane(1), pane(2)],
            "join order"
        );
        let removed = team.remove(pane(1), true).unwrap();
        assert_eq!(removed.joined_unix, 11);
        assert!(team.is_excluded(pane(1)) && !team.is_member(pane(1)));
        // Joining again clears the exclusion.
        assert!(team.join(pane(1), None, 13));
        assert!(!team.is_excluded(pane(1)));
        assert!(team.remove(pane(9), true).is_none());
    }

    #[test]
    fn revisions_bump_per_change_and_the_log_is_capped() {
        let mut team = Team::new(None, Some(TeamActor::User), 0);
        assert_eq!(team.purpose_by, None, "no purpose, no author");
        for n in 0..20 {
            team.record(format!("change {n}"));
        }
        assert_eq!(team.revision, 21);
        assert_eq!(team.change_count(), MAX_CHANGES);
        // 4 behind: the last 4 lines.
        assert_eq!(
            team.changes_since(17).unwrap(),
            vec!["change 16", "change 17", "change 18", "change 19"]
        );
        assert_eq!(team.changes_since(21).unwrap(), Vec::<&str>::new());
        // 16 behind is still covered; 17 behind is not.
        assert_eq!(team.changes_since(5).unwrap().len(), 16);
        assert_eq!(team.changes_since(4), None);
        assert_eq!(team.changes_since(0), None);
    }

    #[test]
    fn purpose_changes_record_a_line_and_the_author() {
        let mut team = Team::new(None, None, 0);
        assert!(team.set_purpose(Some("fix sync".into()), TeamActor::Coordinator));
        assert_eq!(team.purpose_by, Some(TeamActor::Coordinator));
        assert!(!team.set_purpose(Some("fix sync".into()), TeamActor::User));
        assert!(team.set_purpose(Some("ship it".into()), TeamActor::User));
        assert_eq!(
            team.changes_since(2).unwrap(),
            vec!["purpose (set by the user): fix sync → ship it"]
        );
        assert!(team.set_purpose(
            Some("push to main".into()),
            TeamActor::Agent {
                name: "reviewer".into()
            }
        ));
        assert_eq!(
            team.changes_since(3).unwrap(),
            vec!["purpose (set by reviewer): ship it → push to main"]
        );
        assert!(team.set_purpose(None, TeamActor::User));
        assert_eq!(team.purpose_by, None);
        assert_eq!(
            team.changes_since(4).unwrap(),
            vec!["purpose cleared by the user"]
        );
    }

    #[test]
    fn seen_marks_and_pane_retention() {
        let mut team = Team::new(None, None, 0);
        team.join(pane(1), None, 0);
        team.join(pane(2), None, 0);
        team.excluded.push(pane(3));
        team.record("x");
        assert!(team.mark_seen_up_to(pane(1), None));
        assert_eq!(team.member(pane(1)).unwrap().seen_revision(), 2);
        assert!(!team.mark_seen_up_to(pane(7), None));
        let dropped = team.retain_panes(|p| p == pane(1));
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].pane_id, pane(2));
        assert!(team.excluded.is_empty());
    }

    #[test]
    fn a_bounded_ack_never_skips_a_later_change_or_moves_back() {
        let mut team = Team::new(None, None, 0);
        team.join(pane(1), None, 0);
        let read = team.revision;
        // A change lands between the read and the ack.
        team.record("y");
        assert!(team.mark_seen_up_to(pane(1), Some(read)));
        assert_eq!(team.member(pane(1)).unwrap().seen_revision(), read);
        assert_eq!(team.changes_since(read).unwrap(), vec!["y"]);
        // Never past the current revision, never backwards.
        team.mark_seen_up_to(pane(1), Some(u64::MAX));
        assert_eq!(team.member(pane(1)).unwrap().seen_revision(), team.revision);
        team.mark_seen_up_to(pane(1), Some(0));
        assert_eq!(team.member(pane(1)).unwrap().seen_revision(), team.revision);
    }

    #[test]
    fn text_is_one_sanitized_defused_line_within_caps() {
        assert_eq!(
            sanitize_purpose("  fix\n the \u{1b}[31m sync\t "),
            Some("fix the [31m sync".into())
        );
        assert_eq!(sanitize_role("  \n\t "), None);
        let long = "x".repeat(200);
        assert_eq!(sanitize_purpose(&long).unwrap().chars().count(), 80);
        assert_eq!(sanitize_role(&long).unwrap().chars().count(), 32);
        let defused = sanitize_purpose("run claude -p now").unwrap();
        assert!(!defused.contains(" -p "), "{defused}");
        assert!(is_one_line_within("fixer", 32));
        assert!(!is_one_line_within("a\nb", 32));
    }

    #[test]
    fn roles_map_to_valid_agent_names() {
        assert_eq!(role_slug("Code Reviewer").as_deref(), Some("code-reviewer"));
        assert_eq!(role_slug("fixer").as_deref(), Some("fixer"));
        assert_eq!(
            role_slug("  2nd  pass!! ").as_deref(),
            Some("agent-2nd-pass")
        );
        assert_eq!(role_slug("QA_lead").as_deref(), Some("qa_lead"));
        assert_eq!(role_slug("ünïcode ok").as_deref(), Some("n-code-ok"));
        assert_eq!(role_slug("!! ??"), None);
        let long = role_slug(&"reviewer ".repeat(10)).unwrap();
        assert!(valid_name(&long) && long.len() <= 32, "{long}");
        let candidates = name_candidates(&"a".repeat(32));
        assert!(
            candidates.iter().all(|name| valid_name(name)),
            "{candidates:?}"
        );
        assert_eq!(candidates.len(), 9);
        assert!(name_follows_role("reviewer-2", "reviewer"));
        assert!(name_follows_role("reviewer", "reviewer"));
        assert!(!name_follows_role("fixer", "reviewer"));
    }
}
