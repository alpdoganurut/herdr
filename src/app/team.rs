//! Teams (fork): the App side of `Workspace.team` — membership rules, naming
//! by role, the `team.*` handlers, and following panes as they move, close
//! or start agents.
//!
//! Team state is a shared runtime fact: it lives on the server with the
//! workspace (persisted), clients only render the `endpoint.teams.v1` push.
//!
//! Cost: everything here is gated on `AppState::team_count > 0` from
//! `emit_event`, never runs from render or output paths, and is O(members)
//! (plus a scan of the workspaces) per structural event. Status changes only
//! touch one member's `status_since_unix` and never bump the view revision.

use std::collections::HashMap;

use super::agents::AgentRenameError;
use super::api::responses::{encode_error, encode_success};
use super::state::AppState;
use super::App;
use crate::api::schema::team::error_code;
use crate::api::schema::{
    AgentStatus, EventData, EventEnvelope, EventKind, ResponseResult, TeamActor, TeamContextParams,
    TeamGetParams, TeamInfo, TeamJoinParams, TeamMakeParams, TeamMemberInfo, TeamPaneParams,
    TeamSetPurposeParams, TeamSetRoleParams, TeamWorkspaceParams,
};
use crate::layout::PaneId;
use crate::workspace::team::{self as model, Team};

/// How long a "you are no longer in a team" line waits for its pane.
const TOMBSTONE_TTL_S: u64 = 600;
pub(crate) const DISBANDED_LINE: &str = "[herdr+ team update] team disbanded";
pub(crate) const REMOVED_LINE: &str = "[herdr+ team update] you are no longer in the team";

/// One pending "you are no longer in a team" line per pane.
#[derive(Debug, Clone, Default)]
pub(crate) struct TeamTombstones {
    /// `(inserted at, line, seq)`; `seq` names this one line for acks.
    lines: HashMap<PaneId, (u64, &'static str, u64)>,
    seq: u64,
}

impl TeamTombstones {
    fn insert(&mut self, pane: PaneId, line: &'static str, now: u64) {
        self.prune(now);
        self.seq = self.seq.wrapping_add(1);
        self.lines.insert(pane, (now, line, self.seq));
    }

    fn prune(&mut self, now: u64) {
        self.lines
            .retain(|_, (at, _, _)| now.saturating_sub(*at) < TOMBSTONE_TTL_S);
    }

    /// The pending line and its ack key.
    fn peek(&self, pane: PaneId, now: u64) -> Option<(&'static str, String)> {
        self.lines
            .get(&pane)
            .filter(|(at, _, _)| now.saturating_sub(*at) < TOMBSTONE_TTL_S)
            .map(|(_, line, seq)| (*line, tombstone_ack_key(*seq)))
    }

    fn clear(&mut self, pane: PaneId) {
        self.lines.remove(&pane);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }
}

/// The `team.context` ack key of a team (see `TeamContextParams.ack_key`).
fn team_ack_key(team: &Team) -> String {
    format!("team:{}", team.epoch())
}

/// The ack key of one pending "no longer in a team" line.
fn tombstone_ack_key(seq: u64) -> String {
    format!("gone:{seq}")
}

impl AppState {
    /// Rebuild `team_index` and `team_count` from the workspaces; whether
    /// either changed.
    pub(crate) fn rebuild_team_index(&mut self) -> bool {
        let mut index = HashMap::new();
        let mut count = 0;
        for ws in &self.workspaces {
            if let Some(team) = &ws.team {
                count += 1;
                for member in &team.members {
                    index.insert(member.pane_id, ws.id.clone());
                }
            }
        }
        let changed = index != self.team_index || count != self.team_count;
        self.team_index = index;
        self.team_count = count;
        // Restored teams (session restore, live handoff) reach clients on
        // the first pass: revision 0 means "never had a team".
        if count > 0 && self.teams_view_rev == 0 {
            self.teams_view_rev = 1;
        }
        changed
    }
}

/// Why a team request failed.
struct TeamError {
    code: &'static str,
    message: String,
}

impl TeamError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

type TeamResult<T> = Result<T, TeamError>;

impl App {
    // ----- lookups ---------------------------------------------------------

    fn team_terminal(
        &self,
        ws_idx: usize,
        pane: PaneId,
    ) -> Option<&crate::terminal::TerminalState> {
        let state = self.state.workspaces.get(ws_idx)?.pane_state(pane)?;
        self.state.terminals.get(&state.attached_terminal_id)
    }

