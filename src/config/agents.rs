//! `[agents]`: how herdr+ wraps plain `claude` / `codex` launches in its
//! panes and whether agents may show the user notices (fork).
//!
//! Everything but `notices` and `team_roster` is off by default: with `wrap` off (and no legacy
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
    /// Give claude / codex launched in a team group the team's roster,
    /// purpose and the team tools (the team-only launch wrap; independent
    /// of `wrap`). Default: true.
    pub team_roster: bool,
    /// The context window in tokens per agent (`claude`, `codex`) for the
    /// tabs sidebar's context use; a missing or zero entry uses the
    /// window the session file states (Codex) or the model id and usage
    /// imply (Claude: 200k, 1M for `[1m]` models or usage past 200k).
    /// Applies at the next read. Default: empty.
    pub context_window: std::collections::BTreeMap<String, u64>,
    /// Answer the OSC 7501 Program Status probe and keep what agents report
    /// (Claude Code 2.1.295+), for `herdr agent explain` and the
    /// disagreement log; screen detection still decides the state. Off: no
    /// reply, reports ignored. Applied live. Default: true.
    pub program_status: bool,
    /// `[agents.handoff]`: handoff, then compact, for Claude agents.
    pub handoff: AgentHandoffConfig,
    /// `[agents.autosuspend]`: park Claude agents left idle (fork).
    pub autosuspend: AgentAutosuspendConfig,
}

/// `[agents.autosuspend]` (fork): suspend a Claude agent that has been idle
/// and seen this long with nothing running (src/app/agent_autosuspend.rs);
/// a message to it wakes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentAutosuspendConfig {
    /// `"off"`, or the idle time as `<N>m`, `<N>h` or `<N>d` (for example
    /// `"90m"`, `"1d"`). Default: `"off"`.
    pub after: String,
}

impl Default for AgentAutosuspendConfig {
    fn default() -> Self {
        Self {
            after: AUTOSUSPEND_OFF.to_string(),
        }
    }
}

/// `[agents.autosuspend] after` when it is off.
pub const AUTOSUSPEND_OFF: &str = "off";

/// Parse an `after` value: `Ok(None)` for off (`"off"` or empty), else the
/// idle time; `<N>` is a whole number from 1, at most 365 days in all.
pub fn parse_autosuspend_after(value: &str) -> Result<Option<std::time::Duration>, String> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case(AUTOSUSPEND_OFF) {
        return Ok(None);
    }
    let split = value.len().saturating_sub(1);
    let (number, unit) = (
        value.get(..split).unwrap_or(""),
        value.get(split..).unwrap_or(""),
    );
    let seconds_per = match unit {
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err("the unit must be m, h or d".to_string()),
    };
    let number: u64 = number
        .parse()
        .map_err(|_| "expected a whole number before the unit".to_string())?;
    if number == 0 {
        return Err("the time must be at least 1".to_string());
    }
    let seconds = number
        .checked_mul(seconds_per)
        .filter(|seconds| *seconds <= 365 * 86_400)
        .ok_or_else(|| "at most 365 days".to_string())?;
    Ok(Some(std::time::Duration::from_secs(seconds)))
}

impl AgentAutosuspendConfig {
    /// The idle time after which an agent is suspended; `None` when off or
    /// invalid (an invalid value is a config diagnostic).
    pub fn after(&self) -> Option<std::time::Duration> {
        parse_autosuspend_after(&self.after).ok().flatten()
    }

    pub fn diagnostics(&self) -> Vec<String> {
        match parse_autosuspend_after(&self.after) {
            Ok(_) => Vec::new(),
            Err(err) => vec![format!(
                "agents.autosuspend.after = {:?} is invalid ({err}); expected \"off\" or <N>m, <N>h, <N>d; autosuspend is off",
                self.after
            )],
        }
    }
}

/// `[agents.handoff]` (fork): before a Claude agent's context is compacted,
/// herdr has it write a handoff, saves the transcript, types `/compact` and
/// points the compacted session back at both (src/agent_handoff.rs).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AgentHandoffConfig {
    /// The switch for the whole feature: the automatic trigger, the menu
    /// item, the keybinding and `agents.handoff`. The PreCompact safety net
    /// (a compaction herdr did not start) also needs it. Default: true.
    pub enabled: bool,
    /// Start a handoff once a Claude agent's context use reaches this
    /// percent (`[agents] context_window`); 0 turns the automatic trigger
    /// off. Default: 80.
    pub at_percent: u8,
    /// The prompt line typed to ask for the handoff, `{path}` replaced by
    /// the handoff file's path (a line without it gets the path appended),
    /// for example `/my:handoff {path}`. Empty: herdr's own prompt.
    /// Default: empty.
    pub command: String,
    /// After the compaction, type one line asking the agent to read the
    /// handoff and continue (or pause when nothing is active). Default: true.
    pub resume: bool,
}

