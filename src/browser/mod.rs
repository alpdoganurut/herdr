//! The herdr browser (fork): a Chromium the server launches for agents, one
//! disposable Playwright sidecar that drives it, and a ledger of who did what
//! to which tab.
//!
//! Layout on disk: `state_dir()/browser/` holds `profiles/<name>/` (the
//! Chromium user data dirs), `profiles/<name>.log`, `profiles.json`,
//! `run/<name>.json` (pid and DevTools port of a launched profile), `host/`
//! (the sidecar assets, its `node_modules`, `runtime.json` and `host.log`)
//! and `shots/<profile>/`. Attribution lives next to `session.json`
//! (`browser.json`, `browser-activity.jsonl`).
//!
//! The hub ([`hub::BrowserHub`]) is process-global: the connection lane
//! (`browser.run`, [`serve`]) and the App lane (`browser.get` and friends,
//! `src/app/browser.rs`) share it.

pub mod host;
pub mod hub;
pub mod launch;
pub mod node;
pub mod profiles;
pub mod serve;
pub mod shape;
pub mod shots;
pub mod state;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use crate::config::BrowserConfig;

/// An error a browser method answers: the socket error code and a one-line
/// message that names the fix where there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserError {
    pub code: String,
    pub message: String,
}

impl BrowserError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn disabled() -> Self {
        Self::new(
            "browser_disabled",
            "the browser is disabled ([browser] enabled = false)",
        )
    }

    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self::new("browser_unavailable", detail)
    }

    pub fn no_current_tab() -> Self {
        Self::new(
            "no_current_tab",
            "this pane has no current browser tab; run `herdr browser open <url>` or pick one with `herdr browser use <tab>` (see `herdr browser tabs`)",
        )
    }

    pub fn tab_not_found(tab: &str) -> Self {
        Self::new(
            "tab_not_found",
            format!("no browser tab {tab}; see `herdr browser tabs`"),
        )
    }

    pub fn tab_closed(tab: &str) -> Self {
        Self::new(
            "tab_closed",
            format!("browser tab {tab} was closed; open another with `herdr browser open <url>`"),
        )
    }

    pub fn profile_not_found(name: &str) -> Self {
        Self::new(
            "profile_not_found",
            format!("no browser profile {name:?}; see `herdr browser profile list`"),
        )
    }

    pub fn invalid_profile(name: &str) -> Self {
        Self::new(
            "invalid_profile",
            format!("{name:?} is not a profile name ([a-z][a-z0-9-]{{0,31}})"),
        )
    }

    pub fn timeout(detail: impl Into<String>) -> Self {
        Self::new("browser_timeout", detail)
    }

    pub fn host_failed(detail: impl Into<String>) -> Self {
        Self::new("host_failed", detail)
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<std::io::Error> for BrowserError {
    fn from(err: std::io::Error) -> Self {
        Self::new("browser_io_error", err.to_string())
    }
}

static HUB: OnceLock<hub::BrowserHub> = OnceLock::new();

/// The process-global hub, created with the default config on first use.
/// `App::new` and `apply_live_config` hand it the loaded `[browser]` section.
pub fn hub() -> &'static hub::BrowserHub {
    HUB.get_or_init(|| hub::BrowserHub::new(BrowserConfig::default()))
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The root of the browser's machine-global state.
pub fn browser_home() -> std::path::PathBuf {
    crate::config::state_dir().join("browser")
}