    /// The agent kind running (or parked) in a pane.
    fn team_agent_kind(&self, ws_idx: usize, pane: PaneId) -> Option<String> {
        let terminal = self.team_terminal(ws_idx, pane)?;
        terminal
            .effective_agent_label()
            .map(str::to_string)
            .or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .map(|record| record.agent.clone())
            })
    }

    fn team_agent_name(&self, ws_idx: usize, pane: PaneId) -> Option<String> {
        let terminal = self.team_terminal(ws_idx, pane)?;
        terminal
            .agent_name
            .clone()
            .or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .and_then(|record| record.name.clone())
            })
            .filter(|name| !name.trim().is_empty())
    }

    /// Whether a pane has, or had, an agent (detected, named or parked).
    fn team_pane_has_agent(&self, ws_idx: usize, pane: PaneId) -> bool {
        self.team_terminal(ws_idx, pane)
            .is_some_and(|terminal| terminal.is_agent_terminal())
    }

    /// The member's name as roster lines show it: agent name, else role,
    /// else agent kind, else "agent".
    fn team_display_name(&self, ws_idx: usize, pane: PaneId, role: Option<&str>) -> String {
        self.team_agent_name(ws_idx, pane)
            .or_else(|| role.map(str::to_string))
            .or_else(|| self.team_agent_kind(ws_idx, pane))
            .unwrap_or_else(|| "agent".into())
    }

    /// The same, for a pane that may have moved to another workspace.
    fn team_display_name_anywhere(&self, pane: PaneId, role: Option<&str>) -> String {
        match self.find_pane(pane) {
            Some((ws_idx, _)) => self.team_display_name(ws_idx, pane, role),
            None => role.map(str::to_string).unwrap_or_else(|| "agent".into()),
        }
    }

    /// A group by id, by its label (unique), or by sidebar number, in that
    /// order (a group labelled "2024" is found by its label); never an index
    /// past the last group.
    fn team_resolve_group(&self, group: &str) -> Option<usize> {
        let group = group.trim();
        if let Some(ws_idx) = self.state.workspaces.iter().position(|ws| ws.id == group) {
            return Some(ws_idx);
        }
        let mut hits = self.state.workspaces.iter().enumerate().filter(|(_, ws)| {
            ws.custom_name.as_deref() == Some(group) || ws.cached_auto_label == group
        });
        if let Some((ws_idx, _)) = hits.next() {
            return hits.next().is_none().then_some(ws_idx);
        }
        self.parse_workspace_id(group)
            .filter(|ws_idx| self.state.workspaces.get(*ws_idx).is_some())
    }

    fn team_group_or_error(&self, group: &str) -> TeamResult<usize> {
        self.team_resolve_group(group).ok_or_else(|| {
            TeamError::new(
                error_code::WORKSPACE_NOT_FOUND,
                format!("group {group} not found"),
            )
        })
    }

    fn team_pane_or_error(&self, pane: &str) -> TeamResult<(usize, PaneId)> {
        self.parse_pane_id(pane).ok_or_else(|| {
            TeamError::new(error_code::PANE_NOT_FOUND, format!("pane {pane} not found"))
        })
    }

    /// Who is asking: absent means the user; a pane is the coordinator when
    /// it is the coordinator's, else an agent by its own record.
    fn team_actor(&self, caller_pane: Option<&str>) -> TeamResult<TeamActor> {
        let Some(caller) = caller_pane.filter(|caller| !caller.trim().is_empty()) else {
            return Ok(TeamActor::User);
        };
        let (ws_idx, pane) = self.resolve_caller_pane(caller).ok_or_else(|| {
            TeamError::new(
                error_code::PANE_NOT_FOUND,
                format!("pane {caller} not found"),
            )
        })?;
        if self.is_coordinator_pane(ws_idx, pane) {
            return Ok(TeamActor::Coordinator);
        }
        let name = self
            .team_agent_name(ws_idx, pane)
            .or_else(|| self.team_agent_kind(ws_idx, pane))
            .or_else(|| self.public_pane_id(ws_idx, pane))
            .unwrap_or_else(|| "agent".into());
        Ok(TeamActor::Agent { name })
    }

    // ----- projection ------------------------------------------------------

    /// One member as replies and the push report it (public ids now).
    fn team_member_info(
        &self,
        ws_idx: usize,
        member: &model::TeamMember,
        with_status: bool,
    ) -> Option<TeamMemberInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = self.public_pane_id(ws_idx, member.pane_id)?;
        let tab_id = ws
            .find_tab_index_for_pane(member.pane_id)
            .and_then(|tab_idx| self.public_tab_id(ws_idx, tab_idx));
        let agent = self.team_agent_kind(ws_idx, member.pane_id);
        let status = (with_status && agent.is_some())
            .then(|| {
                let pane = ws.pane_state(member.pane_id)?;
                let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                Some(crate::workspace::agent_status(
                    terminal.state,
                    pane.seen,
                    terminal.suspended_agent.is_some(),
                ))
            })
            .flatten();
        Some(TeamMemberInfo {
            pane_id,
            tab_id,
            name: self.team_agent_name(ws_idx, member.pane_id),
            agent,
            role: member.role.clone(),
            status,
            status_since_unix: with_status.then_some(member.status_since_unix),
            joined_unix: member.joined_unix,
        })
    }

    /// A team group's team, re-projected now; `None` when it is not a team.
    /// `with_status` adds status and time in state (replies, not the push).
    pub(crate) fn team_info(&self, ws_idx: usize, with_status: bool) -> Option<TeamInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let team = ws.team.as_ref()?;
        Some(TeamInfo {
            workspace_id: ws.id.clone(),
            workspace_label: ws
                .custom_name
                .clone()
                .unwrap_or_else(|| ws.cached_auto_label.clone()),
            purpose: team.purpose.clone(),
            purpose_by: team.purpose_by.clone(),
            created_unix: team.created_unix,
            revision: team.revision,
            members: team
                .members
                .iter()
                .filter_map(|member| self.team_member_info(ws_idx, member, with_status))
                .collect(),
            excluded: team
                .excluded
                .iter()
                .filter_map(|pane| self.public_pane_id(ws_idx, *pane))
                .collect(),
        })
    }

    /// Every team, in sidebar order.
    pub(crate) fn team_infos(&self, with_status: bool) -> Vec<TeamInfo> {
        if self.state.team_count == 0 {
            return Vec::new();
        }
        (0..self.state.workspaces.len())
            .filter_map(|ws_idx| self.team_info(ws_idx, with_status))
            .collect()
    }

    // ----- change plumbing -------------------------------------------------

    /// A change clients render: bump the view revision and ask for a pass.
    fn bump_teams_view(&mut self) {
        self.state.teams_view_rev = self.state.teams_view_rev.wrapping_add(1).max(1);
        self.render_dirty.request_generic();
        self.render_notify.notify_one();
    }

    /// A structural change: clients, the saved session and the coordinator's
    /// facts all follow.
    fn teams_changed(&mut self) {
        self.state.rebuild_team_index();
        self.bump_teams_view();
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.mark_coordinator_input_dirty();
    }

    fn team_mut(&mut self, ws_idx: usize) -> Option<&mut Team> {
        self.state.workspaces.get_mut(ws_idx)?.team.as_mut()
    }

    /// Add a pane to its group's team and log the join; whether it joined.
    fn team_add_member(&mut self, ws_idx: usize, pane: PaneId, role: Option<String>) -> bool {
        // Fork (agents v2): the role lives on the pane; a member without one
        // takes the pane's (it survives leaving and rejoining).
        let role = role.or_else(|| {
            self.team_terminal(ws_idx, pane)
                .and_then(|terminal| terminal.agent_meta().role.clone())
        });
        let now = crate::coordinator::now_unix();
        let name = self.team_display_name(ws_idx, pane, role.as_deref());
        let agent = self.team_agent_kind(ws_idx, pane);
        let public = self.public_pane_id(ws_idx, pane).unwrap_or_default();
        let Some(team) = self.team_mut(ws_idx) else {
            return false;
        };
        if !team.join(pane, role, now) {
            return false;
        }
        team.record(match agent {
            Some(agent) => format!("{name} ({agent}, {public}) joined"),
            None => format!("{name} ({public}) joined"),
        });
        self.team_tombstones.clear(pane);
        self.state
            .team_index
            .insert(pane, self.state.workspaces[ws_idx].id.clone());
        true
    }

    /// Remove a member and log it; `exclude` keeps it from auto-joining.
    fn team_remove_member(&mut self, ws_idx: usize, pane: PaneId, exclude: bool) -> bool {
        let role = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.team.as_ref())
            .and_then(|team| team.member(pane))
            .and_then(|member| member.role.clone());
        let name = self.team_display_name(ws_idx, pane, role.as_deref());
        let Some(team) = self.team_mut(ws_idx) else {
            return false;
        };
        if team.remove(pane, exclude).is_none() {
            return false;
        }
        team.record(format!("{name} left"));
        // Fork (agents v2): the exclusion follows the pane (a close and
        // reopen does not rejoin); the role stays on the pane.
        let workspace_id = exclude.then(|| self.state.workspaces[ws_idx].id.clone());
        self.write_meta(ws_idx, pane, |meta| {
            if meta.role.is_none() && !meta.role_cleared {
                meta.role = role;
            }
            if let Some(workspace_id) = workspace_id {
                meta.team_excluded = Some(workspace_id);
            }
        });

        self.state.team_index.remove(&pane);
        self.team_tombstones
            .insert(pane, REMOVED_LINE, crate::coordinator::now_unix());
        true
    }

    // ----- naming by role --------------------------------------------------

    /// Whether another terminal already uses `name` (session-wide).
    fn team_name_taken(&self, name: &str, own: Option<&crate::terminal::TerminalId>) -> bool {
        self.state.terminals.values().any(|terminal| {
            Some(&terminal.id) != own
                && (terminal.agent_name.as_deref() == Some(name)
                    || terminal
                        .suspended_agent
                        .as_ref()
                        .and_then(|record| record.name.as_deref())
                        == Some(name))
        })
    }

    /// Make the member's agent name follow its role (`slug(role)`, then
    /// `-2`..`-9` on a clash). `Some(true)` when the name follows the role,
    /// `Some(false)` when every candidate is taken (the name stays), `None`
    /// when there is no role or the rename waits for the agent (pending).
    fn team_apply_role_name(&mut self, ws_idx: usize, pane: PaneId) -> Option<bool> {
        let role = self
            .state
            .workspaces
            .get(ws_idx)?
            .team
            .as_ref()?
            .member(pane)?
            .role
            .clone();
        let Some(slug) = role.as_deref().and_then(model::role_slug) else {
            self.team_set_pending(ws_idx, pane, false);
            return None;
        };
        let (ready, terminal_id, old_name) = {
            let terminal = self.team_terminal(ws_idx, pane)?;
            (
                terminal.effective_agent_label().is_some()
                    && !terminal.managed_agent_launch_pending()
                    && terminal.suspended_agent.is_none(),
                terminal.id.clone(),
                terminal.agent_name.clone(),
            )
        };
        if old_name
            .as_deref()
            .is_some_and(|name| model::name_follows_role(name, &slug))
        {
            self.team_set_pending(ws_idx, pane, false);
            return Some(true);
        }
        if !ready {
            self.team_set_pending(ws_idx, pane, true);
            return None;
        }
        let public = self.public_pane_id(ws_idx, pane)?;
        for candidate in model::name_candidates(&slug) {
            if self.team_name_taken(&candidate, Some(&terminal_id)) {
                continue;
            }
            match self.rename_agent_target(&public, Some(candidate.clone())) {
                Ok(_) => {
                    self.team_set_pending(ws_idx, pane, false);
                    self.team_follow_tab_name(ws_idx, pane, old_name.as_deref(), &candidate);
                    let line = match &old_name {
                        Some(old) => format!("{old} is now {candidate}"),
                        None => format!("{public} is now {candidate}"),
                    };
                    if let Some(team) = self.team_mut(ws_idx) {
                        team.record(line);
                    }
                    return Some(true);
                }
                Err(AgentRenameError::DuplicateName { .. }) => continue,
                Err(AgentRenameError::PendingLaunch | AgentRenameError::Suspended(_)) => {
                    self.team_set_pending(ws_idx, pane, true);
                    return None;
                }
                Err(_) => {
                    self.team_set_pending(ws_idx, pane, false);
                    return Some(false);
                }
            }
        }
        tracing::debug!(
            event = "team.rename",
            pane = %public,
            slug = %slug,
            "every name for this role is taken; the name stays"
        );
        self.team_set_pending(ws_idx, pane, false);
        Some(false)
    }

    /// A member's pane and current name, before a user rename of `target`
    /// (`agent.rename`); `None` when it is not a member (or no team exists).
    pub(crate) fn team_member_before_rename(
        &self,
        target: &str,
    ) -> Option<(PaneId, Option<String>)> {
        if self.state.team_count == 0 {
            return None;
        }
        let resolved = self.resolve_agent_target(target).ok()?;
        self.state
            .team_index
            .contains_key(&resolved.pane_id)
            .then(|| {
                (
                    resolved.pane_id,
                    self.team_agent_name(resolved.ws_idx, resolved.pane_id),
                )
            })
    }

    /// A member was renamed outside a role change: teammates address it by
    /// name, so the roster changes (logged, pushed, delivered next turn).
    pub(crate) fn team_follow_agent_rename(&mut self, pane: PaneId, old_name: Option<String>) {
        let Some((ws_idx, _)) = self.find_pane(pane) else {
            return;
        };
        let new_name = self.team_agent_name(ws_idx, pane);
        if new_name == old_name {
            return;
        }
        let public = self.public_pane_id(ws_idx, pane).unwrap_or_default();
        let line = match (old_name, new_name) {
            (Some(old), Some(new)) => format!("{old} is now {new}"),
            (None, Some(new)) => format!("{public} is now {new}"),
            (Some(old), None) => format!("{old} ({public}) has no name now"),
            (None, None) => return,
        };
        let Some(team) = self.team_mut(ws_idx) else {
            return;
        };
        team.record(line);
        self.teams_changed();
    }

    fn team_set_pending(&mut self, ws_idx: usize, pane: PaneId, pending: bool) {
        if let Some(member) = self.team_mut(ws_idx).and_then(|team| team.member_mut(pane)) {
            member.pending_rename = pending;
        }
    }

    /// The tab follows the new name only when it had no custom name or was
    /// named after the old agent name (user-chosen tab names stay).
    fn team_follow_tab_name(
        &mut self,
        ws_idx: usize,
        pane: PaneId,
        old_name: Option<&str>,
        new_name: &str,
    ) {
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane))
        else {
            return;
        };
        let Some(tab) = self.state.workspaces[ws_idx].tabs.get_mut(tab_idx) else {
            return;
        };
        let follows = match tab.custom_name.as_deref() {
            None => true,
            Some(current) => old_name == Some(current) || current.is_empty(),
        };
        if !follows || tab.custom_name.as_deref() == Some(new_name) {
            return;
        }
        tab.set_custom_name(new_name.to_string());
        let (Some(tab_id), workspace_id) = (
            self.public_tab_id(ws_idx, tab_idx),
            self.public_workspace_id(ws_idx),
        ) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::TabRenamed,
            data: EventData::TabRenamed {
                tab_id,
                workspace_id,
                label: new_name.to_string(),
            },
        });
    }

    /// Store a member's role (already sanitized) and apply the name; the
    /// `renamed` reply value.
    fn team_store_role(
        &mut self,
        ws_idx: usize,
        pane: PaneId,
        role: Option<String>,
    ) -> Option<bool> {
        let current = self
            .state
            .workspaces
            .get(ws_idx)?
            .team
            .as_ref()?
            .member(pane)?
            .role
            .clone();
        if current != role {
            let name = self.team_display_name(ws_idx, pane, current.as_deref());
            let team = self.team_mut(ws_idx)?;
            if let Some(member) = team.member_mut(pane) {
                member.role = role.clone();
            }
            team.record(match &role {
                Some(role) => format!("{name}'s role: {role}"),
                None => format!("{name}'s role cleared"),
            });
        }
        // Fork (agents v2): the pane's meta holds the role; the member's is
        // its mirror (an older build reads the mirror back).
        let meta_role = role.clone();
        self.write_meta(ws_idx, pane, |meta| {
            if meta.role != meta_role {
                meta.role_cleared = meta_role.is_none();
                meta.role = meta_role;
            }
        });
        role.as_ref()?;
        self.team_apply_role_name(ws_idx, pane)
    }

    // ----- following panes and agents --------------------------------------

    /// Follow one structural or agent event (only called while a team
    /// exists). O(members) plus a workspace scan.
    pub(crate) fn follow_teams(&mut self, event: &EventEnvelope) {
        #[cfg(test)]
        {
            self.team_follow_calls += 1;
        }
        match &event.data {
            EventData::PaneAgentDetected {
                pane_id, released, ..
            } => self.follow_team_agent(pane_id, *released),
            EventData::PaneAgentStatusChanged { pane_id, .. } => self.follow_team_status(pane_id),
            EventData::PaneMoved {
                previous_workspace_id,
                pane,
                ..
            } => {
                let reconciled = self.reconcile_teams();
                let moved_in = self.follow_team_move(previous_workspace_id, &pane.pane_id);
                if reconciled || moved_in {
                    self.teams_changed();
                } else if self.team_pane_in_team(&pane.pane_id) {
                    // A move within the team: only the public id changed.
                    self.bump_teams_view();
                }
            }
            EventData::PaneClosed { .. }
            | EventData::PaneExited { .. }
            | EventData::TabClosed { .. }
            | EventData::WorkspaceClosed { .. } => {
                if self.reconcile_teams() {
                    self.teams_changed();
                }
            }
            EventData::WorkspaceRenamed { workspace_id, .. } => {
                if self
                    .state
                    .workspaces
                    .iter()
                    .any(|ws| &ws.id == workspace_id && ws.team.is_some())
                {
                    self.bump_teams_view();
                    self.mark_coordinator_input_dirty();
                }
            }
            EventData::TabRenamed { tab_id, .. } => {
                let member_tab = self.parse_tab_id(tab_id).is_some_and(|(ws_idx, tab_idx)| {
                    self.state.workspaces[ws_idx]
                        .tabs
                        .get(tab_idx)
                        .is_some_and(|tab| {
                            tab.panes
                                .keys()
                                .any(|pane| self.state.team_index.contains_key(pane))
                        })
                });
                if member_tab {
                    self.bump_teams_view();
                }
            }
            _ => {}
        }
    }

    fn team_pane_in_team(&self, public_pane: &str) -> bool {
        self.parse_pane_id(public_pane)
            .is_some_and(|(_, pane)| self.state.team_index.contains_key(&pane))
    }

    /// An agent started (or re-took a slot) or went away in a pane.
    fn follow_team_agent(&mut self, public_pane: &str, released: bool) {
        let Some((ws_idx, pane)) = self.parse_pane_id(public_pane) else {
            return;
        };
        let Some(team) = self.state.workspaces[ws_idx].team.as_ref() else {
            return;
        };
        let now = crate::coordinator::now_unix();
        if team.is_member(pane) {
            if let Some(member) = self.team_mut(ws_idx).and_then(|team| team.member_mut(pane)) {
                member.status_since_unix = now;
            }
            if !released {
                // The agent re-takes its slot: the name follows the role.
                let revision = self.team_revision(ws_idx);
                self.team_apply_role_name(ws_idx, pane);
                if self.team_revision(ws_idx) != revision {
                    self.teams_changed();
                    return;
                }
            }
            // The member's agent kind changed (or it went): clients see it.
            self.bump_teams_view();
            self.mark_coordinator_input_dirty();
            return;
        }
        if released
            || team.is_excluded(pane)
            || self.meta_excludes_team(ws_idx, pane)
            || self.is_coordinator_pane(ws_idx, pane)
        {
            return;
        }
        if self.team_add_member(ws_idx, pane, None) {
            tracing::info!(event = "team.join", pane = %public_pane, "agent joined its team group's team");
            self.teams_changed();
        }
    }

    /// Fork (agents v2): the pane's meta names this group's team as one it
    /// left or was removed from.
    fn meta_excludes_team(&self, ws_idx: usize, pane: PaneId) -> bool {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return false;
        };
        self.team_terminal(ws_idx, pane).is_some_and(|terminal| {
            terminal.agent_meta().team_excluded.as_deref() == Some(ws.id.as_str())
        })
    }

    /// Fork (agents v2): clear the pane's exclusion from this group's team.
    fn clear_meta_exclusion(&mut self, ws_idx: usize, pane: PaneId) {
        if self.meta_excludes_team(ws_idx, pane) {
            self.write_meta(ws_idx, pane, |meta| meta.team_excluded = None);
        }
    }

    fn team_revision(&self, ws_idx: usize) -> u64 {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.team.as_ref())
            .map_or(0, |team| team.revision)
    }

    /// A member's status changed: its time in state restarts (no push); a
    /// rename waiting for activation is applied.
    fn follow_team_status(&mut self, public_pane: &str) {
        let Some((ws_idx, pane)) = self.parse_pane_id(public_pane) else {
            return;
        };
        if !self.state.team_index.contains_key(&pane) {
            return;
        }
        let now = crate::coordinator::now_unix();
        let pending = match self.team_mut(ws_idx).and_then(|team| team.member_mut(pane)) {
            Some(member) => {
                member.status_since_unix = now;
                member.pending_rename
            }
            None => false,
        };
        if pending {
            let revision = self.team_revision(ws_idx);
            self.team_apply_role_name(ws_idx, pane);
            if self.team_revision(ws_idx) != revision {
                self.teams_changed();
            }
        }
    }

    /// A pane moved: into a team group with an agent, it joins (moving in
    /// clears a removal). Leaving is the reconcile's job.
    fn follow_team_move(&mut self, previous_workspace_id: &str, public_pane: &str) -> bool {
        let Some((ws_idx, pane)) = self.parse_pane_id(public_pane) else {
            return false;
        };
        let ws = &self.state.workspaces[ws_idx];
        let Some(team) = ws.team.as_ref() else {
            return false;
        };
        if ws.id == previous_workspace_id || team.is_member(pane) {
            return false;
        }
        // Fork (agents v2): moving in clears a removal from this team.
        self.clear_meta_exclusion(ws_idx, pane);
        if !self.team_pane_has_agent(ws_idx, pane) || self.is_coordinator_pane(ws_idx, pane) {
            // It may join later, on detection: moving in cleared a removal.
            if let Some(team) = self.team_mut(ws_idx) {
                let before = team.excluded.len();
                team.excluded.retain(|excluded| *excluded != pane);
                return team.excluded.len() != before;
            }
            return false;
        }
        self.team_add_member(ws_idx, pane, None)
    }

    /// [`Self::reconcile_teams`], publishing any change (a pane removed
    /// without a following event, e.g. after `pane.exited`).
    pub(crate) fn reconcile_teams_and_publish(&mut self) {
        if self.reconcile_teams() {
            self.teams_changed();
        }
    }

    /// Drop members and removals whose pane is no longer in their team's
    /// group, and teams whose group is gone (their members get the disband
    /// line). Whether anything changed.
    pub(crate) fn reconcile_teams(&mut self) -> bool {
        let mut changed = false;
        let now = crate::coordinator::now_unix();
        // Members of teams whose group is gone.
        let gone: Vec<PaneId> = self
            .state
            .team_index
            .iter()
            .filter(|(_, ws_id)| !self.state.workspaces.iter().any(|ws| &ws.id == *ws_id))
            .map(|(pane, _)| *pane)
            .collect();
        for pane in gone {
            changed = true;
            if self.find_pane(pane).is_some() {
                self.team_tombstones.insert(pane, DISBANDED_LINE, now);
            }
        }
        for ws_idx in 0..self.state.workspaces.len() {
            let Some(mut team) = self.state.workspaces[ws_idx].team.take() else {
                continue;
            };
            let dropped = {
                let ws = &self.state.workspaces[ws_idx];
                let before = team.excluded.len();
                let dropped = team.retain_panes(|pane| ws.pane_state(pane).is_some());
                changed |= team.excluded.len() != before;
                dropped
            };
            self.state.workspaces[ws_idx].team = Some(team);
            for member in dropped {
                changed = true;
                let name = self.team_display_name_anywhere(member.pane_id, member.role.as_deref());
                if let Some(team) = self.team_mut(ws_idx) {
                    team.record(format!("{name} left"));
                }
                if self.find_pane(member.pane_id).is_some() {
                    self.team_tombstones
                        .insert(member.pane_id, REMOVED_LINE, now);
                }
            }
        }
        changed |= self.state.rebuild_team_index();
        changed
    }

    // ----- handlers --------------------------------------------------------

    fn team_reply(&self, id: String, ws_idx: Option<usize>, renamed: Option<bool>) -> String {
        encode_success(
            id,
            ResponseResult::TeamReply {
                team: ws_idx.and_then(|ws_idx| self.team_info(ws_idx, true)),
                renamed,
            },
        )
    }

    fn team_error(id: String, err: TeamError) -> String {
        encode_error(id, err.code, err.message)
    }

    /// `team.list`.
    pub(super) fn handle_team_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::TeamList {
                revision: self.state.teams_view_rev,
                teams: self.team_infos(true),
            },
        )
    }

    /// `team.get`.
    pub(super) fn handle_team_get(&mut self, id: String, params: TeamGetParams) -> String {
        let ws_idx = match (&params.workspace_id, &params.caller_pane) {
            (Some(group), _) => self.team_group_or_error(group),
            (None, Some(caller)) => self.team_pane_or_error(caller).map(|(ws_idx, _)| ws_idx),
            (None, None) => Err(TeamError::new(
                error_code::INVALID_PARAMS,
                "team.get needs workspace_id or caller_pane",
            )),
        };
        match ws_idx {
            Ok(ws_idx) => self.team_reply(id, Some(ws_idx), None),
            Err(err) => Self::team_error(id, err),
        }
    }

    /// `team.make`.
    pub(super) fn handle_team_make(&mut self, id: String, params: TeamMakeParams) -> String {
        match self.team_make(&params) {
            Ok(ws_idx) => self.team_reply(id, Some(ws_idx), None),
            Err(err) => Self::team_error(id, err),
        }
    }

    fn team_make(&mut self, params: &TeamMakeParams) -> TeamResult<usize> {
        let ws_idx = self.team_group_or_error(&params.workspace_id)?;
        if ws_idx == 0 {
            return Err(TeamError::new(
                error_code::FIRST_SPACE,
                "the first space (the ungrouped tabs) cannot be a team",
            ));
        }
        if self.state.workspaces[ws_idx].team.is_some() {
            return Err(TeamError::new(
                error_code::TEAM_EXISTS,
                format!("group {} is already a team", params.workspace_id),
            ));
        }
        let actor = self.team_actor(params.caller_pane.as_deref())?;
        let purpose = params.purpose.as_deref().and_then(model::sanitize_purpose);
        let now = crate::coordinator::now_unix();
        let mut team = Team::new(purpose, Some(actor), now);
        let ws = &self.state.workspaces[ws_idx];
        let candidates: Vec<PaneId> = ws
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.pane_ids())
            .collect();
        for pane in candidates {
            if self.team_pane_has_agent(ws_idx, pane) && !self.is_coordinator_pane(ws_idx, pane) {
                // Fork (agents v2): the pane's role comes along.
                let role = self
                    .team_terminal(ws_idx, pane)
                    .and_then(|terminal| terminal.agent_meta().role.clone());
                team.join(pane, role, now);
                self.team_tombstones.clear(pane);
            }
        }
        // Fork (agents v2): making the team clears the panes' exclusions.
        let panes: Vec<PaneId> = self.state.workspaces[ws_idx]
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.pane_ids())
            .collect();
        for pane in panes {
            self.clear_meta_exclusion(ws_idx, pane);
        }
        tracing::info!(
            event = "team.make",
            workspace = %self.state.workspaces[ws_idx].id,
            members = team.members.len(),
            "team made"
        );
        self.state.workspaces[ws_idx].team = Some(team);
        self.teams_changed();
        Ok(ws_idx)
    }

    /// `team.disband`.
    pub(super) fn handle_team_disband(
        &mut self,
        id: String,
        params: TeamWorkspaceParams,
    ) -> String {
        let ws_idx = match self.team_group_or_error(&params.workspace_id) {
            Ok(ws_idx) => ws_idx,
            Err(err) => return Self::team_error(id, err),
        };
        let Some(team) = self.state.workspaces[ws_idx].team.take() else {
            return Self::team_error(
                id,
                TeamError::new(
                    error_code::NO_TEAM,
                    format!("group {} is not a team", params.workspace_id),
                ),
            );
        };
        let now = crate::coordinator::now_unix();
        for member in &team.members {
            self.team_tombstones
                .insert(member.pane_id, DISBANDED_LINE, now);
        }
        tracing::info!(
            event = "team.disband",
            workspace = %self.state.workspaces[ws_idx].id,
            "team disbanded"
        );
        self.teams_changed();
        self.team_reply(id, None, None)
    }

    /// `team.set_purpose`.
    pub(super) fn handle_team_set_purpose(
        &mut self,
        id: String,
        params: TeamSetPurposeParams,
    ) -> String {
        let result = (|| {
            let ws_idx = self.team_group_or_error(&params.workspace_id)?;
            if self.state.workspaces[ws_idx].team.is_none() {
                return Err(TeamError::new(
                    error_code::NO_TEAM,
                    format!("group {} is not a team", params.workspace_id),
                ));
            }
            let actor = self.team_actor(params.caller_pane.as_deref())?;
            let purpose = params.purpose.as_deref().and_then(model::sanitize_purpose);
            let changed = self
                .team_mut(ws_idx)
                .is_some_and(|team| team.set_purpose(purpose, actor));
            if changed {
                self.teams_changed();
            }
            Ok(ws_idx)
        })();
        match result {
            Ok(ws_idx) => self.team_reply(id, Some(ws_idx), None),
            Err(err) => Self::team_error(id, err),
        }
    }

    fn team_member_or_error(&self, pane: &str) -> TeamResult<(usize, PaneId)> {
        let (ws_idx, raw) = self.team_pane_or_error(pane)?;
        let member = self.state.workspaces[ws_idx]
            .team
            .as_ref()
            .is_some_and(|team| team.is_member(raw));
        if !member {
            return Err(TeamError::new(
                error_code::NOT_A_MEMBER,
                format!("pane {pane} is not a team member"),
            ));
        }
        Ok((ws_idx, raw))
    }

    /// `team.set_role`.
    pub(super) fn handle_team_set_role(&mut self, id: String, params: TeamSetRoleParams) -> String {
        let (ws_idx, pane) = match self.team_member_or_error(&params.pane_id) {
            Ok(found) => found,
            Err(err) => return Self::team_error(id, err),
        };
        if let Some(caller) = params.caller_pane.as_deref() {
            if let Err(err) = self.team_actor(Some(caller)) {
                return Self::team_error(id, err);
            }
        }
        let role = params.role.as_deref().and_then(model::sanitize_role);
        let revision = self.team_revision(ws_idx);
        let renamed = self.team_store_role(ws_idx, pane, role);
        if self.team_revision(ws_idx) != revision {
            self.teams_changed();
        }
        self.team_reply(id, Some(ws_idx), renamed)
    }

    /// `team.join`.
    pub(super) fn handle_team_join(&mut self, id: String, params: TeamJoinParams) -> String {
        match self.team_join(&params) {
            Ok((ws_idx, renamed)) => self.team_reply(id, Some(ws_idx), renamed),
            Err(err) => Self::team_error(id, err),
        }
    }

    fn team_join(&mut self, params: &TeamJoinParams) -> TeamResult<(usize, Option<bool>)> {
        let (ws_idx, pane) = self.team_pane_or_error(&params.pane_id)?;
        if self.state.workspaces[ws_idx].team.is_none() {
            return Err(TeamError::new(
                error_code::NOT_IN_TEAM_GROUP,
                format!("pane {} is not in a team group", params.pane_id),
            ));
        }
        if self.is_coordinator_pane(ws_idx, pane) {
            return Err(TeamError::new(
                error_code::COORDINATOR_PANE,
                "the coordinator never joins a team",
            ));
        }
        let role = params.role.as_deref().and_then(model::sanitize_role);
        if role.is_none() && !self.team_pane_has_agent(ws_idx, pane) {
            return Err(TeamError::new(
                error_code::NO_AGENT,
                format!(
                    "pane {} has no agent (give a role to join before it starts)",
                    params.pane_id
                ),
            ));
        }
        // Fork (agents v2): an explicit join clears the pane's exclusion.
        self.clear_meta_exclusion(ws_idx, pane);
        let revision = self.team_revision(ws_idx);
        let joined = self.team_add_member(ws_idx, pane, None);
        if !joined {
            // Already a member: joining clears nothing else.
            if let Some(team) = self.team_mut(ws_idx) {
                team.excluded.retain(|excluded| *excluded != pane);
            }
        }
        let renamed = match role {
            Some(role) => self.team_store_role(ws_idx, pane, Some(role)),
            None => None,
        };
        if joined || self.team_revision(ws_idx) != revision {
            self.teams_changed();
        }
        Ok((ws_idx, renamed))
    }

    /// `team.leave`.
    pub(super) fn handle_team_leave(&mut self, id: String, params: TeamPaneParams) -> String {
        let (ws_idx, pane) = match self.team_member_or_error(&params.pane_id) {
            Ok(found) => found,
            Err(err) => return Self::team_error(id, err),
        };
        if self.team_remove_member(ws_idx, pane, true) {
            self.teams_changed();
        }
        self.team_reply(id, Some(ws_idx), None)
    }

    /// `team.context`: read-only except `ack`, which never bumps the view
    /// revision, saves or asks for a render.
    pub(super) fn handle_team_context(&mut self, id: String, params: TeamContextParams) -> String {
        let Some((ws_idx, pane)) = self.resolve_caller_pane(&params.caller_pane) else {
            return encode_error(
                id,
                error_code::PANE_NOT_FOUND,
                format!("pane {} not found", params.caller_pane),
            );
        };
        let now = crate::coordinator::now_unix();
        let team_info = self.team_info(ws_idx, true);
        let (eligible, is_member, seen, revision, team_key) =
            match self.state.workspaces[ws_idx].team.as_ref() {
                Some(team) => {
                    let member = team.member(pane);
                    (
                        !team.is_excluded(pane) && !self.is_coordinator_pane(ws_idx, pane),
                        member.is_some(),
                        member.map_or(0, |member| member.seen_revision()),
                        team.revision,
                        Some(team_ack_key(team)),
                    )
                }
                None => (false, false, 0, 0, None),
            };
        let member = team_info.as_ref().and_then(|info| {
            let public = self.public_pane_id(ws_idx, pane)?;
            info.members
                .iter()
                .find(|member| member.pane_id == public)
                .cloned()
        });
        let mut tombstone_key = None;
        let text = if is_member {
            let behind = seen < revision;
            if params.full || behind {
                let changes = (!params.full)
                    .then(|| {
                        self.state.workspaces[ws_idx]
                            .team
                            .as_ref()
                            .and_then(|team| team.changes_since(seen))
                            .map(|lines| lines.into_iter().map(str::to_string).collect::<Vec<_>>())
                    })
                    .flatten();
                team_info
                    .as_ref()
                    .map(|info| self.team_text(info, ws_idx, pane, changes.as_deref()))
            } else {
                None
            }
        } else if eligible && params.full {
            team_info
                .as_ref()
                .map(|info| self.team_text(info, ws_idx, pane, None))
        } else if team_info.is_none() || !eligible {
            self.team_tombstones.peek(pane, now).map(|(line, key)| {
                tombstone_key = Some(key);
                line.to_string()
            })
        } else {
            None
        };
        // What `text` comes from; an ack naming anything else is stale
        // (a disband, a re-make or a move since the read) and is ignored.
        let ack_key = if is_member { team_key } else { tombstone_key };
        let ack_applies = params
            .ack_key
            .as_ref()
            .is_none_or(|key| ack_key.as_ref() == Some(key));
        if params.ack && ack_applies {
            if is_member {
                if let Some(team) = self.team_mut(ws_idx) {
                    team.mark_seen_up_to(pane, params.ack_revision);
                }
            } else if text.is_some() {
                self.team_tombstones.clear(pane);
            }
        }
        encode_success(
            id,
            ResponseResult::TeamContext {
                member,
                eligible,
                team: team_info,
                text,
                revision,
                ack_key,
            },
        )
    }

    /// The roster text for `pane` (full when `changes` is `None`).
    fn team_text(
        &self,
        info: &TeamInfo,
        ws_idx: usize,
        pane: PaneId,
        changes: Option<&[String]>,
    ) -> String {
        let you_public = self.public_pane_id(ws_idx, pane).unwrap_or_default();
        let purpose_by = info.purpose_by.as_ref().map(TeamActor::describe);
        let status_of = |member: &TeamMemberInfo| match member.status {
            Some(status) => status_word(status),
            None => "no agent",
        };
        let as_text = |member: &TeamMemberInfo| -> TeamTextMemberOwned {
            TeamTextMemberOwned {
                name: member
                    .name
                    .clone()
                    .or_else(|| member.role.clone())
                    .or_else(|| member.agent.clone())
                    .unwrap_or_else(|| "agent".into()),
                role: member.role.clone(),
                agent: member.agent.clone(),
                pane: member.pane_id.clone(),
                status: status_of(member).to_string(),
            }
        };
        let you = info
            .members
            .iter()
            .find(|member| member.pane_id == you_public)
            .map(as_text)
            .unwrap_or_else(|| TeamTextMemberOwned {
                name: self
                    .team_agent_name(ws_idx, pane)
                    .unwrap_or_else(|| "agent".into()),
                role: None,
                agent: self.team_agent_kind(ws_idx, pane),
                pane: you_public.clone(),
                status: "idle".into(),
            });
        let others: Vec<TeamTextMemberOwned> = info
            .members
            .iter()
            .filter(|member| member.pane_id != you_public)
            .map(as_text)
            .collect();
        render_team_text(
            &info.workspace_label,
            info.purpose.as_deref(),
            purpose_by.as_deref(),
            &you,
            &others,
            changes,
        )
    }
}

