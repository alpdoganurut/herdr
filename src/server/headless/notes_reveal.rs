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

/// The framed payloads of one render pass (one per reveal), built on first
/// use, with the revision they were built after.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<(u64, Vec<Vec<u8>>)>,
}

/// Send `client` every kept reveal newer than the last it had, oldest first.
/// A client's first pass records the current revision without sending (no
/// replay of a reveal from before it attached). `Err` when the client's
/// writer is gone or framing failed (the caller drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let reveals = &app.notes.reveals;
    let revision = reveals.back().map_or(0, |reveal| reveal.revision);
    let Some(sent) = client.shell_notes_reveal_sent else {
        client.shell_notes_reveal_sent = Some(revision);
        return Ok(());
    };
    if revision <= sent {
        return Ok(());
    }
    let mut framed = Vec::new();
    let cached = frame
        .framed
        .as_ref()
        .filter(|(after, _)| *after == sent)
        .map(|(_, frames)| frames.clone());
    match cached {
        Some(frames) => framed = frames,
        None => {
            for reveal in reveals.iter().filter(|reveal| reveal.revision > sent) {
                let payload = NotesRevealPayload {
                    boot_id: boot_id.to_owned(),
                    revision: reveal.revision,
                    tab_id: reveal.tab_id.clone(),
                };
                let message = payload.message().map_err(|err| err.to_string())?;
                framed.push(
                    HeadlessServer::frame_server_message(&message)
                        .map_err(|err| err.to_string())?,
                );
            }
            frame.framed = Some((sent, framed.clone()));
        }
    }
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    for bytes in framed {
        writer
            .control
            .send(bytes)
            .map_err(|_| "client control channel closed".to_string())?;
    }
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
