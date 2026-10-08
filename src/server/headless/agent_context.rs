//! Agent context use on the wire (fork): the `endpoint.agent-context.v1`
//! control message every client shell receives (src/app/agent_context.rs
//! has the server side).
//!
//! Each payload is the whole list of panes with a known context use
//! (idempotent; a client replaces its list for that endpoint when `boot_id`
//! differs or `revision` is higher). The push is a revision compare in the
//! render pass (`AppState::agent_context_view_rev`): equal revisions cost
//! one comparison and no allocation, and the list is projected and framed
//! at most once per pass, only when some client is behind. It rides the
//! JSON control channel, so the bincode shell snapshot (and
//! `PROTOCOL_VERSION`) is unchanged.
//!
//! A server that never read a context use (revision 0) sends nothing; an
//! older client ignores the unknown kind.

use serde::{Deserialize, Serialize};

use crate::app::agent_context::AgentContextPane;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the context use list.
pub const AGENT_CONTEXT_KIND: &str = "endpoint.agent-context.v1";

/// How full each pane agent's context window is, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentContextPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's context view revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// Every pane with a known context use; a pane not listed has none.
    #[serde(default)]
    pub panes: Vec<AgentContextPane>,
}

impl AgentContextPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: AGENT_CONTEXT_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.agent-context.v1` control message;
    /// `None` for malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The framed payload of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<Vec<u8>>,
}

/// Send `client` the context use list when its last sent revision is
/// behind. `Err` when the client's writer is gone or framing failed (the
/// caller drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.state.agent_context_view_rev;
    if revision == 0 || client.shell_agent_context_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let payload = AgentContextPayload {
            boot_id: boot_id.to_owned(),
            revision,
            panes: app.agent_context_panes(),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("agent context frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_agent_context_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_garbage_is_rejected_and_panes_default() {
        let payload = AgentContextPayload {
            boot_id: "boot".into(),
            revision: 3,
            panes: vec![
                AgentContextPane {
                    pane_id: "w1:p2".into(),
                    used: 164_000,
                    window: 200_000,
                },
                AgentContextPane {
                    pane_id: "w2:p1".into(),
                    used: 184_995,
                    window: 258_400,
                },
            ],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, AGENT_CONTEXT_KIND);
        assert!(data.contains(r#""used":164000"#), "{data}");
        assert_eq!(AgentContextPayload::decode(&data).unwrap(), payload);
        assert_eq!(AgentContextPayload::decode("{not json"), None);
        let bare = AgentContextPayload::decode(r#"{"boot_id":"b","revision":1}"#).unwrap();
        assert!(bare.panes.is_empty());
        // Fields a newer server adds are ignored.
        let newer = AgentContextPayload::decode(
            r#"{"boot_id":"b","revision":2,"extra":1,"panes":[{"pane_id":"w1:p1","used":3,"window":4,"model":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(newer.panes[0].window, 4);
    }
}
