//! The companion app on the client (fork, companion launcher): what the
//! pinned Browser row says while the server runs the companion, the row
//! menu's companion items, the Browser overlay's companion block (its rows,
//! chips and keys) and the `browser.companion` requests behind them.
//!
//! Everything comes from `browser.get`'s `companion_app` (always current on
//! a server that has it, changed or not); uptimes and countdowns are
//! computed here from its timestamps. A remote endpoint is read-only.

use super::super::*;
use super::{BrowserOverlayRow, BrowserRowState};
use crate::api::schema::{
    BrowserCompanionAction, BrowserCompanionAppInfo, BrowserCompanionParams, BrowserGetInfo, Method,
};
use crate::browser::companion_app::duration_text;

/// The companion profile's name in `browser.get`.
const COMPANION: &str = "companion";

/// How a companion detail line is drawn in the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompanionTone {
    Dim,
    Warn,
    Error,
}

/// A mouse target on the overlay's companion line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CompanionChip {
    pub(crate) label: &'static str,
    pub(crate) action: BrowserCompanionAction,
}

/// The overlay's companion line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompanionOverlayRow {
    /// The API state (`running`, `crashed`, …).
    pub(crate) state: String,
    /// `running · pid 51234 · up 2h 3m · v0.2.0`.
    pub(crate) line: String,
    pub(crate) chips: Vec<CompanionChip>,
}

/// Whether the companion's state is a live process herdr routes to.
pub(crate) fn companion_up(state: &str) -> bool {
    matches!(state, "running" | "external")
}

/// The pinned row's state and status while the server runs the companion;
/// `None` when the herdr+ window says more (the companion is down while a
/// window profile runs).
pub(crate) fn companion_row_state(
    info: &BrowserGetInfo,
    companion: &BrowserCompanionAppInfo,
    now: u64,
) -> Option<(BrowserRowState, String)> {
    let window_runs = info.profiles.iter().any(|profile| {
        profile.name != COMPANION && (profile.state == "running" || profile.state == "starting")
    });
    // a companion that is not running says less than the herdr+ window
    // while that window is on (or still runs)
    if (window_runs || !info.window_off)
        && matches!(companion.state.as_str(), "stopped" | "not_installed")
    {
        return None;
    }
    if info.tabs.iter().any(|tab| tab.dialog_open) {
        return Some((BrowserRowState::Dialog, "dialog open".into()));
    }
    let (state, mut status) = match companion.state.as_str() {
        "running" | "external" => {
            let profile = info.profiles.iter().find(|p| p.name == COMPANION);
            let (tabs, agents) = profile.map_or((0, 0), |p| (p.tabs, p.agents));
            let mut status = String::from("companion");
            if tabs > 0 {
                status.push_str(&format!(
                    " · {tabs} {}",
                    if tabs == 1 { "tab" } else { "tabs" }
                ));
            }
            if agents > 0 {
                status.push_str(&format!(
                    " · {agents} {}",
                    if agents == 1 { "agent" } else { "agents" }
                ));
            }
            (BrowserRowState::Running, status)
        }
        "starting" => (BrowserRowState::Starting, "companion starting".into()),
        "reconnecting" => (BrowserRowState::Starting, "companion reconnecting".into()),
        "stopping" => (BrowserRowState::Starting, "companion stopping".into()),
        "crashed" => (
            BrowserRowState::Starting,
            match companion.next_retry_unix {
                Some(at) => format!(
                    "companion restarting · {}",
                    duration_text(at.saturating_sub(now))
                ),
                None => "companion restarting".into(),
            },
        ),
        "failed" => (BrowserRowState::Crashed, "companion failed".into()),
        "not_installed" => (BrowserRowState::Stopped, "companion not installed".into()),
        "stopped" => (BrowserRowState::Stopped, "companion stopped".into()),
        other => (BrowserRowState::Stopped, format!("companion {other}")),
    };
    if companion.update_ready {
        status.push_str(" · update");
    }
    Some((state, status))
}

/// The row menu's companion items for `state`: (label, action, glyph).
pub(crate) fn companion_menu_items(
    state: &str,
) -> Vec<(&'static str, BrowserCompanionAction, &'static str)> {
    use super::super::context_menu::{GLYPH_ACTIVATE, GLYPH_FOCUS, GLYPH_RESTART, GLYPH_STOP};
    let focus = (
        "Focus companion",
        BrowserCompanionAction::Focus,
        GLYPH_FOCUS,
    );
    let start = (
        "Start companion",
        BrowserCompanionAction::Start,
        GLYPH_ACTIVATE,
    );
    let restart = (
        "Restart companion",
        BrowserCompanionAction::Restart,
        GLYPH_RESTART,
    );
    let stop = ("Stop companion", BrowserCompanionAction::Stop, GLYPH_STOP);
    match state {
        "running" | "external" | "reconnecting" => vec![focus, restart, stop],
        "starting" | "crashed" => vec![restart, stop],
        "failed" => vec![restart],
        "stopped" => vec![start],
        _ => Vec::new(),
    }
}

