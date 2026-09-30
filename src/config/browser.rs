//! `[browser]`: the herdr-owned Chromium the server launches for agents (fork).
//!
//! Everything is off until something asks for the browser (`autostart =
//! false`); `enabled = false` turns every `browser.*` method into
//! `browser_disabled`. Sizes are clamped with a diagnostic rather than
//! rejected, so a typo never disables the feature.

use serde::Deserialize;

/// The profile a call without `--profile` uses when `default_profile` is unset.
pub const DEFAULT_BROWSER_PROFILE: &str = "main";
/// `executable = "auto"`: look for `Chromium.app` in the usual places.
pub const AUTO_EXECUTABLE: &str = "auto";

pub const MIN_READ_MAX_CHARS: u64 = 2_000;
pub const MAX_READ_MAX_CHARS: u64 = 200_000;
pub const MIN_SCREENSHOT_MAX_PX: u32 = 256;
pub const MAX_SCREENSHOT_MAX_PX: u32 = 8_192;
pub const MIN_LAUNCH_TIMEOUT_MS: u64 = 3_000;
pub const MAX_LAUNCH_TIMEOUT_MS: u64 = 120_000;
pub const MIN_OP_TIMEOUT_MS: u64 = 1_000;
pub const MAX_OP_TIMEOUT_MS: u64 = 600_000;
/// Fewer kept screenshots than this would prune the file just written before
/// the MCP copy is read.
pub const MIN_SCREENSHOT_KEEP: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Whether the browser feature answers at all. Default: true.
    pub enabled: bool,
    /// Start `default_profile` with the server. Default: false (lazy, on first use).
    pub autostart: bool,
    /// `herdr server stop` also closes the browser. Default: false (the window
    /// survives herdr). Read but not yet honoured (v1b wires the shutdown).
    pub stop_with_server: bool,
    /// The profile used when a call names none. Default: `main`.
    pub default_profile: String,
    /// `"auto"` (`~/Applications/herdr+ Browser.app`, `~/Applications/Chromium.app`, then `/Applications/Chromium.app`) or an
    /// absolute path to a `Chromium.app` bundle or its inner binary.
    pub executable: String,
    /// The node binary that runs the sidecar; empty = the one recorded by `herdr browser
    /// setup`, then discovery.
    pub node: String,
    /// Extra switches appended to the Chromium argv.
    pub extra_args: Vec<String>,
    /// Relaunch a profile that has run before with `--restore-last-session`. Default: true.
    pub restore_tabs: bool,
    /// Characters a `read` returns per page when `--max` is not given. 2000..=200000.
    pub read_max_chars: u64,
    /// Characters a `snapshot` returns per page. 2000..=200000.
    pub snapshot_max_chars: u64,
    /// Long edge, in pixels, of the copy an MCP screenshot returns inline. 256..=8192.
    pub screenshot_max_px: u32,
    /// How many herdr-named screenshot files are kept per profile; older ones are pruned.
    pub screenshot_keep: u32,
    /// A tab or pane touched within this many seconds counts as active. Default: 120.
    pub active_seconds: u64,
    /// Whether `browser eval` is allowed. Default: true.
    pub allow_eval: bool,
    /// Whether the `act` family (click, type, press, select, fill, hover) is
    /// allowed. Default: true.
    pub allow_act: bool,
    /// Whether agents may type into password fields. Default: false (the user
    /// logs in by hand).
    pub type_into_password_fields: bool,
    /// A herdr tab whose pane touched the browser within this many seconds
    /// wears the `◎` glyph in the tabs sidebar. Default: 120.
    pub active_glyph_secs: u64,
    /// How long a launch may take to answer `/json/version`. 3000..=120000 ms.
    pub launch_timeout_ms: u64,
    /// The default deadline of one browser operation. 1000..=600000 ms.
    pub op_timeout_ms: u64,
    /// Show agent activity in the window: per-pane tab groups (the companion
    /// extension), the glow frame and the cursor on tabs an agent works on.
    /// Default: true.
    pub show_activity: bool,
    /// The activity overlay's colour (frame border and glow, cursor fill,
    /// ripple), `#rrggbb`. Default: `#aa6eff`. Invalid → the default.
    pub activity_color: String,
    /// The symbol that starts a tab group's title, keyed by canonical agent
    /// id plus `default` (any other agent, or none). Plain Unicode Chrome's
    /// UI font renders. Keys you omit keep their defaults:
    /// claude `✻`, codex `◇`, default `◌`.
    pub group_symbols: std::collections::BTreeMap<String, String>,
}

