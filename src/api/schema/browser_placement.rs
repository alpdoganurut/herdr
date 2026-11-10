//! Fork (companion launcher): where a client shows the browser.
//!
//! An attached client publishes the part of its terminal grid it keeps for
//! the browser (`browser.placement.set`, client shell); the browser host
//! reads every client's placement with the server's facts about that client
//! (`browser.placements`, a long poll on the connection lane) and says what
//! it does with it (`browser.placement_report`). The names are neutral: a
//! placement is an area of a client's grid, `side` (beside the panes) or
//! `full` (over them); how a client draws around it stays client state.

use serde::{Deserialize, Serialize};

/// How the area relates to the client's panes. The values are inside the
/// `browser.placement.set` digest (a new kind needs a new method name);
/// `Unknown` is the serde fallback (a host ignores such a placement).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPlacementKind {
    /// Beside the panes (they were narrowed for it).
    #[default]
    Side,
    /// Over the panes (they keep their size underneath).
    Full,
    #[serde(other)]
    Unknown,
}

/// A rectangle of terminal cells, origin top left.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
pub struct BrowserCellRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// The client's terminal grid.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
pub struct BrowserGridSize {
    pub cols: u16,
    pub rows: u16,
}

/// A size in device pixels; zero when the terminal does not report it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
pub struct BrowserPxSize {
    pub width: u32,
    pub height: u32,
}

/// One client's placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementSpec {
    /// Random per client process: the client finds the host's report on
    /// its placement by it (`browser.get` `placements`).
    pub id: String,
    pub kind: BrowserPlacementKind,
    /// The herdr tab the client shows; the area belongs to it.
    pub tab_id: String,
    /// Cells of the client's grid.
    pub area: BrowserCellRect,
    pub grid: BrowserGridSize,
    /// The terminal's cell size (`ws_xpixel / cols`, `ws_ypixel / rows`).
    #[serde(default)]
    pub cell_px: BrowserPxSize,
    /// The terminal's pixel extent (`ws_xpixel`, `ws_ypixel`).
    #[serde(default)]
    pub extent_px: BrowserPxSize,
    /// The client process and its ancestors, nearest first (at most 16):
    /// a host on the same machine finds the terminal window by them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pids: Vec<u32>,
    /// `false`: reserved but not to be covered (something of the client's
    /// own is over it, or the shown tab has no area while another has).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub shown: bool,
    /// Bumped when the user asks the host to bind this placement to the
    /// frontmost terminal window.
    #[serde(default, skip_serializing_if = "super::is_zero")]
    pub nudge: u32,
}

/// `browser.placement.set` (client shell): this client's placement, `None`
/// to clear it. Kept until replaced, cleared or the client disconnects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementSetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<BrowserPlacementSpec>,
}

/// `browser.placements` (browser host, connection lane): every placement,
/// at once when the board is newer than `since_rev` (or it is absent),
/// otherwise after the next change or `wait_ms` (at most 25 000).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementsParams {
    pub lease_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_rev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
}

/// A placement with the server's facts about its client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementInfo {
    pub placement: BrowserPlacementSpec,
    /// The client's terminal has the user's focus (its outer focus).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub focused: bool,
    /// The client the server treats as the one in front.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub foreground: bool,
    pub updated_unix: u64,
    /// Fork (compact bar): the client takes `browser.placement_action` (its
    /// handshake says so; an older client does not, and the host hides its
    /// expand and close for that placement).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub placement_actions: bool,
}

/// `browser.placement_report` (browser host, connection lane): what the
/// host does with placement `id`: `covering`, `hidden`, `moving`,
/// `unbound`, `unsupported` or `elsewhere`, with a reason for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementReportParams {
    pub lease_id: String,
    pub id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The host's last report on a placement (`browser.get` `placements`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementStatus {
    pub id: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Fork (compact bar): what the host asks the placement's client to do with
/// its area (`browser.placement_action`): `promote` a side area to a full
/// one, `restore` a full one to a side one, `close` it (a side area goes, a
/// full one gives the panes back). The values are new with the method; a
/// new action needs a new method name. `Unknown` is the serde fallback
/// (answered `invalid_params`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPlacementAction {
    #[default]
    Promote,
    Restore,
    Close,
    #[serde(other)]
    Unknown,
}

/// `browser.placement_action` (browser host, App lane): the user asked the
/// host to change placement `id` (its bar's expand, restore or close). The
/// server relays it to the client that owns the placement, which decides
/// (the area is client state); answered `ok` once relayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPlacementActionParams {
    pub lease_id: String,
    pub id: String,
    pub action: BrowserPlacementAction,
}

/// Fork (browser keys): `browser.view_cycle` (browser host, App lane): the
/// user pressed the browser-view cycle key (`keys.cycle_browser`) while the
/// host had the keyboard. The server relays it to one client, which steps
/// its focused tab's browser like the key does in its terminal (none,
/// docked, Browser view, none): the client owning placement `id` when the
/// host covers one, else the foreground client. Answered `ok` once relayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserViewCycleParams {
    pub lease_id: String,
    /// The placement the host covers, if any (its client takes the cycle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The herdr tab the host shows; a client focused on another tab
    /// ignores the cycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}