impl Default for AgentHandoffConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            at_percent: crate::agent_handoff::DEFAULT_AT_PERCENT,
            command: String::new(),
            resume: true,
        }
    }
}

impl AgentHandoffConfig {
    /// The custom prompt line, trimmed; `None` for herdr's own.
    pub fn command(&self) -> Option<&str> {
        let command = self.command.trim();
        (!command.is_empty()).then_some(command)
    }

    /// The automatic trigger's percent, `None` when off (capped at 100).
    pub fn auto_percent(&self) -> Option<u8> {
        (self.enabled && self.at_percent > 0).then_some(self.at_percent.min(100))
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            wrap: None,
            tools: false,
            instructions: false,
            instructions_file: String::new(),
            notices: true,
            team_roster: true,
            context_window: std::collections::BTreeMap::new(),
            program_status: true,
            handoff: AgentHandoffConfig::default(),
            autosuspend: AgentAutosuspendConfig::default(),
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
        assert!(config.team_roster, "team groups get the roster by default");
        assert_eq!(config.instructions_file(), None);
        assert_eq!(
            Config::default().agents_wrap(),
            (false, WrapSource::Default)
        );
    }

    #[test]
    fn the_section_parses() {
        let config: Config = toml::from_str(
            "[agents]\nwrap = true\ntools = true\ninstructions = true\ninstructions_file = \" ~/a.md \"\nnotices = false\nteam_roster = false\n",
        )
        .unwrap();
        assert!(!config.agents.team_roster);
        assert_eq!(config.agents.wrap, Some(true));
        assert!(config.agents.tools && config.agents.instructions && !config.agents.notices);
        assert_eq!(config.agents.instructions_file(), Some("~/a.md"));
        assert!(config.agents.context_window.is_empty());
        let config: Config =
            toml::from_str("[agents]\ncontext_window = { claude = 1000000 }\n").unwrap();
        assert_eq!(config.agents.context_window.get("claude"), Some(&1_000_000));
    }

    #[test]
    fn the_handoff_section_parses_with_defaults() {
        let config = Config::default();
        assert!(config.agents.handoff.enabled && config.agents.handoff.resume);
        assert_eq!(config.agents.handoff.at_percent, 80);
        assert_eq!(config.agents.handoff.auto_percent(), Some(80));
        assert_eq!(config.agents.handoff.command(), None);
        let config: Config = toml::from_str(
            "[agents.handoff]\nat_percent = 30\ncommand = \" /leap:handoff --compact {path} \"\nresume = false\n",
        )
        .unwrap();
        assert_eq!(config.agents.handoff.auto_percent(), Some(30));
        assert_eq!(
            config.agents.handoff.command(),
            Some("/leap:handoff --compact {path}")
        );
        assert!(!config.agents.handoff.resume);
        let config: Config = toml::from_str("[agents.handoff]\nat_percent = 0\n").unwrap();
        assert_eq!(config.agents.handoff.auto_percent(), None, "0 = no auto");
        assert!(config.agents.handoff.enabled, "the manual trigger stays");
        let config: Config = toml::from_str("[agents.handoff]\nenabled = false\n").unwrap();
        assert_eq!(config.agents.handoff.auto_percent(), None);
        let config: Config = toml::from_str("[agents.handoff]\nat_percent = 250\n").unwrap();
        assert_eq!(config.agents.handoff.auto_percent(), Some(100));
    }

    #[test]
    fn the_autosuspend_section_parses_off_minutes_hours_and_days() {
        let config = Config::default();
        assert_eq!(config.agents.autosuspend.after, "off");
        assert_eq!(config.agents.autosuspend.after(), None, "off by default");
        for (text, seconds) in [
            ("off", None),
            ("", None),
            ("OFF", None),
            ("90m", Some(90 * 60)),
            ("2h", Some(2 * 3600)),
            ("1d", Some(86_400)),
            (" 1d ", Some(86_400)),
            ("365d", Some(365 * 86_400)),
        ] {
            let config: Config =
                toml::from_str(&format!("[agents.autosuspend]\nafter = \"{text}\"\n")).unwrap();
            assert_eq!(
                config
                    .agents
                    .autosuspend
                    .after()
                    .map(|after| after.as_secs()),
                seconds,
                "{text:?}"
            );
            assert!(
                config.agents.autosuspend.diagnostics().is_empty(),
                "{text:?}"
            );
        }
        for bad in ["1", "0m", "1w", "1.5h", "-1d", "366d", "d", "1 d", "1dd"] {
            let config: Config =
                toml::from_str(&format!("[agents.autosuspend]\nafter = \"{bad}\"\n")).unwrap();
            assert_eq!(config.agents.autosuspend.after(), None, "{bad:?}");
            assert_eq!(config.agents.autosuspend.diagnostics().len(), 1, "{bad:?}");
            assert!(
                config
                    .collect_diagnostics()
                    .iter()
                    .any(|d| d.contains("agents.autosuspend.after")),
                "{bad:?}"
            );
        }
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
