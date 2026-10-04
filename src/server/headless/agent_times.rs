//! Agent state times on the wire (fork): the `endpoint.agent-times.v1`
//! control message every client shell receives (src/app/agent_times.rs has
//! the server side).
//!
//! Each payload is the whole list of panes whose agent state changed since
//! the server started (idempotent; a client replaces its list for that
//! endpoint when `boot_id` differs or `revision` is higher). Every entry
//! carries the agent's `state_change_seq` so a client uses a time only while
//! it matches the snapshot's agent, and `server_now_unix_ms` so the client
//! measures each age against the server's own clock (no clock skew between
//! machines). The push is a revision compare in the render pass
//! (`AppState::agent_times_view_rev`): equal revisions cost one comparison
//! and no allocation, and the list is projected and framed at most once per
//! pass, only when some client is behind. It rides the JSON control channel,
//! so the bincode shell snapshot (and `PROTOCOL_VERSION`) is unchanged.
//!
//! A server that never saw a state change (revision 0) sends nothing; an
//! older client ignores the unknown kind.

// Sidebar v2 S0b: the render pass (headless/render.rs), the control decoder
// and the client dispatch wire this up in the data-push step; until then
// the non-test build does not call it.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::app::agent_times::AgentTimePane;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the agent times list.
pub const AGENT_TIMES_KIND: &str = "endpoint.agent-times.v1";

/// When each pane's agent entered its current state, as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentTimesPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's agent times view revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// The server's wall clock when the payload was built (milliseconds since
    /// the Unix epoch); ages are `server_now_unix_ms - since_unix_ms`.
    pub server_now_unix_ms: u64,
    /// Every pane with a known state change; a pane not listed has no time.
    #[serde(default)]
    pub panes: Vec<AgentTimePane>,
}

impl AgentTimesPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: AGENT_TIMES_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.agent-times.v1` control message; `None`
    /// for malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The framed payload of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<Vec<u8>>,
}

/// Send `client` the agent times list when its last sent revision is
/// behind. `Err` when the client's writer is gone or framing failed (the
/// caller drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.state.agent_times_view_rev;
    if revision == 0 || client.shell_agent_times_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let payload = AgentTimesPayload {
            boot_id: boot_id.to_owned(),
            revision,
            server_now_unix_ms: crate::codex_sessions::now_unix_ms(),
            panes: app.agent_times(),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("agent times frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_agent_times_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_garbage_is_rejected_and_panes_default() {
        let payload = AgentTimesPayload {
            boot_id: "boot".into(),
            revision: 4,
            server_now_unix_ms: 1_700_000_100_000,
            panes: vec![
                AgentTimePane {
                    pane_id: "w1:p2".into(),
                    state_change_seq: 7,
                    since_unix_ms: 1_700_000_000_000,
                },
                AgentTimePane {
                    pane_id: "w2:p1".into(),
                    state_change_seq: 9,
                    since_unix_ms: 1_700_000_050_000,
                },
            ],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, AGENT_TIMES_KIND);
        assert!(data.contains(r#""state_change_seq":7"#), "{data}");
        assert_eq!(AgentTimesPayload::decode(&data).unwrap(), payload);
        assert_eq!(AgentTimesPayload::decode("{not json"), None);
        assert_eq!(
            AgentTimesPayload::decode(r#"{"boot_id":"b","revision":1}"#),
            None,
            "server_now_unix_ms is required"
        );
        let bare =
            AgentTimesPayload::decode(r#"{"boot_id":"b","revision":1,"server_now_unix_ms":5}"#)
                .unwrap();
        assert!(bare.panes.is_empty());
        // Fields a newer server adds are ignored.
        let newer = AgentTimesPayload::decode(
            r#"{"boot_id":"b","revision":2,"server_now_unix_ms":9,"extra":1,"panes":[{"pane_id":"w1:p1","state_change_seq":3,"since_unix_ms":4,"kind":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(newer.panes[0].since_unix_ms, 4);
    }
}
