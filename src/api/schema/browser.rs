//! The herdr browser (fork): the parameters of `browser.run` and its operation
//! set (`BrowserOp`), what `browser.get` gives the client shell, what
//! `browser.status`, `browser.log` and the profile methods answer, and the
//! actor records attribution is made of.
//!
//! `browser.run` is the one method agents call (CLI and MCP); new operations
//! are appended to `BrowserOp` here, `Method` never changes for them.

use serde::{Deserialize, Serialize};

/// Who is calling: the pane whose environment the CLI or the MCP server ran
/// in (`HERDR_PANE_ID`), or `--pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserCaller {
    pub pane_id: String,
}

/// Who did something to a browser tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserActor {
    /// A herdr pane, as it looked when the call was made.
    Pane {
        pane_id: String,
        tab_id: String,
        workspace_id: String,
        #[serde(default)]
        tab_label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// The herdr session name (`default` for the unnamed one).
        #[serde(default)]
        session: String,
        /// The pane no longer exists (re-resolved by `browser.get`).
        #[serde(default, skip_serializing_if = "super::is_false")]
        gone: bool,
        /// The pane's shell process (the PTY child) when the call is
        /// resolved: an MCP server checks it is among its own ancestors.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell_pid: Option<u32>,
    },
    /// The human, in the Chromium window.
    User,
    /// A caller outside any herdr pane (or another herdr server).
    External {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        raw: Option<String>,
    },
}

impl Default for BrowserActor {
    fn default() -> Self {
        BrowserActor::External { raw: None }
    }
}

impl BrowserActor {
    /// The pane id of a pane actor.
    pub fn pane_id(&self) -> Option<&str> {
        match self {
            BrowserActor::Pane { pane_id, .. } => Some(pane_id),
            _ => None,
        }
    }

    /// The herdr tab id of a pane actor.
    pub fn tab_id(&self) -> Option<&str> {
        match self {
            BrowserActor::Pane { tab_id, .. } => Some(tab_id),
            _ => None,
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, BrowserActor::User)
    }

    /// A short human label: `planner · claude`, `you`, `external`.
    pub fn label(&self) -> String {
        match self {
            BrowserActor::Pane {
                tab_label,
                agent,
                pane_id,
                ..
            } => {
                let name = if tab_label.is_empty() {
                    pane_id.clone()
                } else {
                    tab_label.clone()
                };
                match agent {
                    Some(agent) => format!("{name} · {agent}"),
                    None => name,
                }
            }
            BrowserActor::User => "you".into(),
            BrowserActor::External { raw } => match raw {
                Some(raw) => format!("external ({raw})"),
                None => "external".into(),
            },
        }
    }
}

