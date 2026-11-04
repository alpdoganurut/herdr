//! Fork (compact bar): relaying the browser host's asks on a placement on
//! the wire: the `endpoint.placement-action.v1` control message the client
//! that owns the placement receives after a `browser.placement_action`
//! (the companion's expand, restore and close).
//!
//! The server only relays: the payload names the placement, its herdr tab
//! and the action, and the client decides what that does to its dock and
//! Browser view (TUI state, never the server's). One-shot like the notes
//! reveal: a client gets each ask on its own placement once, and its first
//! render pass records the current revision without sending (no replay). It
//! rides the JSON control channel, so the bincode shell snapshot (and
//! `PROTOCOL_VERSION`) is unchanged; an older client ignores the unknown
//! kind (and an older server never advertises `placement_actions`).

use serde::{Deserialize, Serialize};

use crate::api::schema::BrowserPlacementAction;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of a placement action.
pub const PLACEMENT_ACTION_KIND: &str = "endpoint.placement-action.v1";

/// One ask as relayed to its client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlacementActionPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The ask's revision; a client acts on a higher one only, per
    /// `boot_id`.
    pub revision: u64,
    /// The placement the host asks about (the client's own id).
    pub id: String,
    /// The herdr tab the placement showed when asked.
    pub tab_id: String,
    pub action: BrowserPlacementAction,
}

impl PlacementActionPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: PLACEMENT_ACTION_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.placement-action.v1` control message;
    /// `None` for malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// Send client `client_id` its kept asks newer than the last it had, oldest
/// first. `latest` is the board's newest revision, read once per pass: a
/// client already at it costs no board lock. A client's first pass records
/// `latest` without sending. `Err` when the client's writer is gone or
/// framing failed (the caller drops the client).
pub(super) fn sync_client(
    latest: u64,
    boot_id: &str,
    client_id: u64,
    client: &mut ClientConnection,
) -> Result<(), String> {
    let Some(sent) = client.shell_placement_action_sent else {
        client.shell_placement_action_sent = Some(latest);
        return Ok(());
    };
    if latest <= sent {
        return Ok(());
    }
    let records = crate::browser::placements::board().actions_for(client_id, sent);
    if !records.is_empty() {
        let Some(writer) = client.writer.as_ref() else {
            return Err("client writer gone".into());
        };
        for record in records {
            let payload = PlacementActionPayload {
                boot_id: boot_id.to_owned(),
                revision: record.revision,
                id: record.id,
                tab_id: record.tab_id,
                action: record.action,
            };
            let message = payload.message().map_err(|err| err.to_string())?;
            let bytes =
                HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?;
            writer
                .control
                .send(bytes)
                .map_err(|_| "client control channel closed".to_string())?;
        }
    }
    client.shell_placement_action_sent = Some(latest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = PlacementActionPayload {
            boot_id: "boot".into(),
            revision: 3,
            id: "pl-1".into(),
            tab_id: "w1:t2".into(),
            action: BrowserPlacementAction::Restore,
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, PLACEMENT_ACTION_KIND);
        assert!(data.contains(r#""action":"restore""#), "{data}");
        assert_eq!(PlacementActionPayload::decode(&data).unwrap(), payload);
        assert_eq!(PlacementActionPayload::decode("{not json"), None);
        // an action a newer server adds reads as Unknown (the client ignores it)
        let newer = data.replace("restore", "detach");
        assert_eq!(
            PlacementActionPayload::decode(&newer).unwrap().action,
            BrowserPlacementAction::Unknown
        );
    }
}
