//! Fork (browser keys): relaying the browser host's cycle key on the wire:
//! the `endpoint.view-cycle.v1` control message one client receives after
//! a `browser.view_cycle` (the user pressed `keys.cycle_browser` while the
//! companion had the keyboard).
//!
//! The server only relays: the payload names the herdr tab the host showed,
//! and the client steps its focused tab's browser like the key in its own
//! terminal (dock and Browser view are TUI state, never the server's). The
//! board picked the client when the host asked (the owner of the placement
//! the host covers, else the foreground client). One-shot like the
//! placement actions: a client gets each cycle once, and its first render
//! pass records the current revision without sending (no replay). It rides
//! the JSON control channel, so the bincode shell snapshot (and
//! `PROTOCOL_VERSION`) is unchanged; an older client ignores the unknown
//! kind, and the server refuses the cycle for a client whose handshake
//! does not say it takes it.

use serde::{Deserialize, Serialize};

use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of a view cycle.
pub const VIEW_CYCLE_KIND: &str = "endpoint.view-cycle.v1";

/// One cycle as relayed to its client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ViewCyclePayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The cycle's revision; a client acts on a higher one only, per
    /// `boot_id`.
    pub revision: u64,
    /// The herdr tab the host showed; a client focused on another one
    /// ignores the cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

impl ViewCyclePayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: VIEW_CYCLE_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.view-cycle.v1` control message; `None`
    /// for malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// Send client `client_id` its kept cycles newer than the last it had,
/// oldest first. `latest` is the board's newest revision, read once per
/// pass: a client already at it costs no board lock. A client's first pass
/// records `latest` without sending. `Err` when the client's writer is gone
/// or framing failed (the caller drops the client).
pub(super) fn sync_client(
    latest: u64,
    boot_id: &str,
    client_id: u64,
    client: &mut ClientConnection,
) -> Result<(), String> {
    let Some(sent) = client.shell_view_cycle_sent else {
        client.shell_view_cycle_sent = Some(latest);
        return Ok(());
    };
    if latest <= sent {
        return Ok(());
    }
    let records = crate::browser::placements::board().view_cycles_for(client_id, sent);
    if !records.is_empty() {
        let Some(writer) = client.writer.as_ref() else {
            return Err("client writer gone".into());
        };
        for record in records {
            let payload = ViewCyclePayload {
                boot_id: boot_id.to_owned(),
                revision: record.revision,
                tab_id: record.tab_id,
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
    client.shell_view_cycle_sent = Some(latest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = ViewCyclePayload {
            boot_id: "boot".into(),
            revision: 4,
            tab_id: Some("w1:t2".into()),
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, VIEW_CYCLE_KIND);
        assert_eq!(ViewCyclePayload::decode(&data).unwrap(), payload);
        assert_eq!(ViewCyclePayload::decode("{not json"), None);
        // no tab: the field is left out, and reads back as none
        let bare = ViewCyclePayload {
            tab_id: None,
            ..payload
        };
        let ServerMessage::EndpointControl { data, .. } = bare.message().unwrap() else {
            panic!("expected a control message");
        };
        assert!(!data.contains("tab_id"), "{data}");
        assert_eq!(ViewCyclePayload::decode(&data).unwrap(), bare);
    }
}