/// One operation of `browser.run`. Unknown operations (an older server) map
/// to `Unknown`, which answers `unknown_op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BrowserOp {
    /// Open a new background tab and make it the caller's current tab.
    Open {
        url: String,
        /// Also select the tab and raise the window.
        #[serde(default, skip_serializing_if = "super::is_false")]
        focus: bool,
        /// `domcontentloaded` (default), `load` or `networkidle`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
    },
    Navigate {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
    },
    /// `back`, `forward` or `reload`.
    History { action: String },
    Read {
        /// `markdown` (default), `text`, `snapshot` or `html`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        /// Return everything (no paging).
        #[serde(default, skip_serializing_if = "super::is_false")]
        all: bool,
        /// Only actionable nodes and their headings (snapshot only).
        #[serde(default, skip_serializing_if = "super::is_false")]
        interactive: bool,
    },
    Find {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<u32>,
    },
    Links {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
    },
    Screenshot {
        #[serde(default, skip_serializing_if = "super::is_false")]
        full: bool,
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// `jpeg` (default) or `png`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// Write the full-size file here instead of `shots/`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        out: Option<String>,
        /// Select the tab and raise the window first (logged as `focus`).
        #[serde(default, skip_serializing_if = "super::is_false")]
        front: bool,
    },
    Console {
        /// `error`, `warn` or `all` (default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
    },
    Network {
        #[serde(default, skip_serializing_if = "super::is_false")]
        failed: bool,
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        match_: Option<String>,
        #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
        type_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u32>,
    },
    Wait {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gone: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        load: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_s: Option<u64>,
    },
    Scroll {
        /// `top`, `bottom` or a ref `eN`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        /// Pixels (negative scrolls up).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<i64>,
    },
    Eval {
        expr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
    },
    /// `accept` (with optional prompt text) or `dismiss` an open dialog.
    Dialog {
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Tabs {
        #[serde(default, skip_serializing_if = "super::is_false")]
        mine: bool,
    },
    /// Make the tab in `BrowserRunParams.tab` the caller's current tab.
    Use,
    /// Close the current (or `BrowserRunParams.tab`) tab.
    Close,
    /// Select the tab and raise the window (for the human).
    Focus,
    /// Click an element (`ref` from a snapshot, or a CSS `selector`).
    Click {
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    /// Type into an element key by key (`clear` first, `submit` presses Enter after).
    Type {
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "super::is_false")]
        submit: bool,
        #[serde(default, skip_serializing_if = "super::is_false")]
        clear: bool,
    },
    /// Press a key (`Enter`, `Control+a`, …) on an element or the focused one.
    Press {
        key: String,
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    /// Pick an option of a `<select>` by value or label.
    Select {
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        value: String,
    },
    /// Set an input's value at once.
    Fill {
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        text: String,
    },
    Hover {
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    /// An aria snapshot with refs (`read --format snapshot`); as a batch step
    /// its refs serve the following steps and its output is kept.
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
        ref_: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offset: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<u64>,
        #[serde(default, skip_serializing_if = "super::is_false")]
        interactive: bool,
    },
    /// Several steps on one tab, in order, under one deadline.
    Batch {
        ops: Vec<BrowserBatchStep>,
        /// Stop at the first failing step (default true).
        #[serde(default = "default_true")]
        stop_on_error: bool,
        /// `snapshot` or `screenshot` after the steps.
        #[serde(default, rename = "final", skip_serializing_if = "Option::is_none")]
        final_: Option<String>,
        /// Close the tabs this batch opened, after the final step (default false).
        #[serde(default, skip_serializing_if = "super::is_false")]
        close_opened: bool,
        /// Glide the activity cursor before each act step (default true; false
        /// skips the glide, the frame and the tab group stay).
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        animate: bool,
    },
    /// Fork (companion browser): show a local HTML file in a sandboxed
    /// view of the caller's session (the companion), else a window tab with
    /// the network routed off; it reloads when the file changes.
    Show {
        /// Absolute path of a `.html` / `.htm` file.
        file: String,
        /// Let the page reach the network (default false: http(s) and ws(s) blocked).
        #[serde(default, skip_serializing_if = "super::is_false")]
        allow_network: bool,
        /// Fork (page API): the page gets `window.herdr.send`, whose messages
        /// reach the caller as page content (default false; not with
        /// `allow_network`).
        #[serde(default, skip_serializing_if = "super::is_false")]
        page_api: bool,
        /// Reload the view when the file or its directory changes (default true).
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        watch: bool,
        /// Also bring the view up for the user.
        #[serde(default, skip_serializing_if = "super::is_false")]
        focus: bool,
        /// The view's title in the strip (default: the document's).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// One step of a batch: the operation plus, when it needs one, its tab
/// (`use`, `close`, `focus`, or a step meant for another tab).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserBatchStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    #[serde(flatten)]
    pub op: BrowserOp,
}

/// The most steps one batch runs.
pub const BATCH_MAX_STEPS: usize = 20;

impl BrowserOp {
    /// The name the activity log uses.
    pub fn name(&self) -> &'static str {
        match self {
            BrowserOp::Open { .. } => "open",
            BrowserOp::Navigate { .. } => "navigate",
            BrowserOp::History { action } => match action.as_str() {
                "back" => "back",
                "forward" => "forward",
                _ => "reload",
            },
            BrowserOp::Read { .. } => "read",
            BrowserOp::Find { .. } => "find",
            BrowserOp::Links { .. } => "links",
            BrowserOp::Screenshot { .. } => "screenshot",
            BrowserOp::Console { .. } => "console",
            BrowserOp::Network { .. } => "network",
            BrowserOp::Wait { .. } => "wait",
            BrowserOp::Scroll { .. } => "scroll",
            BrowserOp::Eval { .. } => "eval",
            BrowserOp::Dialog { .. } => "dialog",
            BrowserOp::Tabs { .. } => "tabs",
            BrowserOp::Use => "use",
            BrowserOp::Close => "close",
            BrowserOp::Focus => "focus",
            BrowserOp::Click { .. } => "act:click",
            BrowserOp::Type { .. } => "act:type",
            BrowserOp::Press { .. } => "act:press",
            BrowserOp::Select { .. } => "act:select",
            BrowserOp::Fill { .. } => "act:fill",
            BrowserOp::Hover { .. } => "act:hover",
            BrowserOp::Snapshot { .. } => "snapshot",
            BrowserOp::Batch { .. } => "batch",
            BrowserOp::Show { .. } => "show",
            BrowserOp::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserRunParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<BrowserCaller>,
    /// The profile to use; switches the caller's cursor to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// `main:t3` or `t3`; unset = the caller's current tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    #[serde(flatten)]
    pub op: BrowserOp,
    /// Overrides `[browser] op_timeout_ms` for this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// What `browser.run` answers: one header line, the shaped text, and the
/// structured payload the same call would print as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserRunResult {
    /// `[t3 · github.com/… · "title"]`, plus hints.
    pub header: String,
    /// The shaped body (paged content, table, matches…); may be empty.
    #[serde(default)]
    pub text: String,
    /// The tab the operation ran on, `profile:tN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    /// A screenshot file (full size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    /// The downscaled copy for inline return, when it differs from `image_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_inline_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_mime: Option<String>,
    /// Structured payload, operation-specific.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
    /// Milliseconds the operation took.
    #[serde(default)]
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserGetParams {
    /// Answer `{unchanged: true}` when nothing changed since this sequence number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostInfo {
    /// `absent`, `starting`, `running` or `failed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playwright: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserProfileInfo {
    pub name: String,
    /// `stopped`, `starting`, `running`, `crashed`, `user_quit` or `in_use`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// Seconds since the Unix epoch when the state was entered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(default)]
    pub tabs: u32,
    #[serde(default)]
    pub agents: u32,
    #[serde(default)]
    pub dialogs: u32,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub temporary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The companion extension (tab groups) on a running profile: `ready`,
    /// `missing` (Chromium ignored --load-extension), `off` (show_activity =
    /// false) or `unsupported (…)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<String>,
}

/// The last agent touch of a tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserTouch {
    pub actor: BrowserActor,
    pub op: String,
    #[serde(default)]
    pub detail: String,
    pub at: u64,
    #[serde(default)]
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserTabInfo {
    /// `profile:tN`.
    pub id: String,
    pub profile: String,
    pub target_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    /// The selected tab of its window.
    #[serde(default)]
    pub selected: bool,
    pub opened_by: BrowserActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<BrowserTouch>,
    pub last_actor: BrowserActor,
    /// Pane ids that touched it, most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub dialog_open: bool,
    #[serde(default, skip_serializing_if = "super::is_zero")]
    pub console_errors: u32,
    /// Touched within `[browser] active_seconds`.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub active: bool,
    /// `open` or `closed`.
    #[serde(default)]
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
}

/// A pane's current browser tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserPaneCursor {
    pub pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// `profile:tN`.
    pub current: String,
    pub last_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserGetInfo {
    pub seq: u64,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub unchanged: bool,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub enabled: bool,
    #[serde(default)]
    pub host: BrowserHostInfo,
    #[serde(default)]
    pub profiles: Vec<BrowserProfileInfo>,
    #[serde(default)]
    pub tabs: Vec<BrowserTabInfo>,
    #[serde(default)]
    pub recent_panes: Vec<BrowserPaneCursor>,
    /// A check that only an explicit request may fix (an MCP registration,
    /// the shell hook) is failing: the Browser row shows a `!` hint.
    #[serde(default)]
    pub setup_needed: bool,
    /// Fork (companion browser): per herdr tab with browser state, its
    /// active page and whether it needs the user (`◉`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<BrowserSessionInfo>,
    /// Fork (companion launcher): the companion app herdr installs and runs.
    /// Absent from an older server, and while the config keeps the
    /// companion off (the browser disabled, or `[browser] host = "window"`
    /// with the window on) and nothing of it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_app: Option<BrowserCompanionAppInfo>,
    /// Fork (companion launcher): the herdr+ window is off (`[browser]
    /// window = false`): clients hide its profiles. An older server never
    /// says so (its window is on).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub window_off: bool,
    /// Fork (companion launcher): the browser host's last report on each
    /// client's placement (`browser.placement_report`), so a client can say
    /// why its area is empty. Always current, also on an `unchanged` answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placements: Vec<super::BrowserPlacementStatus>,
}

/// Fork (companion launcher): what `browser.companion` does. The values are
/// inside the method's frozen digest (a new action needs a new method name);
/// `Unknown` is the serde fallback (answered `invalid_request`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserCompanionAction {
    #[default]
    Status,
    Start,
    Stop,
    Restart,
    Focus,
    #[serde(other)]
    Unknown,
}

/// Fork (companion launcher): `browser.companion`, the controls of the
/// companion app herdr runs (the CLI, the Browser row and overlay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserCompanionParams {
    pub action: BrowserCompanionAction,
    /// The pane a CLI call came from (`HERDR_PANE_ID`): its starts run in
    /// the background and its focus never takes the keyboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<BrowserCaller>,
}

/// Fork (companion launcher): the companion browser app herdr manages
/// (`browser.companion`, `browser.get`). Timestamps are Unix seconds; the
/// client computes uptimes and countdowns from them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserCompanionAppInfo {
    /// `not_installed`, `stopped`, `starting`, `running`, `reconnecting`,
    /// `stopping`, `crashed` (a restart is scheduled), `failed` (herdr gave
    /// up restarting it), `external` (started by hand: herdr routes to it
    /// but never stops or restarts it) or, in a `browser.companion` reply
    /// only, `disabled` (the config keeps it off; the note says why).
    pub state: String,
    /// herdr started this process (its registration carried the launch token).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The version the running process registered with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The installed app version (`herdr browser setup --companion`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    /// The installed Electron version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub electron: Option<String>,
    /// Newer app files are installed than the running process uses: a
    /// restart applies them (nothing restarts by itself).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub update_ready: bool,
    /// When the running process started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_unix: Option<u64>,
    /// When the current state was entered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_unix: Option<u64>,
    /// Crashes within the crash window (5 minutes).
    #[serde(default, skip_serializing_if = "super::is_zero")]
    pub restarts: u32,
    /// `crashed`: when the next start is due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_unix: Option<u64>,
    /// The last failure: how it exited and its last log lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// `[browser] companion_autostart`.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub autostart: bool,
    /// The managed process's log file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// `herdr browser setup`'s record of the installed runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserRuntimeInfo {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playwright_core: Option<String>,
    #[serde(default)]
    pub assets_sha256: String,
    #[serde(default)]
    pub installed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserStatusInfo {
    #[serde(flatten)]
    pub get: BrowserGetInfo,
    /// `state_dir()/browser`.
    pub home: String,
    /// Where the ledger lives (the session data directory).
    #[serde(default)]
    pub ledger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<BrowserRuntimeInfo>,
    /// The embedded assets differ from the installed ones.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub runtime_outdated: bool,
    #[serde(default)]
    pub default_profile: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub log: Vec<BrowserActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrowserActivity {
    pub seq: u64,
    pub at: u64,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    pub actor: BrowserActor,
    pub op: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserLogParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserTabTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub tab: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserProfileTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserStopParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserProfileCreateParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub temporary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserProfileName {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserProfileRecord {
    pub name: String,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub temporary: bool,
    /// The profile directory exists on disk.
    #[serde(default)]
    pub exists: bool,
    #[serde(default)]
    pub state: String,
}

/// How a failing check gets fixed: `safe` fixes (the sidecar's assets and
/// `npm ci`, the companion) also run automatically after a herdr update;
/// `edits_files` fixes (`~/.zshrc`, `~/.claude.json`, Codex's config.toml)
/// only on an explicit request (`browser.fix`, `herdr browser setup`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserFixKind {
    #[default]
    None,
    Safe,
    EditsFiles,
}

/// One doctor check (`browser.settings`, `herdr browser doctor --json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserCheckInfo {
    /// `executable`, `helper`, `extension`, `mcp_claude`, `mcp_codex`, `shell_hook`, `launch_context`.
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub fixable: bool,
    #[serde(default)]
    pub fix_kind: BrowserFixKind,
    /// A failing check whose fix is withheld because agents use the browser
    /// (the extension update): how many, at check time. 0 otherwise.
    #[serde(default)]
    pub waits_for_agents: u32,
}

/// What a fix did (`browser.settings` after `browser.fix`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserFixResult {
    pub id: String,
    pub ok: bool,
    #[serde(default)]
    pub detail: String,
}

/// `browser.settings`: the `[browser]` keys the settings overlay edits, the
/// checks (cached on the server; `checking` while they are being refreshed)
/// and the last fixes (`fixing` while they run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserSettingsInfo {
    pub enabled: bool,
    pub show_activity: bool,
    pub pin_dashboard: bool,
    pub activity_color: String,
    pub steer_agents: bool,
    pub wrap_agents: bool,
    pub disable_native_browser: bool,
    #[serde(default)]
    pub mcp_agents: Vec<String>,
    pub shell_hook: bool,
    /// The default profile and whether it runs (the `open · stop` row).
    pub profile: String,
    pub running: bool,
    /// `running · 3 tabs · 2 agents · profile main`, `stopped`, `off`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub checks: Vec<BrowserCheckInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u64>,
    #[serde(default)]
    pub checking: bool,
    #[serde(default)]
    pub fixing: bool,
    #[serde(default)]
    pub fixes: Vec<BrowserFixResult>,
    /// The server host's name (a remote endpoint's rows say where edits land).
    #[serde(default)]
    pub host: String,
}

