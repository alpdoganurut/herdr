//! Pinned tabs on the wire (fork, sidebar v3): the `endpoint.tab-pins.v1`
//! control message every client shell receives (src/app/tab_pin.rs has the
//! server side).
//!
//! Each payload is the whole list of pinned public tab ids (idempotent; a
//! client replaces its set for that endpoint when `boot_id` differs or
//! `revision` is higher). The push is a revision compare in the render pass
//! (`AppState::tab_pins_view_rev`): equal revisions cost one comparison and
//! no allocation, and the list is projected and framed at most once per
//! pass, only when some client is behind. It rides the JSON control
//! channel, so the bincode shell snapshot (and `PROTOCOL_VERSION`) is
//! unchanged.
//!
//! A server that never had a pin (revision 0) sends nothing; an older
//! client ignores the unknown kind.

use serde::{Deserialize, Serialize};

use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the pinned tab list.
pub const TAB_PINS_KIND: &str = "endpoint.tab-pins.v1";

/// The pinned tabs as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TabPinsPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's pins view revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// Every pinned tab's public id, in workspace and tab order; a tab not
    /// listed is not pinned.
    #[serde(default)]
    pub tab_ids: Vec<String>,
}

impl TabPinsPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: TAB_PINS_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.tab-pins.v1` control message; `None` for
    /// malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The framed payload of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<Vec<u8>>,
}

/// Send `client` the pinned tab list when its last sent revision is behind.
/// `Err` when the client's writer is gone or framing failed (the caller
/// drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.state.tab_pins_view_rev;
    if revision == 0 || client.shell_tab_pins_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let payload = TabPinsPayload {
            boot_id: boot_id.to_owned(),
            revision,
            tab_ids: app.tab_pin_ids(),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("tab pins frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_tab_pins_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = TabPinsPayload {
            boot_id: "boot".into(),
            revision: 3,
            tab_ids: vec!["w1:t2".into(), "w2:t1".into()],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, TAB_PINS_KIND);
        assert!(data.contains(r#""tab_ids":["w1:t2","w2:t1"]"#), "{data}");
        assert_eq!(TabPinsPayload::decode(&data).unwrap(), payload);
        assert_eq!(TabPinsPayload::decode("{not json"), None);
        let bare = TabPinsPayload::decode(r#"{"boot_id":"b","revision":1}"#).unwrap();
        assert!(bare.tab_ids.is_empty());
    }
}
