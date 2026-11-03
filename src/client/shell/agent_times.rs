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
//!
//! Fork (restart screen): a pane's `resuming` / `resume_failed` names become
//! a [`ClientResumeMark`] for the suspended card; a name this client does not
//! know is ignored.

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
    /// Fork (autosuspend): the panes whose agent herdr parked for idling.
    pub(crate) auto_suspended: std::collections::HashSet<String>,
    /// Fork (restart screen): the parked panes being relaunched, or whose
    /// relaunch gave up.
    pub(crate) resume_marks: HashMap<String, ClientResumeMark>,
}

/// Fork (restart screen): what the suspended card says about a parked
/// agent's relaunch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientResumeMark {
    /// `agent.restart` owes it a relaunch.
    Restarting,
    /// A message's wake is being retried.
    Waking,
    /// The restart gave up.
    RestartFailed,
    /// The wake gave up.
    WakeFailed,
}

impl ClientResumeMark {
    /// The mark for a pane's pushed names: a relaunch under way wins over
    /// a failed one; unknown names are none.
    fn from_names(resuming: Option<&str>, failed: Option<&str>) -> Option<Self> {
        match resuming {
            Some("restart") => return Some(Self::Restarting),
            Some("wake") => return Some(Self::Waking),
            _ => {}
        }
        match failed {
            Some("restart") => Some(Self::RestartFailed),
            Some("wake") => Some(Self::WakeFailed),
            _ => None,
        }
    }

    pub(crate) fn is_resuming(self) -> bool {
        matches!(self, Self::Restarting | Self::Waking)
    }

    pub(crate) fn is_failed(self) -> bool {
        matches!(self, Self::RestartFailed | Self::WakeFailed)
    }
}

impl ClientAgentTimesState {
    pub(crate) fn from_payload(payload: AgentTimesPayload, received: Instant) -> Self {
        let now_ms = payload.server_now_unix_ms;
        let auto_suspended = payload
            .panes
            .iter()
            .filter(|pane| pane.auto_suspended)
            .map(|pane| pane.pane_id.clone())
            .collect();
        let resume_marks = payload
            .panes
            .iter()
            .filter_map(|pane| {
                ClientResumeMark::from_names(
                    pane.resuming.as_deref(),
                    pane.resume_failed.as_deref(),
                )
                .map(|mark| (pane.pane_id.clone(), mark))
            })
            .collect();
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            auto_suspended,
            resume_marks,
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

    /// Fork (autosuspend): whether herdr parked `pane_id`'s agent for idling.
    pub(crate) fn auto_suspended(&self, pane_id: &str) -> bool {
        !self.auto_suspended.is_empty() && self.auto_suspended.contains(pane_id)
    }

    /// Fork (restart screen): whether any pane is marked restarting or waking.
    pub(crate) fn any_resuming(&self) -> bool {
        self.resume_marks.values().any(|mark| mark.is_resuming())
    }

    /// Fork (restart screen): what the suspended card says about `pane_id`'s relaunch.
    pub(crate) fn resume_mark(&self, pane_id: &str) -> Option<ClientResumeMark> {
        if self.resume_marks.is_empty() {
            return None;
        }
        self.resume_marks.get(pane_id).copied()
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
                        auto_suspended: false,
                        ..Default::default()
                    },
                    AgentTimePane {
                        pane_id: "w1:p2".into(),
                        state_change_seq: 5,
                        // A since after the server's now (clock stepped back).
                        since_unix_ms: 2_000_000,
                        auto_suspended: false,
                        ..Default::default()
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

    #[test]
    fn resume_marks_are_read_from_the_push_and_unknown_names_ignored() {
        let pane = |id: &str, resuming: Option<&str>, failed: Option<&str>| AgentTimePane {
            pane_id: id.into(),
            state_change_seq: 1,
            since_unix_ms: 1,
            resuming: resuming.map(str::to_string),
            resume_failed: failed.map(str::to_string),
            ..Default::default()
        };
        let state = ClientAgentTimesState::from_payload(
            AgentTimesPayload {
                boot_id: "b".into(),
                revision: 1,
                server_now_unix_ms: 2,
                panes: vec![
                    pane("w1:p1", Some("restart"), None),
                    pane("w1:p2", Some("wake"), Some("wake")),
                    pane("w1:p3", None, Some("restart")),
                    pane("w1:p4", None, Some("wake")),
                    pane("w1:p5", Some("phase-2"), Some("later")),
                    pane("w1:p6", None, None),
                ],
            },
            Instant::now(),
        );
        assert_eq!(
            state.resume_mark("w1:p1"),
            Some(ClientResumeMark::Restarting)
        );
        assert_eq!(state.resume_mark("w1:p2"), Some(ClientResumeMark::Waking));
        assert_eq!(
            state.resume_mark("w1:p3"),
            Some(ClientResumeMark::RestartFailed)
        );
        assert_eq!(
            state.resume_mark("w1:p4"),
            Some(ClientResumeMark::WakeFailed)
        );
        assert_eq!(state.resume_mark("w1:p5"), None, "a newer server's name");
        assert_eq!(state.resume_mark("w1:p6"), None);
        assert_eq!(state.resume_mark("w9:p9"), None);
    }
}
