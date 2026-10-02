//! The Agents settings section (fork): what `agents.settings` reports and
//! the parameters of `agents.settings.set` and `agents.fix`.
//!
//! The section edits the server's `[agents]` config keys and shows the
//! wrap's status checks. A fix that edits a user file (`~/.zshrc`) runs only
//! with `confirm: true`; without it the server answers `confirm_required`
//! and writes nothing. Every field past the required core is optional or
//! defaulted, and the closed enums carry an `Unknown` fallback.

use serde::{Deserialize, Serialize};

/// The `agents.*` method names on the wire.
pub mod method {
    pub const SETTINGS: &str = "agents.settings";
    pub const SETTINGS_SET: &str = "agents.settings.set";
    pub const FIX: &str = "agents.fix";
}

/// The error codes the `agents.*` methods answer with.
pub mod error_code {
    /// `agents.fix` asked for a fix that edits a user file without `confirm`.
    pub const CONFIRM_REQUIRED: &str = "confirm_required";
    /// An unknown key, a value of the wrong type, an unknown fix id.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// The config file could not be written, or the reload did not apply it.
    pub const CONFIG_WRITE_FAILED: &str = "agents_config_write_failed";
    /// The fix ran and failed.
    pub const FIX_FAILED: &str = "agents_fix_failed";
}

/// The `agents.settings.set` keys.
pub mod key {
    pub const WRAP: &str = "wrap";
    pub const TOOLS: &str = "tools";
    pub const INSTRUCTIONS: &str = "instructions";
    pub const INSTRUCTIONS_FILE: &str = "instructions_file";
    pub const NOTICES: &str = "notices";
    pub const TEAM_ROSTER: &str = "team_roster";
}

/// The check ids in [`AgentsSettingsInfo::checks`].
pub mod check {
    pub const SHELL_HOOK: &str = "shell_hook";
    pub const CLAUDE: &str = "claude";
    pub const CODEX: &str = "codex";
}

/// Where the effective `wrap` value comes from.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentsWrapSource {
    /// `[agents] wrap`.
    Agents,
    /// The legacy `[browser] wrap_agents` (read only as a fallback).
    BrowserLegacy,
    /// Neither is written: off.
    #[default]
    Default,
    #[serde(other)]
    Unknown,
}

/// A status check's state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentsCheckState {
    /// Installed and current.
    Ok,
    /// Installed but old (the shell hook's file lacks the current marker).
    Outdated,
    /// Not installed (no `.zshrc` line).
    #[default]
    Missing,
    /// Not on the server's PATH (`claude`, `codex`).
    Absent,
    #[serde(other)]
    Unknown,
}

/// One status check (`shell_hook`, `claude`, `codex`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentsCheckInfo {
    pub id: String,
    #[serde(default)]
    pub state: AgentsCheckState,
    #[serde(default)]
    pub detail: String,
    /// `agents.fix` can repair it.
    #[serde(default)]
    pub fixable: bool,
    /// The fix edits a user file: the client must confirm first and send
    /// `agents.fix { confirm: true }`.
    #[serde(default)]
    pub edits_files: bool,
}

/// `agents.settings`: the section's picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentsSettingsInfo {
    /// The effective master switch (`[agents] wrap`, else the legacy key).
    pub wrap: bool,
    #[serde(default)]
    pub wrap_source: AgentsWrapSource,
    /// Add the herdr_agents MCP server to wrapped launches.
    pub tools: bool,
    /// Add the herdr+ paragraph to wrapped launches' system prompt.
    pub instructions: bool,
    /// `""` = the built-in paragraph, else the file that overrides it.
    #[serde(default)]
    pub instructions_file: String,
    /// `built-in`, `~/.config/herdr/agents.md (412 B)`, `missing → built-in`.
    #[serde(default)]
    pub instructions_detail: String,
    /// `[browser] steer_agents` (read-only here; applies while wrapped).
    #[serde(default)]
    pub steer_browser: bool,
    /// Accept `agent.notify` cards (`[agents] notices`).
    #[serde(default = "default_true")]
    pub notices: bool,
    #[serde(default)]
    pub checks: Vec<AgentsCheckInfo>,
    /// The exact `.zshrc` line the shell-hook fix adds, and the file it edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_preview: Option<String>,
    /// Launches in a team group get the roster, purpose and team tools
    /// (`[agents] team_roster`; independent of `wrap`). Absent from older
    /// servers: on, the default.
    #[serde(default = "default_true")]
    pub team_roster: bool,
}

fn default_true() -> bool {
    true
}

/// `agents.settings.set`: one `[agents]` key. `value` is a bool for `wrap`,
/// `tools`, `instructions`, `notices` and `team_roster`; a string for `instructions_file`
/// (`""` or `"default"` = built-in, `"file"` = `<config dir>/agents.md`,
/// seeded with the built-in text when missing, anything else = that path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentsSettingsSetParams {
    pub key: String,
    pub value: serde_json::Value,
}

/// `agents.fix`: the checks to fix; empty = every fixable check. A fix with
/// `edits_files` runs only when `confirm` is true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct AgentsFixParams {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_fall_back_and_params_default() {
        let source: AgentsWrapSource = serde_json::from_str("\"team\"").unwrap();
        assert_eq!(source, AgentsWrapSource::Unknown);
        assert_eq!(
            serde_json::to_value(AgentsWrapSource::BrowserLegacy).unwrap(),
            "browser_legacy"
        );
        let state: AgentsCheckState = serde_json::from_str("\"stale\"").unwrap();
        assert_eq!(state, AgentsCheckState::Unknown);
        let fix: AgentsFixParams = serde_json::from_str("{}").unwrap();
        assert!(fix.ids.is_empty() && !fix.confirm);
        let info: AgentsSettingsInfo =
            serde_json::from_str(r#"{"wrap":false,"tools":false,"instructions":false}"#).unwrap();
        assert!(info.notices);
        assert!(info.team_roster, "an older server's answer reads as on");
        assert_eq!(info.wrap_source, AgentsWrapSource::Default);
    }
}
