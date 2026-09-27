use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS: u64 = 5;
pub(crate) const DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS: u64 = 2;
pub(crate) const MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS: u64 = 31_536_000;
pub(crate) const MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS: u64 = 3_600;
pub(crate) const MAX_TAB_BAR_RIGHT_ENTRIES: usize = 16;
/// Fork: the most trailing output lines a command entry may keep (`lines`).
pub(crate) const MAX_TAB_BAR_COMMAND_LINES: u8 = 4;

fn default_datetime_format() -> String {
    "%H:%M".to_string()
}

fn default_command_interval_seconds() -> u64 {
    DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS
}

fn default_command_timeout_seconds() -> u64 {
    DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS
}

fn default_command_lines() -> u8 {
    1
}

/// Fork: a command entry's `lines`, clamped to 1..=MAX_TAB_BAR_COMMAND_LINES.
pub(crate) fn effective_tab_bar_command_lines(lines: u8) -> u8 {
    lines.clamp(1, MAX_TAB_BAR_COMMAND_LINES)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TabBarRightEntryConfig {
    Zoom,
    Hostname,
    Datetime {
        #[serde(default = "default_datetime_format")]
        format: String,
    },
    Text {
        text: String,
    },
    Command {
        command: String,
        #[serde(default = "default_command_interval_seconds")]
        interval_seconds: u64,
        #[serde(default = "default_command_timeout_seconds")]
        timeout_seconds: u64,
        /// Fork: keep the last `lines` non-empty output lines (1..=4).
        #[serde(default = "default_command_lines")]
        lines: u8,
        /// Fork: keep SGR color and weight sequences instead of stripping them.
        #[serde(default)]
        ansi: bool,
    },
}

pub(crate) fn parse_tab_bar_datetime_format(
    value: &str,
) -> Result<time::format_description::OwnedFormatItem, String> {
    if value.is_empty() {
        return Err("datetime format is empty".into());
    }
    let format = time::format_description::parse_strftime_owned(value)
        .map_err(|err| format!("invalid datetime format: {err}"))?;
    time::PrimitiveDateTime::MIN
        .format(&format)
        .map_err(|err| format!("unsupported datetime format: {err}"))?;
    Ok(format)
}

pub(crate) fn tab_bar_right_diagnostics(entries: &[TabBarRightEntryConfig]) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if entries.len() > MAX_TAB_BAR_RIGHT_ENTRIES {
        diagnostics.push(format!(
            "ui.tab_bar_right may contain at most {MAX_TAB_BAR_RIGHT_ENTRIES} entries; ignoring extras"
        ));
    }

    for (index, entry) in entries.iter().enumerate().take(MAX_TAB_BAR_RIGHT_ENTRIES) {
        match entry {
            TabBarRightEntryConfig::Datetime { format } => {
                if format.is_empty() {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] datetime format is empty; hiding entry"
                    ));
                } else if let Err(err) = parse_tab_bar_datetime_format(format) {
                    diagnostics.push(format!("ui.tab_bar_right[{index}] has {err}; hiding entry"));
                }
            }
            TabBarRightEntryConfig::Command {
                command,
                interval_seconds,
                timeout_seconds,
                lines,
                ansi: _,
            } => {
                if effective_tab_bar_command_lines(*lines) != *lines {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] lines must be between 1 and {MAX_TAB_BAR_COMMAND_LINES}; using {}",
                        effective_tab_bar_command_lines(*lines)
                    ));
                }
                if command.trim().is_empty() {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] command is empty; hiding entry"
                    ));
                }
                if *interval_seconds == 0 {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] interval_seconds must be at least 1; hiding entry"
                    ));
                }
                if *interval_seconds > MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] interval_seconds may be at most {MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS}; hiding entry"
                    ));
                }
                if *timeout_seconds == 0 {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] timeout_seconds must be at least 1; hiding entry"
                    ));
                }
                if *timeout_seconds > MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS {
                    diagnostics.push(format!(
                        "ui.tab_bar_right[{index}] timeout_seconds may be at most {MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS}; hiding entry"
                    ));
                }
            }
            TabBarRightEntryConfig::Zoom
            | TabBarRightEntryConfig::Hostname
            | TabBarRightEntryConfig::Text { .. } => {}
        }
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_bar_entries_parse_with_command_defaults() {
        #[derive(Deserialize)]
        struct Wrapper {
            entries: Vec<TabBarRightEntryConfig>,
        }

        let parsed: Wrapper = toml::from_str(
            r#"
entries = [
  { type = "zoom" },
  { type = "hostname" },
  { type = "datetime", format = "%H:%M" },
  { type = "text", text = "prod" },
  { type = "command", command = "status.sh" },
]
"#,
        )
        .expect("parse tab bar entries");

        assert_eq!(parsed.entries.len(), 5);
        assert!(matches!(
            &parsed.entries[4],
            TabBarRightEntryConfig::Command {
                interval_seconds: DEFAULT_TAB_BAR_COMMAND_INTERVAL_SECONDS,
                timeout_seconds: DEFAULT_TAB_BAR_COMMAND_TIMEOUT_SECONDS,
                ..
            }
        ));
    }

    #[test]
    fn diagnostics_reject_invalid_datetime_and_command_schedules() {
        let entries = vec![
            TabBarRightEntryConfig::Datetime {
                format: "%Q".into(),
            },
            TabBarRightEntryConfig::Datetime {
                format: "%z".into(),
            },
            TabBarRightEntryConfig::Command {
                command: String::new(),
                interval_seconds: 0,
                timeout_seconds: 0,
                lines: 1,
                ansi: false,
            },
            TabBarRightEntryConfig::Command {
                command: "status.sh".into(),
                interval_seconds: MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS + 1,
                timeout_seconds: MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS + 1,
                lines: 1,
                ansi: false,
            },
        ];

        let diagnostics = tab_bar_right_diagnostics(&entries).join("\n");
        assert!(diagnostics.contains("invalid datetime format"));
        assert!(diagnostics.contains("unsupported datetime format"));
        assert!(diagnostics.contains("command is empty"));
        assert!(diagnostics.contains("interval_seconds must be at least 1"));
        assert!(diagnostics.contains("interval_seconds may be at most"));
        assert!(diagnostics.contains("timeout_seconds must be at least 1"));
        assert!(diagnostics.contains("timeout_seconds may be at most"));
        assert!(parse_tab_bar_datetime_format("").is_err());
    }

    #[test]
    fn command_lines_and_ansi_default_to_upstream_and_lines_clamp_with_a_diagnostic() {
        #[derive(Deserialize)]
        struct Wrapper {
            entries: Vec<TabBarRightEntryConfig>,
        }

        let parsed: Wrapper = toml::from_str(
            r#"
entries = [
  { type = "command", command = "status.sh" },
  { type = "command", command = "status.sh", lines = 3, ansi = true },
  { type = "command", command = "status.sh", lines = 0 },
  { type = "command", command = "status.sh", lines = 9 },
]
"#,
        )
        .expect("parse tab bar entries");
        assert!(matches!(
            &parsed.entries[0],
            TabBarRightEntryConfig::Command {
                lines: 1,
                ansi: false,
                ..
            }
        ));
        assert!(matches!(
            &parsed.entries[1],
            TabBarRightEntryConfig::Command {
                lines: 3,
                ansi: true,
                ..
            }
        ));

        let diagnostics = tab_bar_right_diagnostics(&parsed.entries);
        assert_eq!(
            diagnostics,
            vec![
                "ui.tab_bar_right[2] lines must be between 1 and 4; using 1".to_string(),
                "ui.tab_bar_right[3] lines must be between 1 and 4; using 4".to_string(),
            ]
        );
        assert_eq!(effective_tab_bar_command_lines(0), 1);
        assert_eq!(effective_tab_bar_command_lines(3), 3);
        assert_eq!(effective_tab_bar_command_lines(200), 4);
    }
}