/// `browser.settings.set`: one `[browser]` key. `value` is a bool for the
/// toggles, a `#rrggbb` string for `activity_color`, an array of agent names
/// for `mcp_agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserSettingsSetParams {
    pub key: String,
    pub value: serde_json::Value,
}

/// `browser.fix`: the checks to fix; empty = every failing fixable check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserFixParams {
    #[serde(default)]
    pub ids: Vec<String>,
}

// ----- Fork (companion browser): the browser host role ----------------------

/// What kind of browser host registers. Append-closed, `Unknown` the serde
/// fallback (FORK §4).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostKind {
    #[default]
    Companion,
    #[serde(other)]
    Unknown,
}

/// The host's DevTools endpoint (loopback only in v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostCdp {
    pub host: String,
    pub port: u16,
}

/// `browser.host_register`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostRegisterParams {
    #[serde(default)]
    pub kind: BrowserHostKind,
    /// Random per app launch; the same instance re-registering replaces its lease.
    pub instance: String,
    /// Informational only: never signalled, never probed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cdp: BrowserHostCdp,
    /// The control page answers it (`window.__herdrCompanion.nonce()`).
    pub nonce: String,
    #[serde(default)]
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Fork (companion launcher): the token herdr's launcher gave this
    /// process (`--launch-token`); it makes the lease managed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_token: Option<String>,
    /// Fork (companion launcher): the app says it runs managed
    /// (informational: only the token counts).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub managed: bool,
}