/// The tab group title symbols keys omit fall back to.
pub const DEFAULT_GROUP_SYMBOLS: [(&str, &str); 3] =
    [("claude", "✻"), ("codex", "◇"), ("default", "◌")];

/// The activity overlay's default colour (the approved purple).
pub const DEFAULT_ACTIVITY_COLOR: &str = "#aa6eff";

/// A `#rrggbb` colour, case-insensitive.
pub fn valid_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            autostart: false,
            stop_with_server: false,
            default_profile: DEFAULT_BROWSER_PROFILE.into(),
            executable: AUTO_EXECUTABLE.into(),
            node: String::new(),
            extra_args: Vec::new(),
            restore_tabs: true,
            read_max_chars: 20_000,
            snapshot_max_chars: 16_000,
            screenshot_max_px: 1_568,
            screenshot_keep: 300,
            active_seconds: 120,
            allow_eval: true,
            allow_act: true,
            type_into_password_fields: false,
            active_glyph_secs: 120,
            launch_timeout_ms: 15_000,
            op_timeout_ms: 30_000,
            show_activity: true,
            activity_color: DEFAULT_ACTIVITY_COLOR.into(),
            group_symbols: std::collections::BTreeMap::new(),
        }
    }
}

impl BrowserConfig {
    /// The tab group symbol for an agent (`default` for none or an unknown
    /// one); a configured value wins over the built-in, blank values are ignored.
    pub fn group_symbol(&self, agent: Option<&str>) -> String {
        let configured = |key: &str| {
            self.group_symbols
                .get(key)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let builtin = |key: &str| {
            DEFAULT_GROUP_SYMBOLS
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        };
        let agent = agent.map(str::trim).filter(|a| !a.is_empty());
        agent
            .and_then(|agent| configured(agent).or_else(|| builtin(agent)))
            .or_else(|| configured("default"))
            .or_else(|| builtin("default"))
            .unwrap_or_default()
    }

    /// The overlay colour, lower-case `#rrggbb`; the default when invalid.
    pub fn activity_color(&self) -> String {
        let value = self.activity_color.trim();
        if valid_hex_color(value) {
            value.to_ascii_lowercase()
        } else {
            DEFAULT_ACTIVITY_COLOR.to_string()
        }
    }

    /// The default profile name: the configured one when valid, else `main`.
    pub fn default_profile(&self) -> &str {
        if valid_profile_name(&self.default_profile) {
            &self.default_profile
        } else {
            DEFAULT_BROWSER_PROFILE
        }
    }

    pub fn read_max_chars(&self) -> u64 {
        self.read_max_chars
            .clamp(MIN_READ_MAX_CHARS, MAX_READ_MAX_CHARS)
    }

    pub fn snapshot_max_chars(&self) -> u64 {
        self.snapshot_max_chars
            .clamp(MIN_READ_MAX_CHARS, MAX_READ_MAX_CHARS)
    }

    pub fn screenshot_max_px(&self) -> u32 {
        self.screenshot_max_px
            .clamp(MIN_SCREENSHOT_MAX_PX, MAX_SCREENSHOT_MAX_PX)
    }

    pub fn launch_timeout_ms(&self) -> u64 {
        self.launch_timeout_ms
            .clamp(MIN_LAUNCH_TIMEOUT_MS, MAX_LAUNCH_TIMEOUT_MS)
    }

    pub fn op_timeout_ms(&self) -> u64 {
        self.op_timeout_ms
            .clamp(MIN_OP_TIMEOUT_MS, MAX_OP_TIMEOUT_MS)
    }

    /// A caller-supplied per-call deadline, kept within the same bounds.
    pub fn clamp_op_timeout_ms(ms: u64) -> u64 {
        ms.clamp(MIN_OP_TIMEOUT_MS, MAX_OP_TIMEOUT_MS)
    }

    pub fn screenshot_keep(&self) -> u32 {
        self.screenshot_keep.max(MIN_SCREENSHOT_KEEP)
    }

    /// The configured node path, if any.
    pub fn node(&self) -> Option<&str> {
        let node = self.node.trim();
        (!node.is_empty()).then_some(node)
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        if !valid_profile_name(&self.default_profile) {
            diagnostics.push(format!(
                "browser.default_profile = {:?} is not a profile name ([a-z][a-z0-9-]{{0,31}}); using \"main\"",
                self.default_profile
            ));
        }
        let clamp_u64 =
            |key: &str, value: u64, min: u64, max: u64, diagnostics: &mut Vec<String>| {
                if value < min || value > max {
                    diagnostics.push(format!(
                        "browser.{key} = {value} is outside {min}..={max}; clamped to {}",
                        value.clamp(min, max)
                    ));
                }
            };
        clamp_u64(
            "read_max_chars",
            self.read_max_chars,
            MIN_READ_MAX_CHARS,
            MAX_READ_MAX_CHARS,
            &mut diagnostics,
        );
        clamp_u64(
            "snapshot_max_chars",
            self.snapshot_max_chars,
            MIN_READ_MAX_CHARS,
            MAX_READ_MAX_CHARS,
            &mut diagnostics,
        );
        clamp_u64(
            "screenshot_max_px",
            u64::from(self.screenshot_max_px),
            u64::from(MIN_SCREENSHOT_MAX_PX),
            u64::from(MAX_SCREENSHOT_MAX_PX),
            &mut diagnostics,
        );
        clamp_u64(
            "launch_timeout_ms",
            self.launch_timeout_ms,
            MIN_LAUNCH_TIMEOUT_MS,
            MAX_LAUNCH_TIMEOUT_MS,
            &mut diagnostics,
        );
        clamp_u64(
            "op_timeout_ms",
            self.op_timeout_ms,
            MIN_OP_TIMEOUT_MS,
            MAX_OP_TIMEOUT_MS,
            &mut diagnostics,
        );
        if self.screenshot_keep < MIN_SCREENSHOT_KEEP {
            diagnostics.push(format!(
                "browser.screenshot_keep = {} is below {MIN_SCREENSHOT_KEEP}; clamped to {MIN_SCREENSHOT_KEEP}",
                self.screenshot_keep
            ));
        }
        if self.executable != AUTO_EXECUTABLE && !self.executable.starts_with('/') {
            diagnostics.push(format!(
                "browser.executable = {:?} is neither \"auto\" nor an absolute path; the browser will not start",
                self.executable
            ));
        }
        if !valid_hex_color(self.activity_color.trim()) {
            diagnostics.push(format!(
                "browser.activity_color = {:?} is not a #rrggbb colour; using {DEFAULT_ACTIVITY_COLOR}",
                self.activity_color
            ));
        }
        for arg in &self.extra_args {
            if is_forbidden_switch(arg) {
                diagnostics.push(format!(
                    "browser.extra_args entry {arg:?} is never passed (it marks the browser as automated or weakens it); dropped"
                ));
            }
        }
        diagnostics
    }

    /// `extra_args` without the switches herdr never passes.
    pub fn extra_args(&self) -> Vec<String> {
        self.extra_args
            .iter()
            .filter(|arg| !is_forbidden_switch(arg))
            .cloned()
            .collect()
    }
}

/// Switches that are never passed to Chromium, whatever the config says: they
/// mark the browser as automated, weaken it, or belong to herdr (the port, the
/// profile dir and the companion extension). The one list the argv filter and
/// the diagnostics share.
pub const FORBIDDEN_SWITCHES: &[&str] = &[
    "--enable-automation",
    "--remote-allow-origins",
    "--headless",
    "--disable-web-security",
    "--remote-debugging-address",
    "--remote-debugging-pipe",
    "--remote-debugging-port",
    "--no-sandbox",
    "--use-mock-keychain",
    "--user-data-dir",
    "--load-extension",
    "--disable-extensions",
];

/// Whether `arg` is a forbidden switch, in its `--switch`, `--switch=value`,
/// `-switch` or `-switch=value` form.
pub fn is_forbidden_switch(arg: &str) -> bool {
    let Some(body) = arg.strip_prefix('-') else {
        return false;
    };
    let body = body.strip_prefix('-').unwrap_or(body);
    let name = body.split_once('=').map(|(name, _)| name).unwrap_or(body);
    FORBIDDEN_SWITCHES
        .iter()
        .any(|switch| switch.trim_start_matches('-') == name)
}

/// A profile name: `[a-z][a-z0-9-]{0,31}`.
pub fn valid_profile_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_lazy_main_auto() {
        let config = BrowserConfig::default();
        assert!(config.enabled);
        assert!(!config.autostart);
        assert!(!config.stop_with_server);
        assert_eq!(config.default_profile(), "main");
        assert_eq!(config.executable, "auto");
        assert_eq!(config.node(), None);
        assert!(config.restore_tabs);
        assert_eq!(config.read_max_chars(), 20_000);
        assert_eq!(config.snapshot_max_chars(), 16_000);
        assert_eq!(config.screenshot_max_px(), 1_568);
        assert_eq!(config.launch_timeout_ms(), 15_000);
        assert_eq!(config.op_timeout_ms(), 30_000);
        assert!(config.allow_eval);
        assert!(config.allow_act);
        assert!(!config.type_into_password_fields);
        assert_eq!(config.active_glyph_secs, 120);
        assert!(config.diagnostics().is_empty());
    }

