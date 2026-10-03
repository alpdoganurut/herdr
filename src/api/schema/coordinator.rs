//! The coordinator (fork): what `coordinator.get` reports and the parameters
//! of `coordinator.open_dashboard`, `coordinator.wake`, `coordinator.start`
//! and the `coordinator.set_*` setters.
//!
//! Every field past the required core is optional or defaulted, and the closed enums carry an `Unknown` fallback, so
//! an older client can read a newer server's reply.

use serde::{Deserialize, Serialize};

/// The `coordinator.*` method names on the wire.
pub mod method {
    pub const GET: &str = "coordinator.get";
    pub const OPEN: &str = "coordinator.open";
    pub const OPEN_DASHBOARD: &str = "coordinator.open_dashboard";
    pub const WAKE: &str = "coordinator.wake";
    pub const START: &str = "coordinator.start";
    pub const SET_ENABLED: &str = "coordinator.set_enabled";
    pub const SET_WAKE_CAPS: &str = "coordinator.set_wake_caps";
    pub const SET_MODEL: &str = "coordinator.set_model";
    pub const SET_NOTIFY: &str = "coordinator.set_notify";

    /// Every method, in the order of the design's table.
    #[cfg(test)]
    pub const ALL: [&str; 9] = [
        GET,
        OPEN,
        OPEN_DASHBOARD,
        WAKE,
        START,
        SET_ENABLED,
        SET_WAKE_CAPS,
        SET_MODEL,
        SET_NOTIFY,
    ];
}

/// The error codes the `coordinator.*` methods answer with.
pub mod error_code {
    /// `[coordinator] enabled` is false.
    pub const DISABLED: &str = "coordinator_disabled";
    /// Not available here (session not persisted, no coordinator dir, platform).
    pub const UNAVAILABLE: &str = "coordinator_unavailable";
    /// Another server holds the lock, or the registry is corrupt.
    pub const BLOCKED: &str = "coordinator_blocked";
    /// `coordinator.wake` while the coordinator agent is not running.
    pub const NOT_RUNNING: &str = "coordinator_not_running";
    /// The caller is the coordinator pane inside a turn herdr started.
    pub const IN_TURN: &str = "in_coordinator_turn";
    /// A setter could not write the config file.
    pub const CONFIG_WRITE_FAILED: &str = "coordinator_config_write_failed";
    /// `coordinator.set_wake_caps` with a cap out of range.
    pub const INVALID_CAPS: &str = "invalid_caps";
    /// `coordinator.open_dashboard` with no listening dashboard.
    pub const DASHBOARD_UNAVAILABLE: &str = "dashboard_unavailable";
}

/// `CoordinatorGetInfo.blocked_reason` values (`state: blocked`).
pub mod blocked_reason {
    /// Another herdr server (or a legacy `herdr plus run`) holds the lock.
    pub const LOCKED_ELSEWHERE: &str = "locked_elsewhere";
    /// The managed-agent registry cannot be read.
    pub const REGISTRY_CORRUPT: &str = "registry_corrupt";
}

/// `CoordinatorGetInfo.down_reason` values (`state: down`).
pub mod down_reason {
    /// The coordinator pane stayed busy, or `agent.start` failed.
    pub const START_FAILED: &str = "start_failed";
    /// Another live agent holds the name `coordinator`.
    pub const NAME_TAKEN: &str = "name_taken";
    /// The agent did not come up before the launch deadline.
    pub const LAUNCH_TIMEOUT: &str = "launch_timeout";
    /// More relaunches in the last hour than `relaunch_cap_hour`.
    pub const RELAUNCH_CAP: &str = "relaunch_cap";
}

/// The coordinator's lifecycle state as the read model reports it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorStateInfo {
    #[default]
    Off,
    WaitingForLock,
    Starting,
    Running,
    Down,
    Blocked,
    Unavailable,
    /// A state this client does not know (a newer server).
    #[serde(other)]
    Unknown,
}

/// The live non-user turn marker, when one is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorTurnInfo {
    /// `wake` or `message`.
    pub source: String,
    pub id: String,
    /// Unix seconds.
    pub started_at: u64,
}

/// The wake-up counters and caps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorWakeInfo {
    #[serde(default)]
    pub seq: u64,
    /// Wake-ups in the last hour.
    #[serde(default)]
    pub hour: u32,
    /// Wake-ups today.
    #[serde(default)]
    pub day: u32,
    #[serde(default)]
    pub cap_hour: u32,
    #[serde(default)]
    pub cap_day: u32,
    #[serde(default)]
    pub capped: bool,
    /// Queued changes waiting for the next wake-up.
    #[serde(default)]
    pub pending: u32,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_at: Option<u64>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_periodic_at: Option<u64>,
}

/// One agent the coordinator watches (agents v2: in its wake scope; the
/// field keeps its v1 name `managed`). `project` is no longer filled (U3:
/// folded into the note).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorManagedInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// The agent kind (`claude`, `codex`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_change_at: Option<u64>,
}