/// `browser.host_heartbeat` / `browser.host_unregister`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostLeaseTarget {
    pub lease_id: String,
}

/// The lease `browser.host_register` and `browser.host_heartbeat` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostLeaseInfo {
    pub lease_id: String,
    /// The reserved profile the host serves (`companion`).
    pub profile: String,
    /// `attached`.
    pub state: String,
    pub heartbeat_ms: u64,
    pub ttl_ms: u64,
    pub expires_unix: u64,
    pub host_protocol: u32,
    #[serde(default)]
    pub server_features: Vec<String>,
    /// Fork (companion launcher): a managed companion runs older app files
    /// than are installed: the installed version (a restart applies it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    /// Fork (browser keys): the user's direct (no-prefix) bindings of
    /// `keys.cycle_browser`, as `ctrl+shift+b` labels (modifiers `ctrl`,
    /// `alt`, `shift`, `super` in that order, then the key): pressed in
    /// the host, it sends `browser.view_cycle` (feature `view_cycle`).
    /// Empty when the key is off or only prefixed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub view_cycle_keys: Vec<String>,
}

/// `browser.host_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostStateParams {
    pub lease_id: String,
    /// Only these herdr tabs (default: every tab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_ids: Option<Vec<String>>,
}

/// One page of a herdr tab's browser group. Append-closed kind.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TabBrowserKind {
    #[default]
    Web,
    HtmlView,
    #[serde(other)]
    Unknown,
}

