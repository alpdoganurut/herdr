//! Quiet messages and do-not-disturb on the wire (fork): the
//! `endpoint.quiet.v1` control message every client shell receives
//! (src/app/quiet_messages.rs and src/app/tab_dnd.rs have the server side).
//!
//! Each payload is the whole picture (idempotent; a client replaces its
//! state for that endpoint when `boot_id` differs or `revision` is higher):
//! the public ids of the tabs in do-not-disturb and, per pane, how many quiet
//! messages wait for its agent. The push is a revision compare in the render
//! pass (`QuietStore::view_rev`): equal revisions cost one comparison and no
//! allocation, and the payload is projected and framed at most once per
//! pass, only when some client is behind. It rides the JSON control channel,
//! so the bincode shell snapshot (and `PROTOCOL_VERSION`) is unchanged.
//!
//! A server that never had a quiet message or a do-not-disturb tab
//! (revision 0) sends nothing; an older client ignores the unknown kind.

use serde::{Deserialize, Serialize};

use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the quiet state.
pub const QUIET_KIND: &str = "endpoint.quiet.v1";

/// One pane with quiet messages waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuietPendingPane {
    pub pane_id: String,
    /// The pane's tab (public id), for the tab row's count.
    #[serde(default)]
    pub tab_id: String,
    pub count: u32,
}

/// The quiet state as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct QuietPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's quiet view revision; higher replaces lower for one
    /// `boot_id`.
    pub revision: u64,
    /// Every tab in do-not-disturb (public ids), in workspace and tab order;
    /// a tab not listed is not.
    #[serde(default)]
    pub dnd_tab_ids: Vec<String>,
    /// Every pane with quiet messages waiting; a pane not listed has none.
    #[serde(default)]
    pub pending: Vec<QuietPendingPane>,
}

impl QuietPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: QUIET_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.quiet.v1` control message; `None` for
    /// malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The payload for `app` (the push and tests).
pub(crate) fn payload_of(app: &crate::app::App, boot_id: &str) -> QuietPayload {
    QuietPayload {
        boot_id: boot_id.to_owned(),
        revision: app.quiet.view_rev,
        dnd_tab_ids: app.tab_dnd_ids(),
        pending: app
            .quiet
            .pending_view
            .iter()
            .map(|pane| QuietPendingPane {
                pane_id: pane.pane_id.clone(),
                tab_id: pane.tab_id.clone(),
                count: pane.count,
            })
            .collect(),
    }
}

/// The framed payload of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrame {
    framed: Option<Vec<u8>>,
}

/// Send `client` the quiet state when its last sent revision is behind.
/// `Err` when the client's writer is gone or framing failed (the caller
/// drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frame: &mut PassFrame,
) -> Result<(), String> {
    let revision = app.quiet.view_rev;
    if revision == 0 || client.shell_quiet_sent == Some(revision) {
        return Ok(());
    }
    if frame.framed.is_none() {
        let message = payload_of(app, boot_id)
            .message()
            .map_err(|err| err.to_string())?;
        frame.framed =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = frame.framed.as_ref() else {
        return Err("quiet frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_quiet_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_garbage_is_rejected() {
        let payload = QuietPayload {
            boot_id: "boot".into(),
            revision: 3,
            dnd_tab_ids: vec!["w1:t2".into()],
            pending: vec![QuietPendingPane {
                pane_id: "w1:p3".into(),
                tab_id: "w1:t2".into(),
                count: 2,
            }],
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, QUIET_KIND);
        assert!(data.contains(r#""dnd_tab_ids":["w1:t2"]"#), "{data}");
        assert_eq!(QuietPayload::decode(&data).unwrap(), payload);
        assert_eq!(QuietPayload::decode("{not json"), None);
        let bare = QuietPayload::decode(r#"{"boot_id":"b","revision":1}"#).unwrap();
        assert!(bare.dnd_tab_ids.is_empty() && bare.pending.is_empty());
    }
}