    #[test]
    fn sizes_clamp_with_a_diagnostic() {
        let config = BrowserConfig {
            read_max_chars: 10,
            snapshot_max_chars: 1_000_000,
            screenshot_max_px: 1,
            launch_timeout_ms: 1,
            op_timeout_ms: 10_000_000,
            screenshot_keep: 0,
            ..BrowserConfig::default()
        };
        assert_eq!(config.screenshot_keep(), MIN_SCREENSHOT_KEEP);
        assert_eq!(BrowserConfig::clamp_op_timeout_ms(1), MIN_OP_TIMEOUT_MS);
        assert_eq!(
            BrowserConfig::clamp_op_timeout_ms(u64::MAX),
            MAX_OP_TIMEOUT_MS
        );
        assert_eq!(BrowserConfig::clamp_op_timeout_ms(5_000), 5_000);
        assert_eq!(config.read_max_chars(), MIN_READ_MAX_CHARS);
        assert_eq!(config.snapshot_max_chars(), MAX_READ_MAX_CHARS);
        assert_eq!(config.screenshot_max_px(), MIN_SCREENSHOT_MAX_PX);
        assert_eq!(config.launch_timeout_ms(), MIN_LAUNCH_TIMEOUT_MS);
        assert_eq!(config.op_timeout_ms(), MAX_OP_TIMEOUT_MS);
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 6, "{diagnostics:?}");
        assert!(diagnostics[0].contains("browser.read_max_chars = 10"));
        assert!(diagnostics[4].contains("clamped to 600000"));
        assert!(diagnostics[5].contains("browser.screenshot_keep = 0"));
    }

    #[test]
    fn bad_profile_name_and_executable_report() {
        let config = BrowserConfig {
            default_profile: "Main Profile".into(),
            executable: "chromium".into(),
            ..BrowserConfig::default()
        };
        assert_eq!(config.default_profile(), "main");
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(diagnostics[0].contains("browser.default_profile"));
        assert!(diagnostics[1].contains("browser.executable"));
    }

    #[test]
    fn forbidden_switches_are_dropped_from_extra_args() {
        let config = BrowserConfig {
            extra_args: vec![
                "--lang=en".into(),
                "--enable-automation".into(),
                "--remote-debugging-port=9222".into(),
                "--headless=new".into(),
                "-no-sandbox".into(),
                "--use-mock-keychain".into(),
                "-user-data-dir=/x".into(),
                "--no-sandboxing-extra".into(),
            ],
            ..BrowserConfig::default()
        };
        assert_eq!(config.extra_args(), ["--lang=en", "--no-sandboxing-extra"]);
        assert_eq!(config.diagnostics().len(), 6);
        assert!(is_forbidden_switch("-headless"));
        assert!(is_forbidden_switch("--remote-debugging-pipe"));
        assert!(is_forbidden_switch("--load-extension=/x"));
        assert!(is_forbidden_switch("--disable-extensions"));
        assert!(!is_forbidden_switch("headless"));
        assert!(!is_forbidden_switch("--lang"));
    }

    #[test]
    fn activity_color_parses_hex_and_falls_back_with_a_diagnostic() {
        let config = BrowserConfig {
            activity_color: " #00C8FF ".into(),
            ..BrowserConfig::default()
        };
        assert_eq!(config.activity_color(), "#00c8ff");
        assert!(config.diagnostics().is_empty());
        assert_eq!(BrowserConfig::default().activity_color(), "#aa6eff");
        for bad in ["aa6eff", "#aa6ef", "#aa6efg", "#aa6eff00", "purple", ""] {
            let config = BrowserConfig {
                activity_color: bad.into(),
                ..BrowserConfig::default()
            };
            assert_eq!(config.activity_color(), DEFAULT_ACTIVITY_COLOR, "{bad:?}");
            let diagnostics = config.diagnostics();
            assert_eq!(diagnostics.len(), 1, "{bad:?}: {diagnostics:?}");
            assert!(
                diagnostics[0].contains("browser.activity_color"),
                "{diagnostics:?}"
            );
        }
        assert!(valid_hex_color("#AbCdEf"));
        assert!(!valid_hex_color("#abcdeff"));
    }

    #[test]
    fn group_symbols_default_per_agent_and_take_overrides() {
        let config = BrowserConfig::default();
        assert_eq!(config.group_symbol(Some("claude")), "✻");
        assert_eq!(config.group_symbol(Some("codex")), "◇");
        assert_eq!(config.group_symbol(Some("pi")), "◌");
        assert_eq!(config.group_symbol(None), "◌");
        let config: BrowserConfig =
            toml::from_str("group_symbols = { claude = \"C\", default = \"*\", codex = \" \" }")
                .unwrap();
        assert_eq!(config.group_symbol(Some("claude")), "C");
        assert_eq!(
            config.group_symbol(Some("codex")),
            "◇",
            "a blank override is ignored"
        );
        assert_eq!(config.group_symbol(Some("pi")), "*");
        assert_eq!(config.group_symbol(None), "*");
        assert!(config.diagnostics().is_empty());
    }

    #[test]
    fn profile_names() {
        for ok in ["main", "work", "tmp-20260930-1201", "a", "a-b-c9"] {
            assert!(valid_profile_name(ok), "{ok}");
        }
        for bad in [
            "",
            "Main",
            "1abc",
            "-x",
            "a b",
            "a_b",
            "x".repeat(33).as_str(),
        ] {
            assert!(!valid_profile_name(bad), "{bad}");
        }
    }

    #[test]
    fn section_deserializes_from_toml() {
        let config: BrowserConfig = toml::from_str(
            "executable = \"/Applications/Chromium.app\"\nautostart = true\nextra_args = [\"--lang=tr\"]\nread_max_chars = 30000\n",
        )
        .unwrap();
        assert!(config.autostart);
        assert_eq!(config.executable, "/Applications/Chromium.app");
        assert_eq!(config.extra_args, ["--lang=tr"]);
        assert_eq!(config.read_max_chars(), 30_000);
        assert!(config.diagnostics().is_empty());
    }
}
