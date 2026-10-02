//! Teams (fork): a sidebar group (space) marked as a team, with a purpose,
//! members and their roles (`team.*`).
//!
//! Team state is a shared runtime fact owned by the server and stored with
//! the workspace. Members are identified by their pane; public pane and tab
//! ids are re-projected each time a reply or push is built (they change when
//! a pane moves), never cached.
//!
//! Digest hygiene: the `Team*Params` structs reference only plain strings,
//! booleans and numbers, never an existing enum, so the request-branch
//! digests of the client-shell methods stay independent of other schemas.
//! Every result field past the required core is optional or defaulted, and
//! [`TeamActor`] carries an `Unknown` fallback, so an older client can read a
//! newer server's reply.

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

/// The `team.*` method names on the wire.
pub mod method {
    pub const LIST: &str = "team.list";
    pub const GET: &str = "team.get";
    pub const MAKE: &str = "team.make";
    pub const DISBAND: &str = "team.disband";
    pub const SET_PURPOSE: &str = "team.set_purpose";
    pub const SET_ROLE: &str = "team.set_role";
    pub const JOIN: &str = "team.join";
    pub const LEAVE: &str = "team.leave";
    pub const CONTEXT: &str = "team.context";

    /// The methods advertised to client shells (`team.list` and
    /// `team.context` are not: the push carries the list, and the context is
    /// the agent-side read).
    #[cfg(test)]
    pub const CLIENT_SHELL: [&str; 7] = [GET, MAKE, DISBAND, SET_PURPOSE, SET_ROLE, JOIN, LEAVE];

    /// Every method.
    #[cfg(test)]
    pub const ALL: [&str; 9] = [
        LIST,
        GET,
        MAKE,
        DISBAND,
        SET_PURPOSE,
        SET_ROLE,
        JOIN,
        LEAVE,
        CONTEXT,
    ];
}

/// The error codes the `team.*` methods answer with.
pub mod error_code {
    /// `team.make` on the first space (the ungrouped bucket).
    pub const FIRST_SPACE: &str = "first_space";
    /// `team.make` on a group that is already a team.
    pub const TEAM_EXISTS: &str = "team_exists";
    /// The named group does not exist.
    pub const WORKSPACE_NOT_FOUND: &str = "workspace_not_found";
    /// The group is not a team.
    pub const NO_TEAM: &str = "no_team";
    /// The pane does not exist.
    pub const PANE_NOT_FOUND: &str = "pane_not_found";
    /// The pane is not a member of its group's team.
    pub const NOT_A_MEMBER: &str = "not_a_member";
    /// `team.join` on a pane whose group is not a team.
    pub const NOT_IN_TEAM_GROUP: &str = "not_in_team_group";
    /// `team.join` on the coordinator's pane.
    pub const COORDINATOR_PANE: &str = "coordinator_pane";
    /// `team.join` without a role on a pane that has no agent.
    pub const NO_AGENT: &str = "no_agent";
    /// A parameter is malformed.
    pub const INVALID_PARAMS: &str = "invalid_params";
}

/// Caps on team text (characters, after sanitizing to one line).
pub const PURPOSE_MAX_CHARS: usize = 80;
pub const ROLE_MAX_CHARS: usize = 32;

/// Who set a team's purpose: derived by the server from the calling pane's
/// own record (the coordinator's pane, else the agent's name), never from
/// parameters. Absent `caller_pane` means the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamActor {
    User,
    Coordinator,
    /// An agent, named as it was at the time.
    Agent {
        name: String,
    },
    /// An actor this side does not know (a newer peer).
    #[serde(other)]
    Unknown,
}

impl TeamActor {
    /// "the user", "the coordinator", the agent's name.
    pub fn describe(&self) -> String {
        match self {
            Self::User | Self::Unknown => "the user".into(),
            Self::Coordinator => "the coordinator".into(),
            Self::Agent { name } => name.clone(),
        }
    }
}

/// `team.get`: the team of a group, or of the caller's group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamGetParams {
    /// A group id (or label); absent means `caller_pane`'s group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `team.make`: mark a group as a team. Every pane in it with a live agent
