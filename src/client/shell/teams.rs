//! Teams on the client (fork): the client-only copy of each endpoint's teams,
//! as its last `endpoint.teams.v1` push listed them, and the shell glue for
//! the `team.*` methods.
//!
//! A team is a server-side fact of a group (a space). The server pushes the
//! whole list whenever its team view changes (never on an agent status
//! flip); the client keeps the last list per endpoint and two lookup maps
//! built once per push, so the `tabs` sidebar's group-header mark and the
//! member rows' dim `◆` are O(1) lookups in render:
//!
//! - `by_workspace`: workspace id → index into `teams`;
//! - `member_tabs`: tab ids holding a member.
//!
//! The push carries no status; the Team info overlay (`team_overlay.rs`)
//! pulls `team.get` for status and time in state. A state is used only while
//! its `boot_id` is the active snapshot's, so a server switch never shows the
//! old server's teams. Every menu item and overlay action is gated on the
//! endpoint advertising `team.get`: an old server shows nothing.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::api::schema::team::{TeamInfo, TeamMemberInfo};
use crate::api::schema::Method;
use crate::server::headless::teams::TeamsPayload;

/// The team mark: the group header's lead, the member rows' dim mark.
pub(crate) const TEAM_MARK: &str = "◆";
/// A member without a role in the Team info overlay.
pub(crate) const NO_ROLE_MARK: &str = "◇";

/// One endpoint's teams, as its last push listed them.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientTeamsState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    pub(crate) teams: Vec<TeamInfo>,
    /// Workspace id → index into `teams`; rebuilt only when a push arrives.
    pub(crate) by_workspace: HashMap<String, usize>,
    /// Tabs holding a team member; rebuilt only when a push arrives.
    pub(crate) member_tabs: HashSet<String>,
}

impl ClientTeamsState {
    /// The state for a push, with its lookup maps.
    pub(crate) fn from_payload(payload: TeamsPayload) -> Self {
        let mut by_workspace = HashMap::with_capacity(payload.teams.len());
        let mut member_tabs = HashSet::new();
        for (index, team) in payload.teams.iter().enumerate() {
            by_workspace.insert(team.workspace_id.clone(), index);
            member_tabs.extend(
                team.members
                    .iter()
                    .filter_map(|member| member.tab_id.as_deref())
                    .map(str::to_owned),
            );
        }
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            teams: payload.teams,
            by_workspace,
            member_tabs,
        }
    }

    /// The team of a group: an O(1) lookup.
    pub(crate) fn team(&self, workspace_id: &str) -> Option<&TeamInfo> {
        self.by_workspace
            .get(workspace_id)
            .and_then(|index| self.teams.get(*index))
    }

    /// Whether `tab_id` holds a member: an O(1) lookup.
    pub(crate) fn is_member_tab(&self, tab_id: &str) -> bool {
        self.member_tabs.contains(tab_id)
    }
}

/// The group header's label for a team: the purpose, or the group label
/// while there is none (`true` = drawn dim).
pub(crate) fn header_label<'a>(team: &'a TeamInfo, group_label: &'a str) -> (&'a str, bool) {
    match team
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|purpose| !purpose.is_empty())
    {
        Some(purpose) => (purpose, false),
        None => (group_label, true),
    }
}

/// A member's display name: its agent name (the role's slug once set),
/// else its role, else the agent kind, else `agent`.
pub(crate) fn member_name(member: &TeamMemberInfo) -> &str {
    member
        .name
        .as_deref()
        .or(member.role.as_deref())
        .or(member.agent.as_deref())
        .unwrap_or("agent")
}

/// What the group menu offers for teams, captured when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientGroupTeamMenu {
    /// A group that is not a team: "Make team…".
    NotTeam,
    /// A team group: Team info, Edit purpose…, Disband team, and Ungroup
    /// relabelled "Ungroup (disbands team)" and confirmed.
    Team,
}

/// What the tab menu offers for teams, captured when it opens: only for a
/// tab with an agent pane in a team group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientTabTeamMenu {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    /// The pane is a member ("Set team role…", "Leave team"), else it was
    /// removed or never joined ("Join team").
    pub(crate) member: bool,
    /// The member's role when the menu opened (the Rename modal's text).
    pub(crate) role: Option<String>,
}