/// One entry of a group, as `browser.host_state` answers it and
/// `browser.host_sync` sends it (`view_id` / `target` only in a sync).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostEntry {
    /// The entry id (`b<n>`); absent for a new entry in a sync (minted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
    /// The CDP target id while the host has the view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub kind: TabBrowserKind,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub allow_network: bool,
    /// Fork (page API): html_view shown with `page_api` (its page may
    /// message its opener).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub page_api: bool,
    /// Fork (page API): the opener's identity at show (herdr's token; a
    /// page message is refused once it no longer matches the pane's agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener: Option<String>,
    /// `user` or `agent:<pane_id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by: Option<String>,
}

/// One herdr tab in `browser.host_state`; `key` / `entries` absent when it
/// has no browser state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostTabState {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<BrowserHostEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostStateInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub tabs: Vec<BrowserHostTabState>,
}

/// One group of `browser.host_sync`: the full live list of a herdr tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostSyncGroup {
    pub tab_id: String,
    /// Absent for a new group (minted, answered in `keys`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The active entry: its `id`, or the `view_id` of a new entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default)]
    pub entries: Vec<BrowserHostEntry>,
}

/// `browser.host_sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostSyncParams {
    pub lease_id: String,
    /// Monotonic per lease; `seq <=` the last applied is `stale_seq`.
    pub seq: u64,
    #[serde(default)]
    pub groups: Vec<BrowserHostSyncGroup>,
}

