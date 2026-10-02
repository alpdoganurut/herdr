//! Teams on the wire (fork): the `endpoint.teams.v1` control message every
//! client shell receives.
//!
//! Each payload is the whole team list (idempotent; a client replaces its
//! teams for that endpoint when `boot_id` differs or `revision` is higher).
//! It carries membership, roles, purposes and public ids, never status:
//! headers, marks and status roll-ups come from the frozen shell snapshot,
//! so a status flip costs no payload. The push is a revision compare in the
//! render pass (`AppState::teams_view_rev`): equal revisions cost one
//! comparison and no allocation, and the list is projected and framed at
//! most once per pass, only when some client is behind.
//!
//! A server that never had a team (revision 0) sends nothing, so existing
//! control streams are unchanged; an older client ignores the unknown kind.

use serde::{Deserialize, Serialize};

use crate::api::schema::TeamInfo;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the team list.
pub const TEAMS_KIND: &str = "endpoint.teams.v1";

/// The team list as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TeamsPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's teams view revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// Every team, in sidebar order, without status fields.
    #[serde(default)]
    pub teams: Vec<TeamInfo>,
}

impl TeamsPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: TEAMS_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.teams.v1` control message; `None` for
    /// malformed data.
    // Until the client's control decode calls it (the client commit).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The framed payload of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<Vec<u8>>,
}

/// Send `client` the team list when its last sent revision is behind.
/// `Err` when the client's writer is gone or framing failed (the caller
/// drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.state.teams_view_rev;
    if revision == 0 || client.shell_teams_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let payload = TeamsPayload {
            boot_id: boot_id.to_owned(),
            revision,
            teams: app.team_infos(false),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("teams frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_teams_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{AgentStatus, TeamMemberInfo};

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = TeamsPayload {
            boot_id: "boot".into(),
            revision: 4,
            teams: vec![TeamInfo {
                workspace_id: "w2".into(),
                workspace_label: "search-it".into(),
                purpose: Some("fix sync".into()),
                members: vec![TeamMemberInfo {
                    pane_id: "w2:p1".into(),
                    role: Some("fixer".into()),
                    ..TeamMemberInfo::default()
                }],
                ..TeamInfo::default()
            }],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, TEAMS_KIND);
        assert!(
            !data.contains("status"),
            "the push carries no status: {data}"
        );
        assert_eq!(TeamsPayload::decode(&data).unwrap(), payload);
        assert_eq!(TeamsPayload::decode("{not json"), None);
        let bare = TeamsPayload::decode(r#"{"boot_id":"b","revision":1}"#).unwrap();
        assert!(bare.teams.is_empty());
        // A reply member with status still decodes as a push member.
        let with_status = serde_json::to_string(&TeamMemberInfo {
            pane_id: "w2:p1".into(),
            status: Some(AgentStatus::Idle),
            ..TeamMemberInfo::default()
        })
        .unwrap();
        assert!(with_status.contains("\"status\":\"idle\""));
    }
}