/// One `team.*` request the shell sends. The TUI never sets `caller_pane`:
/// absent means the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TeamRequest {
    Get {
        workspace_id: String,
    },
    Make {
        workspace_id: String,
        purpose: Option<String>,
    },
    Disband {
        workspace_id: String,
    },
    SetPurpose {
        workspace_id: String,
        purpose: Option<String>,
    },
    SetRole {
        pane_id: String,
        role: Option<String>,
    },
    Join {
        pane_id: String,
    },
    Leave {
        pane_id: String,
    },
}

/// One line of user text for a purpose or role: trimmed, `None` when empty
/// (the server sanitizes and caps it).
pub(crate) fn one_line(text: &str) -> Option<String> {
    let text = text.split(['\n', '\r']).next().unwrap_or_default().trim();
    (!text.is_empty()).then(|| text.to_owned())
}

impl TeamRequest {
    /// The `Method` for a request.
    pub(crate) fn method(self) -> Method {
        use crate::api::schema::team::{
            TeamGetParams, TeamJoinParams, TeamMakeParams, TeamPaneParams, TeamSetPurposeParams,
            TeamSetRoleParams, TeamWorkspaceParams,
        };
        match self {
            Self::Get { workspace_id } => Method::TeamGet(TeamGetParams {
                workspace_id: Some(workspace_id),
                caller_pane: None,
            }),
            Self::Make {
                workspace_id,
                purpose,
            } => Method::TeamMake(TeamMakeParams {
                workspace_id,
                purpose,
                caller_pane: None,
            }),
            Self::Disband { workspace_id } => {
                Method::TeamDisband(TeamWorkspaceParams { workspace_id })
            }
            Self::SetPurpose {
                workspace_id,
                purpose,
            } => Method::TeamSetPurpose(TeamSetPurposeParams {
                workspace_id,
                purpose,
                caller_pane: None,
            }),
            Self::SetRole { pane_id, role } => Method::TeamSetRole(TeamSetRoleParams {
                pane_id,
                role,
                caller_pane: None,
            }),
            Self::Join { pane_id } => Method::TeamJoin(TeamJoinParams {
                pane_id,
                role: None,
            }),
            Self::Leave { pane_id } => Method::TeamLeave(TeamPaneParams { pane_id }),
        }
    }

    /// How its reply is routed: a `team.get` for the overlay carries the
    /// workspace it was asked for, so a reply for a closed or replaced
    /// overlay is dropped; every other reply refreshes the open overlay.
    fn kind(&self) -> TeamRequestKind {
        match self {
            Self::Get { workspace_id } => TeamRequestKind::Get {
                workspace_id: workspace_id.clone(),
            },
            _ => TeamRequestKind::Change,
        }
    }
}

/// What a pending `team.*` request was (`PendingEndpointKind::Team`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TeamRequestKind {
    Get { workspace_id: String },
    Change,
}

/// The `team.get` method used to gate every team item.
fn team_get_probe() -> Method {
    TeamRequest::Get {
        workspace_id: String::new(),
    }
    .method()
}

/// An endpoint's teams while they belong to `snapshot`'s server: what the
/// renderer reads (field-level borrows, for the compose pass).
pub(crate) fn active_teams_of<'a>(
    teams: &'a HashMap<ClientEndpointId, ClientTeamsState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientTeamsState> {
    let snapshot = snapshot?;
    teams
        .get(endpoint_id)
        .filter(|teams| teams.boot_id == snapshot.boot_id)
}

impl ClientShellState {
    /// The active endpoint's teams, while they belong to the active
    /// snapshot's server.
    pub(super) fn active_teams(&self) -> Option<&ClientTeamsState> {
        active_teams_of(
            &self.teams,
            &self.active_endpoint_id,
            self.snapshot.as_deref(),
        )
    }

    /// The active endpoint advertises `team.get` (every team item is gated
    /// on it).
    pub(super) fn teams_supported(&self) -> bool {
        self.supports_endpoint_method(&team_get_probe())
    }

    /// A group's team on the active endpoint.
    pub(super) fn active_team(&self, workspace_id: &str) -> Option<&TeamInfo> {
        self.active_teams()?.team(workspace_id)
    }