/// The overlay's chips for `state` (most used first, Stop last).
fn companion_chips(state: &str) -> Vec<CompanionChip> {
    let chip = |label, action| CompanionChip { label, action };
    match state {
        "running" | "external" | "reconnecting" => vec![
            chip("Focus", BrowserCompanionAction::Focus),
            chip("Restart", BrowserCompanionAction::Restart),
            chip("Stop", BrowserCompanionAction::Stop),
        ],
        "starting" | "crashed" => vec![
            chip("Restart", BrowserCompanionAction::Restart),
            chip("Stop", BrowserCompanionAction::Stop),
        ],
        "failed" => vec![chip("Restart", BrowserCompanionAction::Restart)],
        "stopped" => vec![chip("Start", BrowserCompanionAction::Start)],
        _ => Vec::new(),
    }
}

/// The overlay's companion block: the state line and the detail lines that
/// have something to say.
pub(crate) fn companion_overlay_rows(
    companion: &BrowserCompanionAppInfo,
    now: u64,
    home: Option<&str>,
) -> Vec<BrowserOverlayRow> {
    let state = companion.state.as_str();
    let mut parts = vec![match state {
        "external" => "running (started by hand)".to_string(),
        "crashed" => match companion.next_retry_unix {
            Some(at) => format!("restarting in {}", duration_text(at.saturating_sub(now))),
            None => "restarting".into(),
        },
        "not_installed" => "not installed".into(),
        other => other.to_string(),
    }];
    if let Some(pid) = companion.pid {
        parts.push(format!("pid {pid}"));
    }
    if let Some(started) = companion.started_unix.filter(|_| companion_up(state)) {
        parts.push(format!("up {}", duration_text(now.saturating_sub(started))));
    }
    if let Some(version) = companion
        .version
        .as_deref()
        .or(companion.installed.as_deref())
    {
        parts.push(format!("v{version}"));
    }
    let mut rows = vec![BrowserOverlayRow::Companion(CompanionOverlayRow {
        state: companion.state.clone(),
        line: parts.join(" · "),
        chips: companion_chips(state),
    })];
    let note = |text: String, tone| BrowserOverlayRow::CompanionNote { text, tone };
    if state == "not_installed" {
        rows.push(note(
            "run \"herdr browser setup --companion\" in a terminal (downloads Electron, ~115 MB)"
                .into(),
            CompanionTone::Dim,
        ));
        return rows;
    }
    let mut facts = Vec::new();
    if state != "external" {
        facts.push(format!(
            "{} restart{} in 5 min",
            companion.restarts,
            if companion.restarts == 1 { "" } else { "s" }
        ));
    }
    if let Some(log) = companion.log.as_deref() {
        facts.push(shorten_home(log, home));
    }
    if !facts.is_empty() {
        rows.push(note(facts.join(" · "), CompanionTone::Dim));
    }
    if companion.update_ready {
        rows.push(note(
            format!(
                "update ready: {} — Restart applies it",
                companion.installed.as_deref().unwrap_or("?")
            ),
            CompanionTone::Warn,
        ));
    }
    if state == "external" {
        rows.push(note(
            "started by hand: herdr uses it but never stops or restarts it".into(),
            CompanionTone::Dim,
        ));
    }
    if let Some(error) = companion.last_error.as_deref() {
        rows.push(note(format!("last error: {error}"), CompanionTone::Error));
    }
    rows
}

/// `~/…` for a path under the user's home.
fn shorten_home(path: &str, home: Option<&str>) -> String {
    match home.filter(|home| !home.is_empty()) {
        Some(home) => match path.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_string(),
        },
        None => path.to_string(),
    }
}

impl ClientShellState {
    /// The companion's facts from the last `browser.get` (a server that runs it).
    pub(crate) fn browser_companion(&self) -> Option<&BrowserCompanionAppInfo> {
        self.browser.info.as_ref()?.companion_app.as_ref()
    }

    /// `browser.companion {action}` as the user (no caller pane); the reply
    /// refreshes the row and the overlay.
    pub(crate) fn request_browser_companion(
        &mut self,
        action: BrowserCompanionAction,
        outcome: &mut ClientShellInput,
    ) {
        let method = Method::BrowserCompanion(BrowserCompanionParams {
            action,
            caller: None,
        });
        if !self.supports_endpoint_method(&method) {
            self.set_endpoint_error("this server cannot control the companion browser");
            outcome.repaint = true;
            return;
        }
        self.push_endpoint_method_with_kind(method, PendingEndpointKind::BrowserCompanion, outcome);
    }

    /// The overlay's `s`: start a stopped companion, stop a running one.
    pub(super) fn toggle_browser_companion(&mut self, outcome: &mut ClientShellInput) {
        let Some(state) = self.browser_companion().map(|c| c.state.clone()) else {
            return;
        };
        let action = match state.as_str() {
            "stopped" | "failed" => BrowserCompanionAction::Start,
            "not_installed" | "stopping" => return,
            _ => BrowserCompanionAction::Stop,
        };
        self.request_browser_companion(action, outcome);
    }

    /// A `browser.companion` reply: its state at once (the next pull
    /// confirms), a note or an error as a transient message.
    pub(crate) fn handle_browser_companion_result(
        &mut self,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) {
        match result {
            Ok(crate::api::schema::ResponseResult::BrowserCompanion { companion, note }) => {
                if let Some(info) = self.browser.info.as_mut() {
                    info.companion_app = Some(companion);
                }
                if let Some(note) = note {
                    self.set_endpoint_error(note);
                }
            }
            Ok(_) => self.set_endpoint_error("endpoint returned an unexpected companion result"),
            Err(error) => self.set_endpoint_error(error.message),
        }
        self.refresh_browser();
    }
}
