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
    #[serde(other)]
    Unknown,
}

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