/// joins, with no role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamMakeParams {
    /// A group id (or label).
    pub workspace_id: String,
    /// One line, at most 80 characters after sanitizing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// The calling agent's pane; absent means the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `team.disband`: a group by id (or label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamWorkspaceParams {
    pub workspace_id: String,
}

/// `team.set_purpose`: a missing or empty purpose clears it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamSetPurposeParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `team.set_role`: a missing or empty role clears it (the name stays).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamSetRoleParams {
    pub pane_id: String,
    /// One line, at most 32 characters after sanitizing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `team.join`: the pane joins its group's team (clearing a removal). With
/// a role it may join before its agent starts (a pre-launch join).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamJoinParams {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// `team.leave`: the pane leaves its team and is not auto-joined again
/// until it joins or moves in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamPaneParams {
    pub pane_id: String,
}

/// `team.context`: the agent-side read (`HERDR_PANE_ID`). Read-only except
/// for `ack`, which marks the current revision as told to this member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamContextParams {
    pub caller_pane: String,
    #[serde(default)]
    pub ack: bool,
    /// The whole roster block, not just the changes since the last ack.
    #[serde(default)]
    pub full: bool,
    /// With `ack`: mark only up to this revision as told (the revision the
    /// caller read and delivered), so a change landing between a read and a
    /// later ack is not lost. Absent: the current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_revision: Option<u64>,
}

/// One member, with its pane and tab re-projected now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamMemberInfo {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// The agent name, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The agent kind (`claude`, `codex`); absent while no agent runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Filled by `team.get`, `team.list` and `team.context`; never in the
    /// `endpoint.teams.v1` push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_since_unix: Option<u64>,
    #[serde(default)]
    pub joined_unix: u64,
}

/// One team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct TeamInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_by: Option<TeamActor>,
    #[serde(default)]
    pub created_unix: u64,
    /// Bumped by every join, leave, role, purpose and member rename.
    #[serde(default)]
    pub revision: u64,
    /// Join order.
    #[serde(default)]
    pub members: Vec<TeamMemberInfo>,
    /// Panes removed from the team in place (public ids now).
    #[serde(default)]
    pub excluded: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actors_round_trip_and_unknown_actors_fall_back() {
        for actor in [
            TeamActor::User,
            TeamActor::Coordinator,
            TeamActor::Agent {
                name: "fixer".into(),
            },
        ] {
            let json = serde_json::to_value(&actor).unwrap();
            assert_eq!(serde_json::from_value::<TeamActor>(json).unwrap(), actor);
        }
        assert_eq!(
            serde_json::to_value(TeamActor::Agent { name: "a".into() }).unwrap(),
            serde_json::json!({"kind": "agent", "name": "a"})
        );
        let future: TeamActor = serde_json::from_str(r#"{"kind":"robot","id":3}"#).unwrap();
        assert_eq!(future, TeamActor::Unknown);
        assert_eq!(future.describe(), "the user");
    }

    #[test]
    fn params_and_infos_default_their_optional_fields() {
        let context: TeamContextParams =
            serde_json::from_str(r#"{"caller_pane":"w1:p1"}"#).unwrap();
        assert!(!context.ack && !context.full);
        let get: TeamGetParams = serde_json::from_str("{}").unwrap();
        assert_eq!(get, TeamGetParams::default());
        let member: TeamMemberInfo = serde_json::from_str(r#"{"pane_id":"w2:p1"}"#).unwrap();
        assert_eq!(member.status, None);
        assert_eq!(member.joined_unix, 0);
        let info: TeamInfo = serde_json::from_str(r#"{"workspace_id":"w2"}"#).unwrap();
        assert!(info.members.is_empty() && info.purpose.is_none());
        let pushed = serde_json::to_string(&TeamMemberInfo {
            pane_id: "w2:p1".into(),
            ..TeamMemberInfo::default()
        })
        .unwrap();
        assert!(!pushed.contains("status"), "{pushed}");
    }

    #[test]
    fn client_shell_methods_are_a_subset_of_all() {
        for name in method::CLIENT_SHELL {
            assert!(method::ALL.contains(&name));
        }
        assert!(!method::CLIENT_SHELL.contains(&method::CONTEXT));
        assert!(!method::CLIENT_SHELL.contains(&method::LIST));
    }
}