/// The kind of a `browser.host_report` event. Append-closed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostReportKind {
    #[default]
    UserAction,
    PageError,
    #[serde(other)]
    Unknown,
}

/// One event of `browser.host_report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostReportEvent {
    #[serde(default)]
    pub kind: BrowserHostReportKind,
    /// `navigate`, `back`, `forward`, `reload`, `stop`, `new_tab`,
    /// `close_tab`, `select`, `send_page`, `shown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A page error's kind: `load_failed` or `crashed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A page error's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<u64>,
}

/// `browser.host_report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserHostReportParams {
    pub lease_id: String,
    #[serde(default)]
    pub events: Vec<BrowserHostReportEvent>,
}

/// `browser.send_page`: the user sends the page to an agent of the herdr tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserSendPageParams {
    pub tab_id: String,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
}

/// Fork (page API): `browser.page_message`: a page an agent showed with
/// `page_api` sends that agent a message (`window.herdr.send` in the
/// companion). Only the live companion lease may call it; the server
/// re-checks the entry (html_view, page_api, the file, opened by an agent)
/// and delivers to the opener only, as page content, never as the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserPageMessageParams {
    pub lease_id: String,
    /// The herdr tab whose browser group holds the view.
    pub tab_id: String,
    /// The entry id (`b<n>`), when the host knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The host's view id (an entry the host has not learnt the id of yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_id: Option<String>,
    /// The file the sending document is (the host checked the frame's URL).
    pub file: String,
    /// The page's text (at most 4 000 characters).
    #[serde(default)]
    pub text: String,
    /// The page's data as JSON text (at most 32 KiB), parsed and
    /// re-serialized compact by the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_json: Option<String>,
}