/// A summary of the coordinator's board; its content is for the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorBoardInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub suggestion_count: u32,
    #[serde(default)]
    pub unread: u32,
}

/// `coordinator.get`'s read model; every coordinator method answers with it
/// (as `ResponseResult::CoordinatorGet { info }`, `{"type": "coordinator_get",
/// "info": {...}}` on the wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorGetInfo {
    pub enabled: bool,
    #[serde(default)]
    pub state: CoordinatorStateInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub down_reason: Option<String>,
    /// A one-time notice (e.g. after the migration from the POC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Only while the dashboard is listening.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard_url: Option<String>,
    /// Why there is no dashboard URL (e.g. `port 7718 in use`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<CoordinatorTurnInfo>,
    #[serde(default)]
    pub wake: CoordinatorWakeInfo,
    #[serde(default)]
    pub relaunches_hour: u32,
    #[serde(default)]
    pub managed: Vec<CoordinatorManagedInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<CoordinatorBoardInfo>,
    #[serde(default)]
    pub unread_suggestions: u32,
    #[serde(default)]
    pub coordinator_dir: String,
    /// `[coordinator] notify`: the coordinator's own notifications.
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Set only on a `coordinator.wake` reply when the wake cannot be
    /// delivered now: why it waits (e.g. `coordinator is working`). The wake
    /// stays queued and is sent once the coordinator is idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_queued: Option<String>,
    /// `[coordinator] wake_scope` (agents v2): which agents wake the
    /// coordinator, and so which ones `managed` lists (`opened`, `teams`,
    /// `all`). Absent from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_scope: Option<String>,
    /// `managed.json` entries the agents-v2 migration could not place on a
    /// pane (their roles and notes were not carried over); absent before the
    /// migration ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_unmatched: Option<u32>,
}

fn default_true() -> bool {
    true
}

impl Default for CoordinatorGetInfo {
    fn default() -> Self {
        Self {
            enabled: false,
            state: CoordinatorStateInfo::default(),
            blocked_reason: None,
            down_reason: None,
            notice: None,
            tab_id: None,
            pane_id: None,
            coordinator_status: None,
            coordinator_session: None,
            model: None,
            dashboard_url: None,
            dashboard_error: None,
            turn: None,
            wake: CoordinatorWakeInfo::default(),
            relaunches_hour: 0,
            managed: Vec::new(),
            board: None,
            unread_suggestions: 0,
            coordinator_dir: String::new(),
            notify: true,
            wake_queued: None,
            wake_scope: None,
            migration_unmatched: None,
        }
    }
}

/// `coordinator.open_dashboard`: clears unread suggestions; with `open`
/// (default true) the server opens the dashboard URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorOpenDashboardParams {
    #[serde(default = "default_true")]
    pub open: bool,
}

impl Default for CoordinatorOpenDashboardParams {
    fn default() -> Self {
        Self { open: true }
    }
}

/// `coordinator.wake`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorWakeParams {
    /// The calling pane (`HERDR_PANE_ID`); absent means the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `coordinator.start`: start, or restart with `resume` (default true).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoordinatorStartParams {
    #[serde(default = "default_true")]
    pub resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

impl Default for CoordinatorStartParams {
    fn default() -> Self {
        Self {
            resume: true,
            caller_pane: None,
        }
    }
}

/// `coordinator.set_enabled` (written to the config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorSetEnabledParams {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `coordinator.set_wake_caps`: both caps are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorSetWakeCapsParams {
    pub cap_hour: u32,
    pub cap_day: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `coordinator.set_model`: `null` is Claude's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorSetModelParams {
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

/// `coordinator.set_notify`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct CoordinatorSetNotifyParams {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_state_falls_back() {
        let info: CoordinatorGetInfo =
            serde_json::from_str(r#"{"enabled":true,"state":"hibernating"}"#).unwrap();
        assert_eq!(info.state, CoordinatorStateInfo::Unknown);
        assert_eq!(info.wake, CoordinatorWakeInfo::default());
        assert!(info.notify, "notify defaults to the config default");
        assert_eq!(
            serde_json::from_str::<CoordinatorGetInfo>(r#"{"enabled":false}"#).unwrap(),
            CoordinatorGetInfo::default()
        );
    }

    #[test]
    fn method_names_are_namespaced_and_distinct() {
        let names: std::collections::HashSet<_> = method::ALL.iter().collect();
        assert_eq!(names.len(), method::ALL.len());
        assert!(method::ALL
            .iter()
            .all(|name| name.starts_with("coordinator.")));
    }

    #[test]
    fn defaults_for_optional_params() {
        let open: CoordinatorOpenDashboardParams = serde_json::from_str("{}").unwrap();
        assert!(open.open);
        let start: CoordinatorStartParams = serde_json::from_str("{}").unwrap();
        assert!(start.resume);
        assert_eq!(start.caller_pane, None);
    }
}
