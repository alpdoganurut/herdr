//! `[agents]`: how herdr+ wraps plain `claude` / `codex` launches in its
//! panes and whether agents may show the user notices (fork).
//!
//! Everything is off by default: with `wrap` off (and no legacy
//! `[browser] wrap_agents = true`), a launch through the shell hook runs
//! exactly as typed (Codex still gets `--no-daemon`, without which its
//! sessions cannot be attributed to the pane). Changes apply on the next
//! launch; running agents keep what they launched with.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// The master switch: plain `claude` / `codex` in herdr+ panes run
    /// through `herdr agent wrap` and get what the keys below add. Unset:
    /// the legacy `[browser] wrap_agents`, else off.
    pub wrap: Option<bool>,
    /// Add the herdr_agents MCP server (the notify tool) to a wrapped
    /// launch. Default: false.
    pub tools: bool,
    /// Add the herdr+ paragraph to a wrapped launch's system prompt
    /// (Claude) or developer instructions (Codex). Default: false.
    pub instructions: bool,
    /// The paragraph's text: empty = the built-in one; a path (`~`
    /// expanded) replaces it. Default: empty.
    pub instructions_file: String,
    /// Accept agent notices (`agents_notify`, `herdr agent notify`); off
    /// answers `notices_off`. Default: true.
    pub notices: bool,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            wrap: None,
            tools: false,
            instructions: false,
            instructions_file: String::new(),
            notices: true,
        }
    }
}

/// Where the effective `wrap` value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapSource {
    /// `[agents] wrap`.
    Agents,
    /// The legacy `[browser] wrap_agents` (read only as a fallback).
    LegacyBrowser,
    /// Neither is written: off.
    Default,
}

impl WrapSource {
    /// The API spelling (`agents.settings`' `wrap_source`), also the doctor's.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::LegacyBrowser => "browser_legacy",
            Self::Default => "default",
        }
    }
}

impl AgentsConfig {
    /// The configured instructions file, trimmed; `None` for the built-in text.
    pub fn instructions_file(&self) -> Option<&str> {
        let file = self.instructions_file.trim();
        (!file.is_empty()).then_some(file)
    }
}

/// `[agents] wrap`, else the legacy `[browser] wrap_agents`, else off.
pub fn resolve_wrap(agents: Option<bool>, legacy_browser: Option<bool>) -> (bool, WrapSource) {
    match (agents, legacy_browser) {
        (Some(wrap), _) => (wrap, WrapSource::Agents),
        (None, Some(wrap)) => (wrap, WrapSource::LegacyBrowser),
        (None, None) => (false, WrapSource::Default),
    }
}

impl super::Config {
    /// The effective wrap switch and where it comes from.
    pub fn agents_wrap(&self) -> (bool, WrapSource) {
        resolve_wrap(self.agents.wrap, self.browser.wrap_agents)
    }

    /// Copy the effective wrap onto `[browser]` (the browser hub only ever
    /// receives that section). Called by both loaders.
    pub(crate) fn resolve_derived(&mut self) {
        self.browser.effective_wrap = self.agents_wrap().0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn defaults_are_all_off_but_notices() {
        let config = AgentsConfig::default();
        assert_eq!(config.wrap, None);
        assert!(!config.tools && !config.instructions && config.notices);
        assert_eq!(config.instructions_file(), None);
        assert_eq!(
            Config::default().agents_wrap(),
            (false, WrapSource::Default)
        );
    }

    #[test]
    fn the_section_parses() {
        let config: Config = toml::from_str(
            "[agents]\nwrap = true\ntools = true\ninstructions = true\ninstructions_file = \" ~/a.md \"\nnotices = false\n",
        )
        .unwrap();
        assert_eq!(config.agents.wrap, Some(true));
        assert!(config.agents.tools && config.agents.instructions && !config.agents.notices);
        assert_eq!(config.agents.instructions_file(), Some("~/a.md"));
    }

    #[test]
    fn wrap_falls_back_to_the_legacy_browser_key() {
        // legacy true/false/absent × [agents] wrap true/false/absent
        for legacy in [Some(true), Some(false), None] {
            for agents in [Some(true), Some(false), None] {
                let mut text = String::new();
                if let Some(agents) = agents {
                    text.push_str(&format!("[agents]\nwrap = {agents}\n"));
                }
                if let Some(legacy) = legacy {
                    text.push_str(&format!("[browser]\nwrap_agents = {legacy}\n"));
                }
                let config: Config = toml::from_str(&text).unwrap();
                let expected = match (agents, legacy) {
                    (Some(a), _) => (a, WrapSource::Agents),
                    (None, Some(l)) => (l, WrapSource::LegacyBrowser),
                    (None, None) => (false, WrapSource::Default),
                };
                assert_eq!(config.agents_wrap(), expected, "{text}");
            }
        }
        assert_eq!(WrapSource::LegacyBrowser.as_str(), "browser_legacy");
    }
}