/// A herdr tab's browser session, for the client (`browser.get`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct BrowserSessionInfo {
    pub tab_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub pages: u32,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub needs_you: bool,
    /// `companion` while a live host has the group, else `none`.
    #[serde(default)]
    pub host: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_are_tagged_by_op_and_flatten_into_run_params() {
        let params: BrowserRunParams = serde_json::from_str(
            r#"{"caller":{"pane_id":"w2:pD"},"op":"read","format":"markdown","offset":20000,"tab":"t3"}"#,
        )
        .unwrap();
        assert_eq!(params.tab.as_deref(), Some("t3"));
        match &params.op {
            BrowserOp::Read { format, offset, .. } => {
                assert_eq!(format.as_deref(), Some("markdown"));
                assert_eq!(*offset, Some(20000));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(params.op.name(), "read");
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["op"], "read");
        assert_eq!(json["caller"]["pane_id"], "w2:pD");
    }

    #[test]
    fn unknown_ops_fall_back_and_renamed_fields_round_trip() {
        let params: BrowserRunParams =
            serde_json::from_str(r#"{"op":"teleport","where":"x"}"#).unwrap();
        assert_eq!(params.op, BrowserOp::Unknown);
        let params: BrowserRunParams =
            serde_json::from_str(r#"{"op":"network","match":"api","type":"xhr"}"#).unwrap();
        match params.op {
            BrowserOp::Network { match_, type_, .. } => {
                assert_eq!(match_.as_deref(), Some("api"));
                assert_eq!(type_.as_deref(), Some("xhr"));
            }
            other => panic!("{other:?}"),
        }
        let params: BrowserRunParams =
            serde_json::from_str(r#"{"op":"screenshot","ref":"e4"}"#).unwrap();
        assert!(
            matches!(params.op, BrowserOp::Screenshot { ref ref_, .. } if ref_.as_deref() == Some("e4"))
        );
    }

    #[test]
    fn act_and_batch_ops_round_trip() {
        let params: BrowserRunParams =
            serde_json::from_str(r#"{"op":"type","ref":"e4","text":"hello","submit":true}"#)
                .unwrap();
        assert!(
            matches!(&params.op, BrowserOp::Type { ref_: Some(r), text, submit: true, clear: false, .. } if r == "e4" && text == "hello")
        );
        assert_eq!(params.op.name(), "act:type");
        let batch: BrowserRunParams = serde_json::from_str(
            r##"{"op":"batch","ops":[{"op":"fill","selector":"#name","text":"x"},{"op":"click","ref":"e11"},{"op":"use","tab":"t2"}],"final":"snapshot"}"##,
        )
        .unwrap();
        match &batch.op {
            BrowserOp::Batch {
                ops,
                stop_on_error,
                final_,
                close_opened,
                animate,
            } => {
                assert_eq!(ops.len(), 3);
                assert!(*stop_on_error, "defaults to true");
                assert!(!*close_opened, "defaults to false");
                assert!(*animate, "defaults to true");
                assert_eq!(final_.as_deref(), Some("snapshot"));
                assert!(matches!(ops[0].op, BrowserOp::Fill { .. }));
                assert_eq!(ops[2].tab.as_deref(), Some("t2"));
                assert_eq!(ops[2].op, BrowserOp::Use);
            }
            other => panic!("{other:?}"),
        }
        let back = serde_json::to_string(&batch).unwrap();
        assert!(back.contains("\"final\":\"snapshot\""), "{back}");
        let again: BrowserRunParams = serde_json::from_str(&back).unwrap();
        assert_eq!(again, batch);
        assert_eq!(BATCH_MAX_STEPS, 20);
    }

    #[test]
    fn tab_targets_travel_in_params_for_use_close_and_focus() {
        for (json, op) in [
            (r#"{"op":"use","tab":"t4"}"#, BrowserOp::Use),
            (r#"{"op":"close","tab":"t1"}"#, BrowserOp::Close),
            (r#"{"op":"focus","tab":"main:t2"}"#, BrowserOp::Focus),
        ] {
            let params: BrowserRunParams = serde_json::from_str(json).unwrap();
            assert_eq!(params.op, op, "{json}");
            assert!(params.tab.is_some(), "{json}");
            let back = serde_json::to_string(&params).unwrap();
            assert_eq!(back.matches("\"tab\"").count(), 1, "{back}");
            let again: BrowserRunParams = serde_json::from_str(&back).unwrap();
            assert_eq!(again, params);
        }
        let snap: BrowserRunParams =
            serde_json::from_str(r#"{"op":"snapshot","interactive":true}"#).unwrap();
        assert!(matches!(
            snap.op,
            BrowserOp::Snapshot {
                interactive: true,
                ..
            }
        ));
        assert_eq!(snap.op.name(), "snapshot");
        let bare: BrowserRunParams = serde_json::from_str(r#"{"op":"use"}"#).unwrap();
        assert_eq!(bare.op, BrowserOp::Use);
        assert!(
            bare.tab.is_none(),
            "the handler refuses it, the schema accepts it"
        );
    }

    #[test]
    fn actors_are_tagged_by_kind_and_labelled() {
        let pane = BrowserActor::Pane {
            pane_id: "w2:pD".into(),
            tab_id: "w2:tD".into(),
            workspace_id: "w2".into(),
            tab_label: "planner".into(),
            workspace_label: None,
            agent: Some("claude".into()),
            session: "default".into(),
            gone: false,
            shell_pid: None,
        };
        assert_eq!(pane.label(), "planner · claude");
        assert_eq!(pane.pane_id(), Some("w2:pD"));
        assert_eq!(serde_json::to_value(&pane).unwrap()["kind"], "pane");
        assert_eq!(BrowserActor::User.label(), "you");
        assert_eq!(
            serde_json::to_value(BrowserActor::User).unwrap()["kind"],
            "user"
        );
        assert_eq!(BrowserActor::default().label(), "external");
        let back: BrowserActor = serde_json::from_str(r#"{"kind":"user"}"#).unwrap();
        assert!(back.is_user());
    }
}
