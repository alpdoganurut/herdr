//! Agent cards on the wire (fork): the `endpoint.agent-notices.v1` control
//! message every client shell receives.
//!
//! Each payload is the whole card list (idempotent; a client replaces its
//! cards for that endpoint when `boot_id` differs or `revision` is higher).
//! The push is a revision compare in the render pass: a connection whose
//! last sent revision differs from the app's gets the list, framed at most
//! once per pass; equal revisions cost one comparison and no allocation.
//!
//! A server that never had a card (revision 0) sends nothing, so existing
//! control streams are unchanged. Consequences for the client: a connection
//! may receive no payload at all, and a client whose stored `boot_id`
//! differs from the endpoint snapshot's must drop that endpoint's cards
//! itself. The first payload a connection does receive carries
//! `initial: true` (see [`AgentNoticesPayload::initial`]).

use serde::{Deserialize, Serialize};

use crate::api::schema::AgentNoticeInfo;
use crate::protocol::ServerMessage;
use crate::server::clients::ClientConnection;

use super::HeadlessServer;

/// The `ServerMessage::EndpointControl` kind of the card list.
pub const AGENT_NOTICES_KIND: &str = "endpoint.agent-notices.v1";

/// The card list as pushed to client shells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentNoticesPayload {
    /// The server boot (the endpoint snapshot's `boot_id`).
    pub boot_id: String,
    /// The server's card revision; higher replaces lower for one `boot_id`.
    pub revision: u64,
    /// Every card, oldest first.
    #[serde(default)]
    pub notices: Vec<AgentNoticeInfo>,
    /// The first payload this connection receives (attach or reconnect):
    /// the client seeds its seen set from it and rings nothing. Later
    /// payloads ring for ids not seen before. Absent means `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub initial: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl AgentNoticesPayload {
    /// The control message carrying this payload.
    pub fn message(&self) -> serde_json::Result<ServerMessage> {
        Ok(ServerMessage::EndpointControl {
            kind: AGENT_NOTICES_KIND.into(),
            data: serde_json::to_string(self)?,
        })
    }

    /// The payload of an `endpoint.agent-notices.v1` control message;
    /// `None` for malformed data.
    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }
}

/// The framed payloads of one render pass, built on first use.
#[derive(Default)]
pub(super) struct PassFrames {
    update: Option<Vec<u8>>,
    initial: Option<Vec<u8>>,
    notices: Option<Vec<AgentNoticeInfo>>,
}

/// Send `client` the card list when its last sent revision is behind.
/// `Err` when the client's writer is gone or framing failed (the caller
/// drops the client).
pub(super) fn sync_client(
    app: &crate::app::App,
    boot_id: &str,
    client: &mut ClientConnection,
    frames: &mut PassFrames,
) -> Result<(), String> {
    let revision = app.agent_notices.revision();
    if client.shell_agent_notices_sent == Some(revision) {
        return Ok(());
    }
    if revision == 0 {
        // No card ever existed on this server: nothing to say (the
        // connection stays "never sent", so its first payload is `initial`).
        return Ok(());
    }
    let initial = client.shell_agent_notices_sent.is_none();
    let slot = if initial {
        &mut frames.initial
    } else {
        &mut frames.update
    };
    if slot.is_none() {
        let notices = frames
            .notices
            .get_or_insert_with(|| app.agent_notice_infos())
            .clone();
        let payload = AgentNoticesPayload {
            boot_id: boot_id.to_owned(),
            revision,
            notices,
            initial,
        };
        let message = payload.message().map_err(|err| err.to_string())?;
        *slot =
            Some(HeadlessServer::frame_server_message(&message).map_err(|err| err.to_string())?);
    }
    let Some(framed) = slot.as_ref() else {
        return Err("agent notices frame missing".into());
    };
    let Some(writer) = client.writer.as_ref() else {
        return Err("client writer gone".into());
    };
    writer
        .control
        .send(framed.clone())
        .map_err(|_| "client control channel closed".to_string())?;
    client.shell_agent_notices_sent = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_and_initial_defaults_to_false() {
        let payload = AgentNoticesPayload {
            boot_id: "boot".into(),
            revision: 3,
            notices: vec![AgentNoticeInfo {
                id: "n1".into(),
                title: "t".into(),
                name: "api".into(),
                pane_id: "w1:p1".into(),
                unix: 9,
                ..AgentNoticeInfo::default()
            }],
            initial: false,
        };
        let ServerMessage::EndpointControl { kind, data } = payload.message().unwrap() else {
            panic!("expected a control message");
        };
        assert_eq!(kind, AGENT_NOTICES_KIND);
        assert!(!data.contains("initial"), "{data}");
        assert_eq!(AgentNoticesPayload::decode(&data).unwrap(), payload);
        assert_eq!(AgentNoticesPayload::decode("{not json"), None);
        let seeded =
            AgentNoticesPayload::decode(r#"{"boot_id":"b","revision":1,"initial":true}"#).unwrap();
        assert!(seeded.initial && seeded.notices.is_empty());
    }
}