/// A roster line's member, owned (the text functions borrow it).
struct TeamTextMemberOwned {
    name: String,
    role: Option<String>,
    agent: Option<String>,
    pane: String,
    status: String,
}

fn status_word(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
        AgentStatus::Suspended => "suspended",
    }
}

fn text_member(m: &TeamTextMemberOwned) -> crate::agent_wrap::team::TeamTextMember<'_> {
    crate::agent_wrap::team::TeamTextMember {
        name: m.name.as_str(),
        role: m.role.as_deref(),
        agent: m.agent.as_deref(),
        pane: m.pane.as_str(),
        status: m.status.as_str(),
    }
}

/// The member- and delta-text renderer shared with the agent wrap.
fn render_team_text(
    group_label: &str,
    purpose: Option<&str>,
    purpose_by: Option<&str>,
    you: &TeamTextMemberOwned,
    others: &[TeamTextMemberOwned],
    changes: Option<&[String]>,
) -> String {
    let input = crate::agent_wrap::team::TeamTextInput {
        group_label,
        purpose,
        purpose_by,
        you: text_member(you),
        others: others.iter().map(text_member).collect(),
    };
    match changes {
        Some(changes) => crate::agent_wrap::team::delta_text(&input, changes),
        None => crate::agent_wrap::team::full_text(&input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        EmptyParams, Method, PaneMoveDestination, PaneMoveParams, Request, TabRenameParams,
        WorkspaceCloseParams,
    };
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    /// The ungrouped bucket (`w1`) and a group `demo` with three tabs.
    fn team_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut group = Workspace::test_new("demo");
        group.test_add_tab(None);
        group.test_add_tab(Some("notes"));
        app.state.workspaces = vec![Workspace::test_new("bucket"), group];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        app
    }

    fn pane(app: &App, ws_idx: usize, tab_idx: usize) -> PaneId {
        app.state.workspaces[ws_idx].tabs[tab_idx].root_pane
    }

    fn public(app: &App, pane: PaneId) -> String {
        let (ws_idx, _) = app.find_pane(pane).expect("pane exists");
        app.public_pane_id(ws_idx, pane).expect("public id")
    }

    fn group_id(app: &App) -> String {
        app.state.workspaces[1].id.clone()
    }

    fn terminal_mut(app: &mut App, pane: PaneId) -> &mut crate::terminal::TerminalState {
        let (ws_idx, _) = app.find_pane(pane).expect("pane exists");
        let id = app.state.workspaces[ws_idx]
            .pane_state(pane)
            .expect("pane state")
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&id).expect("terminal")
    }

    /// An agent starts (`Some`) or goes (`None`) in a pane, as detection reports it.
    fn detect(app: &mut App, pane: PaneId, agent: Option<Agent>) {
        terminal_mut(app, pane).set_detected_state(agent, AgentState::Idle);
        let (ws_idx, _) = app.find_pane(pane).expect("pane exists");
        app.emit_event(EventEnvelope {
            event: EventKind::PaneAgentDetected,
            data: EventData::PaneAgentDetected {
                pane_id: public(app, pane),
                workspace_id: app.public_workspace_id(ws_idx),
                agent: agent.map(|agent| crate::detect::agent_label(agent).to_string()),
                released: agent.is_none(),
                final_status: None,
            },
        });
    }

    fn call(app: &mut App, method: Method) -> serde_json::Value {
        let response = app.handle_api_request(Request {
            id: "team-test".into(),
            method,
        });
        serde_json::from_str(&response).expect("json response")
    }

    fn make(app: &mut App, purpose: Option<&str>) -> serde_json::Value {
        let workspace_id = group_id(app);
        call(
            app,
            Method::TeamMake(TeamMakeParams {
                workspace_id,
                purpose: purpose.map(str::to_string),
                caller_pane: None,
            }),
        )
    }

    fn context(app: &mut App, pane: PaneId, ack: bool, full: bool) -> serde_json::Value {
        let caller_pane = public(app, pane);
        call(
            app,
            Method::TeamContext(TeamContextParams {
                caller_pane,
                ack,
                full,
                ack_revision: None,
                ack_key: None,
            }),
        )["result"]
            .clone()
    }

    fn set_role(app: &mut App, pane: PaneId, role: Option<&str>) -> serde_json::Value {
        let pane_id = public(app, pane);
        call(
            app,
            Method::TeamSetRole(TeamSetRoleParams {
                pane_id,
                role: role.map(str::to_string),
                caller_pane: None,
            }),
        )
    }

    fn members(app: &App) -> Vec<PaneId> {
        app.state.workspaces[1]
            .team
            .as_ref()
            .map(|team| team.members.iter().map(|member| member.pane_id).collect())
            .unwrap_or_default()
    }

    #[test]
    fn groups_past_the_last_never_panic_and_labels_win_over_numbers() {
        let mut app = team_app();
        // Two groups: positional "9" and "w_9" name nothing.
        for group in ["9", "w_9"] {
            let made = call(
                &mut app,
                Method::TeamMake(TeamMakeParams {
                    workspace_id: group.into(),
                    ..TeamMakeParams::default()
                }),
            );
            assert_eq!(made["error"]["code"], "workspace_not_found", "{made}");
            let disbanded = call(
                &mut app,
                Method::TeamDisband(TeamWorkspaceParams {
                    workspace_id: group.into(),
                }),
            );
            assert_eq!(disbanded["error"]["code"], "workspace_not_found");
            let purpose = call(
                &mut app,
                Method::TeamSetPurpose(TeamSetPurposeParams {
                    workspace_id: group.into(),
                    purpose: Some("x".into()),
                    caller_pane: None,
                }),
            );
            assert_eq!(purpose["error"]["code"], "workspace_not_found");
        }
        // A numeric label is a label, not index 2023.
        app.state.workspaces[1].custom_name = Some("2024".into());
        let made = call(
            &mut app,
            Method::TeamMake(TeamMakeParams {
                workspace_id: "2024".into(),
                ..TeamMakeParams::default()
            }),
        );
        assert_eq!(made["result"]["type"], "team_reply", "{made}");
        assert!(app.state.workspaces[1].team.is_some());
        // The sidebar number still works when no label claims it.
        let purpose = call(
            &mut app,
            Method::TeamSetPurpose(TeamSetPurposeParams {
                workspace_id: "2".into(),
                purpose: Some("ship it".into()),
                caller_pane: None,
            }),
        );
        assert_eq!(purpose["result"]["team"]["purpose"], "ship it", "{purpose}");
    }

    #[test]
    fn make_joins_the_groups_agents_and_refuses_bad_groups() {
        let mut app = team_app();
        let (a, b, c) = (pane(&app, 1, 0), pane(&app, 1, 1), pane(&app, 1, 2));
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal_mut(&mut app, c).set_detected_state(Some(Agent::Codex), AgentState::Idle);
        // The coordinator's pane never joins.
        let coordinator_terminal = app.state.workspaces[1]
            .pane_state(c)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state.coordinator_terminal_id = Some(coordinator_terminal);

        let bucket = app.state.workspaces[0].id.clone();
        let first = call(
            &mut app,
            Method::TeamMake(TeamMakeParams {
                workspace_id: bucket,
                ..TeamMakeParams::default()
            }),
        );
        assert_eq!(first["error"]["code"], "first_space", "{first}");
        let missing = call(
            &mut app,
            Method::TeamMake(TeamMakeParams {
                workspace_id: "w404".into(),
                ..TeamMakeParams::default()
            }),
        );
        assert_eq!(missing["error"]["code"], "workspace_not_found");

        let made = make(&mut app, Some("  fix the\n demo greeting "));
        assert_eq!(made["result"]["type"], "team_reply", "{made}");
        let team = &made["result"]["team"];
        assert_eq!(team["purpose"], "fix the demo greeting");
        assert_eq!(team["purpose_by"]["kind"], "user");
        assert_eq!(team["workspace_label"], "demo");
        assert_eq!(members(&app), vec![a], "only agents, never the coordinator");
        assert_eq!(team["members"][0]["agent"], "claude");
        assert_eq!(team["members"][0]["status"], "idle");
        assert!(b != a);
        assert_eq!(app.state.team_count, 1);
        assert!(app.state.teams_view_rev > 0);
        app.state.assert_invariants_for_test();

        let again = make(&mut app, None);
        assert_eq!(again["error"]["code"], "team_exists");
        // The label resolves too.
        let by_label = call(
            &mut app,
            Method::TeamGet(TeamGetParams {
                workspace_id: Some("demo".into()),
                caller_pane: None,
            }),
        );
        assert_eq!(by_label["result"]["team"]["workspace_id"], group_id(&app));
        let listed = call(&mut app, Method::TeamList(EmptyParams::default()));
        assert_eq!(listed["result"]["teams"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn agents_join_on_detection_and_keep_their_slot_when_they_exit() {
        let mut app = team_app();
        make(&mut app, Some("ship"));
        assert!(members(&app).is_empty());
        let (a, b) = (pane(&app, 1, 0), pane(&app, 1, 1));
        let shell = pane(&app, 0, 0);

        detect(&mut app, a, Some(Agent::Claude));
        assert_eq!(members(&app), vec![a]);
        // Outside the team group nothing happens.
        detect(&mut app, shell, Some(Agent::Claude));
        assert_eq!(app.state.team_index.len(), 1);

        set_role(&mut app, a, Some("Code Reviewer"));
        assert_eq!(
            terminal_mut(&mut app, a).agent_name.as_deref(),
            Some("code-reviewer")
        );
        // The agent exits: the slot and the role stay, the agent is gone.
        detect(&mut app, a, None);
        assert_eq!(members(&app), vec![a]);
        let workspace_id = Some(group_id(&app));
        let got = call(
            &mut app,
            Method::TeamGet(TeamGetParams {
                workspace_id,
                caller_pane: None,
            }),
        );
        let member = &got["result"]["team"]["members"][0];
        assert_eq!(member["role"], "Code Reviewer");
        assert!(member.get("agent").is_none() && member.get("status").is_none());
        // A new agent re-takes the slot.
        detect(&mut app, a, Some(Agent::Codex));
        assert_eq!(members(&app), vec![a]);
        detect(&mut app, b, Some(Agent::Claude));
        assert_eq!(members(&app), vec![a, b]);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn removal_blocks_auto_join_until_join_or_a_move_in() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        make(&mut app, None);
        let pane_id = public(&app, a);
        let left = call(
            &mut app,
            Method::TeamLeave(TeamPaneParams {
                pane_id: pane_id.clone(),
            }),
        );
        assert_eq!(
            left["result"]["team"]["excluded"][0],
            pane_id.as_str(),
            "{left}"
        );
        assert!(members(&app).is_empty());
        let not_member = call(
            &mut app,
            Method::TeamLeave(TeamPaneParams {
                pane_id: pane_id.clone(),
            }),
        );
        assert_eq!(not_member["error"]["code"], "not_a_member");
        // A restarted agent does not rejoin a removed pane.
        detect(&mut app, a, Some(Agent::Codex));
        assert!(members(&app).is_empty());
        // The removed pane is told once.
        let told = context(&mut app, a, true, false);
        assert_eq!(told["text"], REMOVED_LINE);
        assert_eq!(told["eligible"], false);
        assert!(context(&mut app, a, true, false)["text"].is_null());
        // An explicit join clears the removal.
        let joined = call(
            &mut app,
            Method::TeamJoin(TeamJoinParams {
                pane_id,
                role: None,
            }),
        );
        assert_eq!(joined["result"]["team"]["excluded"], serde_json::json!([]));
        assert_eq!(members(&app), vec![a]);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn join_refuses_outside_team_groups_the_coordinator_and_agentless_panes() {
        let mut app = team_app();
        make(&mut app, None);
        let shell = pane(&app, 0, 0);
        let (a, c) = (pane(&app, 1, 0), pane(&app, 1, 2));
        let join = |app: &mut App, pane: PaneId, role: Option<&str>| {
            let pane_id = public(app, pane);
            call(
                app,
                Method::TeamJoin(TeamJoinParams {
                    pane_id,
                    role: role.map(str::to_string),
                }),
            )
        };
        assert_eq!(
            join(&mut app, shell, None)["error"]["code"],
            "not_in_team_group"
        );
        assert_eq!(join(&mut app, a, None)["error"]["code"], "no_agent");
        let coordinator_terminal = app.state.workspaces[1]
            .pane_state(c)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state.coordinator_terminal_id = Some(coordinator_terminal);
        assert_eq!(
            join(&mut app, c, Some("x"))["error"]["code"],
            "coordinator_pane"
        );
        // A pre-launch join with a role: the rename waits for the agent.
        let pre = join(&mut app, a, Some("fixer"));
        assert_eq!(
            pre["result"]["team"]["members"][0]["role"], "fixer",
            "{pre}"
        );
        assert!(pre["result"].get("renamed").is_none(), "deferred: {pre}");
        let member = app.state.workspaces[1]
            .team
            .as_ref()
            .unwrap()
            .member(a)
            .unwrap();
        assert!(member.pending_rename());
        // The managed launch starts it under the role's name: no rename.
        terminal_mut(&mut app, a).set_agent_name("fixer".into());
        detect(&mut app, a, Some(Agent::Claude));
        let member = app.state.workspaces[1]
            .team
            .as_ref()
            .unwrap()
            .member(a)
            .unwrap();
        assert!(!member.pending_rename());
        assert_eq!(
            terminal_mut(&mut app, a).agent_name.as_deref(),
            Some("fixer")
        );
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn roles_rename_agents_with_session_wide_suffixes_and_follow_tab_rules() {
        let mut app = team_app();
        let (a, b, c) = (pane(&app, 1, 0), pane(&app, 1, 1), pane(&app, 1, 2));
        for pane in [a, b, c] {
            terminal_mut(&mut app, pane).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        }
        // `reviewer` is taken outside the team (session-wide).
        let outside = pane(&app, 0, 0);
        terminal_mut(&mut app, outside).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal_mut(&mut app, outside).set_agent_name("reviewer".into());
        make(&mut app, None);

        let renamed = set_role(&mut app, a, Some("reviewer"));
        assert_eq!(renamed["result"]["renamed"], true, "{renamed}");
        assert_eq!(
            terminal_mut(&mut app, a).agent_name.as_deref(),
            Some("reviewer-2")
        );
        // The auto-named tab follows; the user's tab name (`notes`) stays.
        assert_eq!(
            app.state.workspaces[1].tabs[0].custom_name.as_deref(),
            Some("reviewer-2")
        );
        set_role(&mut app, c, Some("reviewer"));
        assert_eq!(
            terminal_mut(&mut app, c).agent_name.as_deref(),
            Some("reviewer-3")
        );
        assert_eq!(
            app.state.workspaces[1].tabs[2].custom_name.as_deref(),
            Some("notes")
        );
        // Same role again: no change, no revision.
        let revision = app.team_revision(1);
        set_role(&mut app, a, Some("reviewer"));
        assert_eq!(app.team_revision(1), revision);

        // Exhausted: every candidate is taken, the role is stored, the name stays.
        for name in crate::workspace::team::name_candidates("fixer") {
            let mut terminal = crate::terminal::TerminalState::new(
                crate::terminal::TerminalId::alloc(),
                std::path::PathBuf::from("/tmp"),
            );
            terminal.set_agent_name(name);
            app.state.terminals.insert(terminal.id.clone(), terminal);
        }
        let kept = set_role(&mut app, b, Some("fixer"));
        assert_eq!(kept["result"]["renamed"], false, "{kept}");
        assert_eq!(terminal_mut(&mut app, b).agent_name, None);
        let team = app.state.workspaces[1].team.as_ref().unwrap();
        assert_eq!(team.member(b).unwrap().role.as_deref(), Some("fixer"));
        // Clearing a role keeps the name.
        set_role(&mut app, a, None);
        assert_eq!(
            terminal_mut(&mut app, a).agent_name.as_deref(),
            Some("reviewer-2")
        );
        let not_member = set_role(&mut app, outside, Some("x"));
        assert_eq!(not_member["error"]["code"], "not_a_member");
    }

    #[test]
    fn a_tab_named_after_the_old_agent_name_follows_the_rename() {
        let mut app = team_app();
        let notes = pane(&app, 1, 2);
        terminal_mut(&mut app, notes).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal_mut(&mut app, notes).set_agent_name("notes".into());
        make(&mut app, None);
        set_role(&mut app, notes, Some("tester"));
        assert_eq!(
            app.state.workspaces[1].tabs[2].custom_name.as_deref(),
            Some("tester")
        );
        let team = app.state.workspaces[1].team.as_ref().unwrap();
        assert_eq!(
            team.changes_since(team.revision - 1).unwrap(),
            vec!["notes is now tester"]
        );
    }

    #[test]
    fn moves_out_in_and_within_follow_the_previous_workspace() {
        let mut app = team_app();
        let (a, b) = (pane(&app, 1, 0), pane(&app, 1, 1));
        let shell = pane(&app, 0, 0);
        for pane in [a, b, shell] {
            terminal_mut(&mut app, pane).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        }
        make(&mut app, None);
        set_role(&mut app, b, Some("reviewer"));
        assert_eq!(members(&app), vec![a, b]);
        let bucket = app.state.workspaces[0].id.clone();
        let group = group_id(&app);

        // Out: the member leaves and its role goes.
        let pane_id = public(&app, b);
        let out = call(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(bucket.clone()),
                    label: None,
                },
                focus: false,
            }),
        );
        assert!(out.get("result").is_some(), "{out}");
        assert_eq!(members(&app), vec![a]);
        assert!(!app.state.team_index.contains_key(&b));
        app.state.assert_invariants_for_test();

        // In: an agent pane joins, with a new public id and no role.
        let before = app.state.teams_view_rev;
        let pane_id = public(&app, shell);
        call(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(group.clone()),
                    label: None,
                },
                focus: false,
            }),
        );
        assert_eq!(members(&app), vec![a, shell]);
        assert!(app.state.teams_view_rev > before);
        let team = app.team_info(1, false).unwrap();
        assert_eq!(team.members[1].pane_id, public(&app, shell));
        assert!(team.members[1].role.is_none());

        // Within: only the public id changes, re-projected on the next push.
        let before = app.state.teams_view_rev;
        let revision = app.team_revision(1);
        let old_public = public(&app, a);
        call(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id: old_public.clone(),
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(group.clone()),
                    label: None,
                },
                focus: false,
            }),
        );
        assert_eq!(members(&app), vec![a, shell]);
        assert_eq!(app.team_revision(1), revision, "no roster change");
        assert!(app.state.teams_view_rev > before, "the public id changed");
        let team = app.team_info(1, false).unwrap();
        assert_eq!(team.members[0].pane_id, public(&app, a));
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn dragging_the_last_tab_out_drops_the_team_and_tells_the_member_once() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        // A one-tab group.
        app.state.workspaces[1].tabs.truncate(1);
        app.state.workspaces[1]
            .public_pane_numbers
            .retain(|pane, _| *pane == a);
        app.state.workspaces[1].active_tab = 0;
        make(&mut app, Some("p"));
        let bucket = app.state.workspaces[0].id.clone();
        let pane_id = public(&app, a);
        call(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some(bucket),
                    label: None,
                },
                focus: false,
            }),
        );
        assert_eq!(app.state.workspaces.len(), 1, "the group closed");
        assert_eq!(app.state.team_count, 0);
        assert!(app.state.team_index.is_empty());
        let told = context(&mut app, a, true, false);
        assert_eq!(told["text"], DISBANDED_LINE, "{told}");
        assert!(told["team"].is_null());
        assert!(context(&mut app, a, true, false)["text"].is_null(), "once");
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn closing_the_group_drops_the_team() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        make(&mut app, None);
        let workspace_id = group_id(&app);
        let closed = call(
            &mut app,
            Method::WorkspaceClose(WorkspaceCloseParams {
                workspace_id,
                close_group: false,
            }),
        );
        assert!(closed.get("result").is_some(), "{closed}");
        assert_eq!(app.state.team_count, 0);
        assert!(app.state.team_index.is_empty());
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn context_gives_full_then_deltas_once_across_channels() {
        let mut app = team_app();
        let (a, b, c) = (pane(&app, 1, 0), pane(&app, 1, 1), pane(&app, 1, 2));
        make(&mut app, Some("fix sync"));
        // Not yet detected: eligible, and the wrap gets the full block.
        let early = context(&mut app, a, false, true);
        assert_eq!(early["eligible"], true, "{early}");
        assert!(early["member"].is_null());
        assert!(
            early["text"].as_str().unwrap().contains("fix sync"),
            "{early}"
        );

        detect(&mut app, a, Some(Agent::Claude));
        detect(&mut app, b, Some(Agent::Claude));
        set_role(&mut app, a, Some("fixer"));
        // First turn: a member that was never told gets everything it missed.
        let first = context(&mut app, a, true, false);
        assert!(first["text"].is_string(), "{first}");
        assert_eq!(first["member"]["role"], "fixer");
        // Told: nothing until something changes.
        assert!(context(&mut app, a, true, false)["text"].is_null());

        // A change reaches `a` once, whichever channel asks first (the
        // Claude hook and the MCP header share the counter); the ack never
        // bumps the view revision.
        let view = app.state.teams_view_rev;
        set_role(&mut app, b, Some("reviewer"));
        let view_after_role = app.state.teams_view_rev;
        assert!(view_after_role > view);
        let peek = context(&mut app, a, false, false);
        let delta = peek["text"].as_str().unwrap().to_string();
        assert!(delta.starts_with("[herdr+ team update]"), "{delta}");
        assert!(delta.contains("reviewer"), "{delta}");
        let hook = context(&mut app, a, true, false);
        assert_eq!(hook["text"].as_str(), Some(delta.as_str()));
        let mcp = context(&mut app, a, true, false);
        assert!(mcp["text"].is_null(), "delivered once: {mcp}");
        assert_eq!(
            app.state.teams_view_rev, view_after_role,
            "ack is not a view change"
        );
        // `full` always gives the roster.
        let full = context(&mut app, a, false, true);
        assert!(full["text"].as_str().unwrap().contains("fix sync"));

        // More than 16 changes behind: the full block instead of a delta.
        for n in 0..20 {
            let purpose = format!("purpose {n}");
            let workspace_id = group_id(&app);
            call(
                &mut app,
                Method::TeamSetPurpose(TeamSetPurposeParams {
                    workspace_id,
                    purpose: Some(purpose),
                    caller_pane: None,
                }),
            );
        }
        let behind = context(&mut app, a, true, false);
        let text = behind["text"].as_str().unwrap();
        assert!(!text.starts_with("[herdr+ team update]"), "{text}");
        assert!(text.contains("purpose 19"), "{text}");
        // Outside a team group: not eligible, no text.
        let shell = pane(&app, 0, 0);
        let outside = context(&mut app, shell, true, true);
        assert_eq!(outside["eligible"], false);
        assert!(outside["text"].is_null() && outside["team"].is_null());
        let _ = c;
    }

    #[test]
    fn a_bounded_ack_keeps_a_later_change_and_a_moved_panes_old_id_still_resolves() {
        let mut app = team_app();
        let (a, b) = (pane(&app, 1, 0), pane(&app, 1, 1));
        detect(&mut app, a, Some(Agent::Claude));
        detect(&mut app, b, Some(Agent::Claude));
        make(&mut app, Some("fix sync"));
        context(&mut app, a, true, false);

        // The MCP reads a change, a second one lands, then it acks what it read.
        set_role(&mut app, b, Some("reviewer"));
        let read = context(&mut app, a, false, false);
        let read_at = read["revision"].as_u64().unwrap();
        set_role(&mut app, b, Some("tester"));
        let caller_pane = public(&app, a);
        call(
            &mut app,
            Method::TeamContext(TeamContextParams {
                caller_pane,
                ack: true,
                full: false,
                ack_revision: Some(read_at),
                ack_key: None,
            }),
        );
        let next = context(&mut app, a, true, false);
        let text = next["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("tester"),
            "the later change still arrives: {next}"
        );
        assert!(
            !text.contains("role: reviewer"),
            "the read one is not repeated: {text}"
        );

        // An agent keeps its launch-time HERDR_PANE_ID across moves: the old
        // public id still resolves to the pane (alias), in and out of the group.
        let launch_id = public(&app, b);
        let bucket = app.state.workspaces[0].id.clone();
        let group = group_id(&app);
        for workspace_id in [bucket, group] {
            let pane_id = public(&app, b);
            call(
                &mut app,
                Method::PaneMove(PaneMoveParams {
                    pane_id,
                    destination: PaneMoveDestination::NewTab {
                        workspace_id: Some(workspace_id),
                        label: None,
                    },
                    focus: false,
                }),
            );
        }
        assert_ne!(public(&app, b), launch_id);
        let moved = call(
            &mut app,
            Method::TeamContext(TeamContextParams {
                caller_pane: launch_id,
                ack: false,
                full: true,
                ack_revision: None,
                ack_key: None,
            }),
        )["result"]
            .clone();
        assert_eq!(moved["member"]["pane_id"], public(&app, b), "{moved}");
        assert_eq!(moved["eligible"], true);
        app.state.assert_invariants_for_test();
    }

    /// The MCP's split ack: `ack_revision` and `ack_key` from an earlier read.
    fn ack_read(app: &mut App, pane: PaneId, read: &serde_json::Value) {
        let caller_pane = public(app, pane);
        call(
            app,
            Method::TeamContext(TeamContextParams {
                caller_pane,
                ack: true,
                full: false,
                ack_revision: read["revision"].as_u64(),
                ack_key: read["ack_key"].as_str().map(str::to_string),
            }),
        );
    }

    #[test]
    fn an_ack_never_reaches_a_team_or_line_it_was_not_read_from() {
        let mut app = team_app();
        let (a, b) = (pane(&app, 1, 0), pane(&app, 1, 1));
        detect(&mut app, a, Some(Agent::Claude));
        detect(&mut app, b, Some(Agent::Claude));
        make(&mut app, Some("fix sync"));
        context(&mut app, a, true, false);
        set_role(&mut app, b, Some("reviewer"));

        // (1) Read a delta, the team is disbanded, then the ack lands: the
        // disband line is still pending.
        let read = context(&mut app, a, false, false);
        assert!(read["ack_key"]
            .as_str()
            .is_some_and(|k| k.starts_with("team:")));
        let workspace_id = group_id(&app);
        call(
            &mut app,
            Method::TeamDisband(TeamWorkspaceParams {
                workspace_id: workspace_id.clone(),
            }),
        );
        ack_read(&mut app, a, &read);
        let gone = context(&mut app, a, false, false);
        assert_eq!(gone["text"], DISBANDED_LINE, "{gone}");

        // (2) Re-made meanwhile: the old team's ack does not mark the new
        // roster as seen.
        make(&mut app, None);
        ack_read(&mut app, a, &read);
        let fresh = context(&mut app, a, false, false);
        assert!(
            fresh["text"].as_str().is_some_and(|t| !t.is_empty()),
            "the new team's roster still arrives: {fresh}"
        );
        assert_ne!(fresh["ack_key"], read["ack_key"]);

        // The pending line's own ack clears it.
        call(
            &mut app,
            Method::TeamDisband(TeamWorkspaceParams { workspace_id }),
        );
        let line = context(&mut app, a, false, false);
        assert!(line["ack_key"]
            .as_str()
            .is_some_and(|k| k.starts_with("gone:")));
        ack_read(&mut app, a, &line);
        assert!(context(&mut app, a, false, false)["text"].is_null());
    }

    #[test]
    fn a_user_rename_of_a_member_is_a_roster_change() {
        let mut app = team_app();
        let (a, b) = (pane(&app, 1, 0), pane(&app, 1, 1));
        detect(&mut app, a, Some(Agent::Claude));
        detect(&mut app, b, Some(Agent::Claude));
        make(&mut app, None);
        set_role(&mut app, b, Some("reviewer"));
        context(&mut app, a, true, false);
        let (view, revision) = (app.state.teams_view_rev, app.team_revision(1));
        let target = public(&app, b);
        let renamed = call(
            &mut app,
            Method::AgentRename(crate::api::schema::AgentRenameParams {
                target,
                name: Some("critic".into()),
            }),
        );
        assert!(renamed.get("result").is_some(), "{renamed}");
        assert!(app.team_revision(1) > revision);
        assert!(app.state.teams_view_rev > view);
        let told = context(&mut app, a, true, false);
        assert!(
            told["text"]
                .as_str()
                .unwrap_or_default()
                .contains("reviewer is now critic"),
            "{told}"
        );
        assert_eq!(
            app.team_info(1, false).unwrap().members[1].name.as_deref(),
            Some("critic")
        );
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn the_purpose_author_comes_from_the_pane_record() {
        let mut app = team_app();
        let (a, c) = (pane(&app, 1, 0), pane(&app, 1, 2));
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal_mut(&mut app, a).set_agent_name("fixer".into());
        let workspace_id = group_id(&app);
        let caller_pane = Some(public(&app, a));
        let made = call(
            &mut app,
            Method::TeamMake(TeamMakeParams {
                workspace_id: workspace_id.clone(),
                purpose: Some("from an agent".into()),
                caller_pane,
            }),
        );
        assert_eq!(
            made["result"]["team"]["purpose_by"],
            serde_json::json!({"kind": "agent", "name": "fixer"})
        );
        let coordinator_terminal = app.state.workspaces[1]
            .pane_state(c)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state.coordinator_terminal_id = Some(coordinator_terminal);
        let caller_pane = Some(public(&app, c));
        let set = call(
            &mut app,
            Method::TeamSetPurpose(TeamSetPurposeParams {
                workspace_id: workspace_id.clone(),
                purpose: Some("from the coordinator".into()),
                caller_pane,
            }),
        );
        assert_eq!(set["result"]["team"]["purpose_by"]["kind"], "coordinator");
        let unknown = call(
            &mut app,
            Method::TeamSetPurpose(TeamSetPurposeParams {
                workspace_id,
                purpose: Some("x".into()),
                caller_pane: Some("w9:p9".into()),
            }),
        );
        assert_eq!(unknown["error"]["code"], "pane_not_found");
    }

    #[test]
    fn status_changes_and_renames_of_unrelated_tabs_cost_no_push() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        make(&mut app, None);
        let view = app.state.teams_view_rev;
        let workspace_id = app.public_workspace_id(1);
        app.emit_event(EventEnvelope {
            event: EventKind::PaneAgentStatusChanged,
            data: EventData::PaneAgentStatusChanged {
                pane_id: public(&app, a),
                workspace_id,
                agent_status: AgentStatus::Working,
                agent: Some("claude".into()),
                title: None,
                display_agent: None,
                state_labels: HashMap::new(),
            },
        });
        assert_eq!(app.state.teams_view_rev, view);
        // A member's tab rename is a view change; another tab's is not.
        let other_tab = app.public_tab_id(1, 1).unwrap();
        call(
            &mut app,
            Method::TabRename(TabRenameParams {
                tab_id: other_tab,
                label: "x".into(),
            }),
        );
        assert_eq!(app.state.teams_view_rev, view);
        let member_tab = app.public_tab_id(1, 0).unwrap();
        call(
            &mut app,
            Method::TabRename(TabRenameParams {
                tab_id: member_tab,
                label: "y".into(),
            }),
        );
        assert!(app.state.teams_view_rev > view);
    }

    #[test]
    fn disband_tells_members_once_and_keeps_names() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        terminal_mut(&mut app, a).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        make(&mut app, None);
        set_role(&mut app, a, Some("fixer"));
        let workspace_id = group_id(&app);
        let disbanded = call(
            &mut app,
            Method::TeamDisband(TeamWorkspaceParams {
                workspace_id: workspace_id.clone(),
            }),
        );
        assert!(disbanded["result"]["team"].is_null(), "{disbanded}");
        assert_eq!(app.state.team_count, 0);
        assert_eq!(
            terminal_mut(&mut app, a).agent_name.as_deref(),
            Some("fixer")
        );
        assert_eq!(context(&mut app, a, true, false)["text"], DISBANDED_LINE);
        assert!(context(&mut app, a, true, false)["text"].is_null());
        let again = call(
            &mut app,
            Method::TeamDisband(TeamWorkspaceParams { workspace_id }),
        );
        assert_eq!(again["error"]["code"], "no_team");
        assert_eq!(app.team_tombstones.len(), 0);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn no_team_means_no_team_work_on_events() {
        let mut app = team_app();
        let a = pane(&app, 1, 0);
        detect(&mut app, a, Some(Agent::Claude));
        let pane_id = public(&app, a);
        call(
            &mut app,
            Method::PaneMove(PaneMoveParams {
                pane_id,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: None,
                },
                focus: false,
            }),
        );
        assert_eq!(app.team_follow_calls, 0, "follow_teams never ran");
        assert_eq!(app.state.teams_view_rev, 0);
        make(&mut app, None);
        let b = pane(&app, 1, 1);
        detect(&mut app, b, Some(Agent::Claude));
        assert!(app.team_follow_calls > 0);
    }

    #[test]
    fn invariants_hold_on_adversarial_state_with_a_team() {
        let mut state = AppState::test_with_adversarial_identity_state();
        assert_eq!(state.team_count, 1);
        assert_eq!(state.team_index.len(), 2);
        state.assert_invariants_for_test();
        // A stale index is caught.
        state.team_index.clear();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.assert_invariants_for_test()
        }));
        assert!(caught.is_err());
        assert!(state.rebuild_team_index());
        state.assert_invariants_for_test();
    }
}
