//! Showing a tab's notes on the wire (fork): the `endpoint.notes-reveal.v1`
//! control message every client shell receives after a `notes.reveal`
//! (an agent's `agents_notes_write reveal=true`; src/app/notes.rs has the
//! server side).
//!
//! The server only signals: the payload names the tab, and each client
//! opens that tab's info pane (TUI state, never the server's) without
//! taking the keyboard, unless it is already open. It is one-shot: a client
//! gets each revision once, and a client that attaches later gets none of
//! the earlier ones (its first render pass only records the current
//! revision). It rides the JSON control channel, so the bincode shell
//! snapshot (and `PROTOCOL_VERSION`) is unchanged; an older client ignores
//! the unknown kind.

use serde::{Deserialize, Serialize};

use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of a notes reveal.
pub const NOTES_REVEAL_KIND: &str = "endpoint.notes-reveal.v1";

/// One reveal as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NotesRevealPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The reveal's revision; a client acts on a higher one only, per
    /// `boot_id`.
    pub revision: u64,
    /// The public id of the tab whose info pane to show.
    pub tab_id: String,
}

impl NotesRevealPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: NOTES_REVEAL_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.notes-reveal.v1` control message; `None`
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

/// Send `client` the latest reveal when it has not had it. A client's first
/// pass records the current revision without sending (no replay of a reveal
/// from before it attached). `Err` when the client's writer is gone or
/// framing failed (the caller drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app
        .notes
        .reveal
        .as_ref()
        .map_or(0, |reveal| reveal.revision);
    let Some(sent) = client.shell_notes_reveal_sent else {
        client.shell_notes_reveal_sent = Some(revision);
        return Ok(());
    };
    let Some(reveal) = app.notes.reveal.as_ref().filter(|_| revision > sent) else {
        return Ok(());
    };
    if frame.framed.is_none() {
        let payload = NotesRevealPayload {
            boot_id: boot_id.to_owned(),
            revision,
            tab_id: reveal.tab_id.clone(),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("notes reveal frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_notes_reveal_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = NotesRevealPayload {
            boot_id: "boot".into(),
            revision: 2,
            tab_id: "w1:t2".into(),
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, NOTES_REVEAL_KIND);
        assert!(data.contains(r#""tab_id":"w1:t2""#), "{data}");
        assert_eq!(NotesRevealPayload::decode(&data).unwrap(), payload);
        assert_eq!(NotesRevealPayload::decode("{not json"), None);
    }
}