    /// An `endpoint.teams.v1` push. Returns whether to repaint. An older or
    /// equal revision of the same server is ignored; a new server replaces
    /// the state. An open Team info overlay of a group in the push re-pulls
    /// `team.get` on the next tick.
    pub(crate) fn receive_teams(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: TeamsPayload,
    ) -> bool {
        if self.teams.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        let state = ClientTeamsState::from_payload(payload);
        if let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() {
            if overlay.endpoint_id == *endpoint_id {
                overlay.on_push(state.team(&overlay.workspace_id));
            }
        }
        self.teams.insert(endpoint_id.clone(), state);
        true
    }

    /// Send one request; `false` when it could not be sent.
    pub(super) fn push_team_request(
        &mut self,
        request: TeamRequest,
        outcome: &mut ClientShellInput,
    ) -> bool {
        let kind = request.kind();
        self.push_endpoint_method_with_kind(
            request.method(),
            PendingEndpointKind::Team(kind),
            outcome,
        )
    }

    /// The tick: the open Team info overlay pulls `team.get` when it opened,
    /// after a push for its group and once a minute.
    pub(crate) fn tick_teams(&mut self, now: std::time::Instant, outcome: &mut ClientShellInput) {
        // Nothing to do (and no method lookup) unless Team info is open.
        if !matches!(self.overlay, Some(ClientShellOverlay::TeamInfo(_))) {
            return;
        }
        let advertised = self.teams_supported();
        let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() else {
            return;
        };
        if overlay.endpoint_id != self.active_endpoint_id {
            return;
        }
        let Some(workspace_id) = overlay.pull_due(now, advertised) else {
            return;
        };
        if !self.push_team_request(TeamRequest::Get { workspace_id }, outcome) {
            if let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() {
                overlay.loading = false;
            }
        }
        outcome.repaint = true;
    }

    /// A `team.*` reply. Errors already raised the generic notice; a
    /// mutation's reply (or refusal) makes the open overlay pull again.
    pub(super) fn handle_team_endpoint_result(
        &mut self,
        kind: TeamRequestKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let reply = match result {
            Ok(crate::api::schema::ResponseResult::TeamReply { team, .. }) => Ok(team),
            Ok(_) => {
                self.set_endpoint_error("endpoint returned an unexpected team result");
                Err(())
            }
            Err(_) => Err(()),
        };
        let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() else {
            return (true, Vec::new());
        };
        match kind {
            TeamRequestKind::Get { workspace_id } => {
                if workspace_id == overlay.workspace_id {
                    overlay.on_get_reply(reply.ok(), unix_now());
                }
            }
            TeamRequestKind::Change => overlay.refresh_due = true,
        }
        (true, Vec::new())
    }
}

impl ClientShellState {
    /// The group menu's team state: `None` on a server without `team.get`
    /// and for the ungrouped bucket (the first space never is a team).
    pub(super) fn group_team_menu(&self, workspace_id: &str) -> Option<ClientGroupTeamMenu> {
        if !self.teams_supported() {
            return None;
        }
        let first = self.snapshot.as_deref()?.workspaces.first()?;
        if first.workspace_id == workspace_id {
            return None;
        }
        Some(if self.active_team(workspace_id).is_some() {
            ClientGroupTeamMenu::Team
        } else {
            ClientGroupTeamMenu::NotTeam
        })
    }

    /// The tab menu's team state: only for a tab whose agent pane is in a
    /// team group of a server with `team.get`.
    pub(super) fn tab_team_menu(
        &self,
        workspace_id: &str,
        agent: Option<&ClientTabMenuAgent>,
    ) -> Option<ClientTabTeamMenu> {
        let agent = agent?;
        if !self.teams_supported() {
            return None;
        }
        let team = self.active_team(workspace_id)?;
        let member = team
            .members
            .iter()
            .find(|member| member.pane_id == agent.pane_id);
        Some(ClientTabTeamMenu {
            pane_id: agent.pane_id.clone(),
            workspace_id: workspace_id.to_owned(),
            member: member.is_some(),
            role: member.and_then(|member| member.role.clone()),
        })
    }

