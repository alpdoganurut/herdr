//! Fork (perf watch): `server.perf_summary`, the server process's always-on
//! frame, render and stall counters (src/perf).
//!
//! An optional advertised method: a server without it answers
//! `invalid_request` (unknown method), and a client then shows nothing.
//! Counters are per server process; `pid` + `started_unix_ms` identify the
//! process and `generation` counts resets, so a reader comparing two answers
//! discards the pair when either differs (a live handoff starts a new
//! process at zero).

use serde::{Deserialize, Serialize};

/// The wire name (the `define_operations!` row in src/api/schema.rs).
pub const METHOD: &str = "server.perf_summary";

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerPerfSummaryParams {
    /// Zero the counters after reading them (the answer carries the values
    /// before the reset; the next answer has the new generation).
    #[serde(default, skip_serializing_if = "is_false")]
    pub reset: bool,
}

/// One watched loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerPerfLoopInfo {
    pub name: String,
    /// Busy time that counts as a stall.
    pub threshold_ms: u64,
    /// Stall episodes reported (each logged once as `perf.stall`).
    pub stalls: u64,
    /// The longest busy period seen in a reported stall.
    pub longest_stall_ms: u64,
    /// How long the loop has been busy at this moment (0 while it waits).
    pub busy_ms: u64,
}

/// Frames queued to one attached client. Rows cover the clients attached
/// now: a client that left keeps its frames only in `frames_sent` /
/// `frame_bytes`, and a client that overflowed into the shared row and later
/// got its own slot has its earlier frames in the shared row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerPerfClientInfo {
    /// The server's connection id; absent for the shared bucket of clients
    /// past the first eight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<u64>,
    pub frames: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerPerfSummaryInfo {
    pub pid: u32,
    pub started_unix_ms: u64,
    /// Resets so far in this process.
    pub generation: u64,
    /// Milliseconds the counters cover (since start or the last reset).
    pub window_ms: u64,
    /// Frames accepted by client render channels (full and retained).
    pub frames_sent: u64,
    pub frame_bytes: u64,
    /// Frames a full client channel turned away (backpressure).
    pub frames_deferred: u64,
    /// Complete render passes.
    pub full_renders: u64,
    pub full_render_total_us: u64,
    pub full_render_max_us: u64,
    /// Retained pane-surface passes that skipped a full render.
    pub retained_renders: u64,
    /// Retained attempts that fell back to a full render.
    pub retained_fallbacks: u64,
    /// Synchronized-output frames held for whole parsing.
    pub sync_holds: u64,
    /// Holds released by their deadline rather than the frame's end.
    pub sync_hold_timeouts: u64,
    pub loops: Vec<ServerPerfLoopInfo>,
    /// The currently attached clients (see [`ServerPerfClientInfo`]).
    pub clients: Vec<ServerPerfClientInfo>,
}

impl From<crate::perf::Snapshot> for ServerPerfSummaryInfo {
    fn from(snapshot: crate::perf::Snapshot) -> Self {
        Self {
            pid: snapshot.pid,
            started_unix_ms: snapshot.started_unix_ms,
            generation: snapshot.generation,
            window_ms: snapshot.window_ms,
            frames_sent: snapshot.frames_sent,
            frame_bytes: snapshot.frame_bytes,
            frames_deferred: snapshot.frames_deferred,
            full_renders: snapshot.full_renders,
            full_render_total_us: snapshot.full_render_total_us,
            full_render_max_us: snapshot.full_render_max_us,
            retained_renders: snapshot.retained_renders,
            retained_fallbacks: snapshot.retained_fallbacks,
            sync_holds: snapshot.sync_holds,
            sync_hold_timeouts: snapshot.sync_hold_timeouts,
            loops: snapshot
                .loops
                .into_iter()
                .map(|lp| ServerPerfLoopInfo {
                    name: lp.name.to_owned(),
                    threshold_ms: lp.threshold_ms,
                    stalls: lp.stalls,
                    longest_stall_ms: lp.longest_stall_ms,
                    busy_ms: lp.busy_ms,
                })
                .collect(),
            clients: snapshot
                .clients
                .into_iter()
                .map(|client| ServerPerfClientInfo {
                    client_id: client.client_id,
                    frames: client.frames,
                    bytes: client.bytes,
                })
                .collect(),
        }
    }
}

/// Answers `server.perf_summary` from the process counters (no app state,
/// so the API's connection lane can answer while the main loop is stalled).
pub(crate) fn summary(params: &ServerPerfSummaryParams) -> super::ResponseResult {
    let perf = ServerPerfSummaryInfo::from(crate::perf::snapshot());
    if params.reset {
        crate::perf::reset();
    }
    super::ResponseResult::ServerPerfSummary { perf }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_smoke_perf_a_reset_answers_the_old_values_then_starts_a_generation() {
        crate::perf::record_frame_sent(1, 10);
        let super::super::ResponseResult::ServerPerfSummary { perf: before } =
            summary(&ServerPerfSummaryParams { reset: true })
        else {
            panic!("expected a perf summary");
        };
        assert!(before.frames_sent >= 1, "the answer keeps the values read");
        let super::super::ResponseResult::ServerPerfSummary { perf: after } =
            summary(&ServerPerfSummaryParams::default())
        else {
            panic!("expected a perf summary");
        };
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.frames_sent, 0);
        assert_eq!(after.pid, before.pid);
    }
}
