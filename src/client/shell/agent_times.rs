//! Agent state times on the client (fork): when each pane's agent entered
//! its current state, as the endpoint's last `endpoint.agent-times.v1` push
//! listed it (server side: src/server/headless/agent_times.rs).
//!
//! Each entry becomes an `Instant` once, at receipt: `received - (server_now
//! - since)`, measured on the server's own clock, so clock skew between the
//! client and an SSH server never enters. A time is used only while its
//! `state_change_seq` equals the snapshot agent's (`since_of`): a push that
//! lags the snapshot shows no time, never a wrong one. A state is used only
//! while its `boot_id` is the active snapshot's.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::*;
use crate::server::headless::agent_times::AgentTimesPayload;

/// One endpoint's agent state times, as its last push listed them.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientAgentTimesState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public pane id → (state change seq, when that state began).
    pub(crate) panes: HashMap<String, (u64, Instant)>,
}

impl ClientAgentTimesState {
    pub(crate) fn from_payload(payload: AgentTimesPayload, received: Instant) -> Self {
        let now_ms = payload.server_now_unix_ms;
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            panes: payload
                .panes
                .into_iter()
                .map(|pane| {
                    let age = Duration::from_millis(now_ms.saturating_sub(pane.since_unix_ms));
                    let since = received.checked_sub(age).unwrap_or(received);
                    (pane.pane_id, (pane.state_change_seq, since))
                })
                .collect(),
        }
    }

    /// When `pane_id`'s agent entered its state, only while the push's seq
    /// is `seq` (the snapshot agent's `state_change_seq`).
    pub(crate) fn since_of(&self, pane_id: &str, seq: u64) -> Option<Instant> {
        self.panes
            .get(pane_id)
            .filter(|(pushed, _)| *pushed == seq)
            .map(|(_, since)| *since)
    }
}

/// The active endpoint's agent times, while they belong to the active
/// snapshot's server.
pub(crate) fn active_agent_times_of<'a>(
    times: &'a HashMap<ClientEndpointId, ClientAgentTimesState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientAgentTimesState> {
    let snapshot = snapshot?;
    times
        .get(endpoint_id)
        .filter(|times| times.boot_id == snapshot.boot_id)
}

impl ClientShellState {
    /// An `endpoint.agent-times.v1` push. Returns whether to repaint.
    pub(crate) fn receive_agent_times(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: AgentTimesPayload,
    ) -> bool {
        self.receive_agent_times_at(endpoint_id, payload, Instant::now())
    }

    /// `receive_agent_times` with a pinned receive instant (tests). An older
    /// or equal revision of the same server is ignored; a new server
    /// replaces the state.
    pub(crate) fn receive_agent_times_at(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: AgentTimesPayload,
        received: Instant,
    ) -> bool {
        if self.agent_times.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.agent_times.insert(
            endpoint_id.clone(),
            ClientAgentTimesState::from_payload(payload, received),
        );
        self.sidebar_model.mark_dirty();
        true
    }

    /// When a shown time in state next changes its text (the tabs sidebar's
    /// minute clock); `None` while no time shows.
    pub(crate) fn next_sidebar_clock_deadline(&self) -> Option<Instant> {
        self.hits.sidebar_clock_deadline
    }

    /// Whether the minute clock is due at `now` (the caller repaints).
    pub(crate) fn tick_sidebar_clock(&self, now: Instant) -> bool {
        self.next_sidebar_clock_deadline()
            .is_some_and(|deadline| now >= deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::agent_times::AgentTimePane;

    #[test]
    fn since_is_measured_on_the_server_clock_and_keyed_by_seq() {
        let received = Instant::now() + Duration::from_secs(3600);
        let state = ClientAgentTimesState::from_payload(
            AgentTimesPayload {
                boot_id: "b".into(),
                revision: 1,
                server_now_unix_ms: 1_000_000,
                panes: vec![
                    AgentTimePane {
                        pane_id: "w1:p1".into(),
                        state_change_seq: 4,
                        since_unix_ms: 1_000_000 - 90_000,
                    },
                    AgentTimePane {
                        pane_id: "w1:p2".into(),
                        state_change_seq: 5,
                        // A since after the server's now (clock stepped back).
                        since_unix_ms: 2_000_000,
                    },
                ],
            },
            received,
        );
        assert_eq!(
            state.since_of("w1:p1", 4),
            Some(received - Duration::from_secs(90))
        );
        assert_eq!(state.since_of("w1:p1", 3), None, "a stale seq has no time");
        assert_eq!(state.since_of("w1:p2", 5), Some(received));
        assert_eq!(state.since_of("w9:p9", 1), None);
    }
}