    /// Tab menu: Set team role…, Leave team, Join team.
    pub(super) fn activate_tab_team_action(
        &mut self,
        team: ClientTabTeamMenu,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::SetTeamRole => self.open_team_role_rename(team, false),
            ClientContextMenuAction::LeaveTeam => {
                self.push_team_request(
                    TeamRequest::Leave {
                        pane_id: team.pane_id,
                    },
                    outcome,
                );
            }
            ClientContextMenuAction::JoinTeam => {
                self.push_team_request(
                    TeamRequest::Join {
                        pane_id: team.pane_id,
                    },
                    outcome,
                );
            }
            _ => {}
        }
        outcome.repaint = true;
    }

    /// The Rename modal for a team's purpose: `make` sends `team.make`
    /// (empty is fine: the purpose can come later), else
    /// `team.set_purpose` (empty clears it).
    pub(super) fn open_team_purpose_rename(
        &mut self,
        workspace_id: String,
        current: &str,
        make: bool,
        reopen: bool,
    ) {
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: if make {
                "team purpose (optional)"
            } else {
                "team purpose"
            },
            input: TextEditor::new(current, false),
            target: ClientRenameTarget::TeamPurpose {
                workspace_id,
                make,
                reopen,
            },
        }));
    }

    /// The Rename modal for a member's role (empty clears it).
    pub(super) fn open_team_role_rename(&mut self, team: ClientTabTeamMenu, reopen: bool) {
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: "team role (the agent is renamed to it)",
            input: TextEditor::new(team.role.as_deref().unwrap_or_default(), false),
            target: ClientRenameTarget::TeamRole {
                pane_id: team.pane_id,
                workspace_id: team.workspace_id,
                reopen,
            },
        }));
    }

    /// The request a saved team Rename modal sends; `None` for any other
    /// target (the caller keeps it).
    pub(super) fn team_rename_request(
        target: &ClientRenameTarget,
        text: &str,
    ) -> Option<TeamRequest> {
        match target {
            ClientRenameTarget::TeamPurpose {
                workspace_id,
                make: true,
                ..
            } => Some(TeamRequest::Make {
                workspace_id: workspace_id.clone(),
                purpose: one_line(text),
            }),
            ClientRenameTarget::TeamPurpose { workspace_id, .. } => Some(TeamRequest::SetPurpose {
                workspace_id: workspace_id.clone(),
                purpose: one_line(text),
            }),
            ClientRenameTarget::TeamRole { pane_id, .. } => Some(TeamRequest::SetRole {
                pane_id: pane_id.clone(),
                role: one_line(text),
            }),
            _ => None,
        }
    }

    /// A team Rename modal opened from Team info closed (saved or not):
    /// bring Team info back.
    pub(super) fn reopen_team_overlay_after(&mut self, target: &ClientRenameTarget) {
        let workspace_id = match target {
            ClientRenameTarget::TeamPurpose {
                workspace_id,
                reopen: true,
                ..
            }
            | ClientRenameTarget::TeamRole {
                workspace_id,
                reopen: true,
                ..
            } => workspace_id.clone(),
            _ => return,
        };
        if self.overlay.is_none() {
            self.open_team_overlay(workspace_id);
        }
    }

    /// Close the Rename modal without saving, reopening Team info when the
    /// modal came from it.
    pub(super) fn cancel_rename_overlay(&mut self) {
        let Some(ClientShellOverlay::Rename(rename)) = self.overlay.take() else {
            return;
        };
        self.reopen_team_overlay_after(&rename.target);
    }

    /// "Ungroup (disbands team)": confirm through the close confirmation.
    pub(super) fn open_ungroup_team_confirmation(&mut self, workspace_id: String) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        let purpose = self
            .active_team(&workspace_id)
            .and_then(|team| team.purpose.as_deref())
            .filter(|purpose| !purpose.trim().is_empty());
        let detail = match purpose {
            Some(purpose) => format!(
                "{} — the team ({purpose}) is disbanded; agents keep running",
                workspace.label
            ),
            None => format!(
                "{} — the team is disbanded; agents keep running",
                workspace.label
            ),
        };
        self.overlay = Some(ClientShellOverlay::ConfirmClose(
            ClientConfirmCloseOverlay {
                workspace_id,
                tab_target: None,
                title: "Ungroup and disband the team?".to_owned(),
                detail,
                close_group: false,
                ungroup: true,
            },
        ));
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
