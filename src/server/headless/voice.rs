//! Voice modes on the wire (fork): the `endpoint.voice.v1` control message
//! every client shell receives (src/app/voice.rs has the server side).
//!
//! Each payload is the whole list of panes in voice mode (idempotent; a
//! client replaces its list for that endpoint when `boot_id` differs or
//! `revision` is higher). The push is a revision compare in the render pass
//! (`AppState::voice_view_rev`): equal revisions cost one comparison and no
//! allocation, and the list is projected and framed at most once per pass,
//! only when some client is behind. It rides the JSON control channel, so
//! the bincode shell snapshot (and `PROTOCOL_VERSION`) is unchanged.
//!
//! A server that never saw a voice mode (revision 0) sends nothing; an older
//! client ignores the unknown kind.

use serde::{Deserialize, Serialize};

use crate::app::voice::VoicePane;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the voice list.
pub const VOICE_KIND: &str = "endpoint.voice.v1";

/// The panes in voice mode as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VoicePayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's voice view revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// Every pane in voice mode (live or muted); a pane not listed is off.
    #[serde(default)]
    pub panes: Vec<VoicePane>,
}

impl VoicePayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: VOICE_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.voice.v1` control message; `None` for
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

/// Send `client` the voice list when its last sent revision is behind.
/// `Err` when the client's writer is gone or framing failed (the caller
/// drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.state.voice_view_rev;
    if revision == 0 || client.shell_voice_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let payload = VoicePayload {
            boot_id: boot_id.to_owned(),
            revision,
            panes: app.voice_panes(),
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("voice frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_voice_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentVoiceMode;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = VoicePayload {
            boot_id: "boot".into(),
            revision: 3,
            panes: vec![
                VoicePane {
                    pane_id: "w1:p2".into(),
                    voice: AgentVoiceMode::Live,
                },
                VoicePane {
                    pane_id: "w2:p1".into(),
                    voice: AgentVoiceMode::Muted,
                },
            ],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, VOICE_KIND);
        assert!(data.contains(r#""voice":"live""#), "{data}");
        assert_eq!(VoicePayload::decode(&data).unwrap(), payload);
        assert_eq!(VoicePayload::decode("{not json"), None);
        let bare = VoicePayload::decode(r#"{"boot_id":"b","revision":1}"#).unwrap();
        assert!(bare.panes.is_empty());
        // A mode this build does not know still decodes.
        let newer = VoicePayload::decode(
            r#"{"boot_id":"b","revision":2,"panes":[{"pane_id":"w1:p1","voice":"speaking"}]}"#,
        )
        .unwrap();
        assert_eq!(newer.panes[0].voice, AgentVoiceMode::Unknown);
    }
}
