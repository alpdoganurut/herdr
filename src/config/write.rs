#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    ToastDelivery(super::ToastDelivery),
    IdleReminderMinutes(u32),
    /// Fork: set (`Some`) or remove (`None`) a `[ui.sound]` file key.
    SoundFile {
        key: &'static str,
        path: Option<&'a str>,
    },
    /// Fork: `ui.daily_reminder_time`, as minutes past midnight.
    DailyReminderTime(u32),
    /// Fork: `news.enabled` (scheduled news runs on or off).
    NewsEnabled(bool),
    /// Fork: `news.interval_hours`.
    NewsIntervalHours(u32),
    /// Fork: `news.quiet_hours`, `HH:MM-HH:MM` or empty for none.
    NewsQuietHours(&'a str),
}

/// Fork: minutes past midnight as a 24-hour "HH:MM".
pub(crate) fn format_time_of_day(minutes: u32) -> String {
    format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60)
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::ToastDelivery(_) => "toast setting",
            Self::IdleReminderMinutes(_) => "reminder setting",
            Self::DailyReminderTime(_) => "daily reminder time",
            Self::SoundFile { .. } => "sound setting",
            Self::NewsEnabled(_) | Self::NewsIntervalHours(_) | Self::NewsQuietHours(_) => {
                "news setting"
            }
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
            Self::IdleReminderMinutes(minutes) => super::upsert_section_value(
                content,
                "ui",
                "idle_reminder_minutes",
                &minutes.to_string(),
            ),
            Self::SoundFile {
                key,
                path: Some(path),
            } => super::upsert_section_value(
                content,
                "ui.sound",
                key,
                &toml::Value::String(path.to_owned()).to_string(),
            ),
            Self::SoundFile { key, path: None } => {
                super::remove_section_key(content, "ui.sound", key)
            }
            Self::DailyReminderTime(minutes) => super::upsert_section_value(
                content,
                "ui",
                "daily_reminder_time",
                &toml::Value::String(format_time_of_day(minutes)).to_string(),
            ),
            Self::NewsEnabled(enabled) => {
                super::upsert_section_bool(content, "news", "enabled", enabled)
            }
            Self::NewsIntervalHours(hours) => {
                super::upsert_section_value(content, "news", "interval_hours", &hours.to_string())
            }
            Self::NewsQuietHours(window) => super::upsert_section_value(
                content,
                "news",
                "quiet_hours",
                &toml::Value::String(window.trim().to_owned()).to_string(),
            ),
        }
    }
}

pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> String,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match super::io::read_optional_config(path) {
        Ok(Some(content)) => content,
        Ok(None) => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    std::fs::write(path, update(&content))
        .map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file_at(&super::config_path(), edit.description(), |content| {
        edit.apply(content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn news_enabled_edit_writes_the_news_section() {
        let edited = ConfigEdit::NewsEnabled(true).apply("[ui]\nsidebar_layout = \"tabs\"\n");
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(config.news.enabled);
        assert_eq!(
            config.ui.sidebar_layout,
            crate::config::SidebarLayoutConfig::Tabs
        );
        let edited = ConfigEdit::NewsEnabled(false).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(!config.news.enabled);
        assert_eq!(edited.matches("[news]").count(), 1);

        let edited = ConfigEdit::NewsIntervalHours(12).apply(&edited);
        let edited = ConfigEdit::NewsQuietHours(" 22:00-08:00 ").apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.news.interval_hours, 12);
        assert_eq!(config.news.quiet_hours, "22:00-08:00");
        let edited = ConfigEdit::NewsQuietHours("").apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(config.news.quiet_hours.is_empty());
        assert_eq!(config.news.quiet_hours(), None);
        assert_eq!(edited.matches("[news]").count(), 1);
    }

    #[test]
    fn sound_file_edit_writes_and_removes_the_sound_key() {
        let content = "[ui.sound]\nenabled = true\n";
        let edited = ConfigEdit::SoundFile {
            key: "reminder_path",
            path: Some("/System/Library/Sounds/Glass.aiff"),
        }
        .apply(content);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(
            config.ui.sound.reminder_path.as_deref(),
            Some(std::path::Path::new("/System/Library/Sounds/Glass.aiff"))
        );
        assert!(config.ui.sound.enabled);
        let edited = ConfigEdit::SoundFile {
            key: "reminder_path",
            path: None,
        }
        .apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.ui.sound.reminder_path, None);
        assert!(!edited.contains("reminder_path"));
    }

    #[test]
    fn daily_reminder_time_edit_writes_a_quoted_24_hour_time() {
        let content = "[ui]\nidle_reminder_minutes = 10\n";
        let edited = ConfigEdit::DailyReminderTime(18 * 60 + 30).apply(content);
        assert!(
            edited.contains("daily_reminder_time = \"18:30\""),
            "{edited}"
        );
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.ui.daily_reminder_time, "18:30");
        assert_eq!(config.ui.idle_reminder_minutes, 10);
        let edited = ConfigEdit::DailyReminderTime(5).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.ui.daily_reminder_time, "00:05");
        assert_eq!(edited.matches("daily_reminder_time").count(), 1);
    }

    #[test]
    fn idle_reminder_minutes_edit_writes_the_ui_key_and_parses_back() {
        let content = "[ui]\nsidebar_width = 30\n";
        let edited = ConfigEdit::IdleReminderMinutes(15).apply(content);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.ui.idle_reminder_minutes, 15);
        assert_eq!(config.ui.sidebar_width, 30);
        let edited = ConfigEdit::IdleReminderMinutes(0).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.ui.idle_reminder_minutes, 0);
        assert_eq!(edited.matches("idle_reminder_minutes").count(), 1);
    }

    #[test]
    fn update_file_at_does_not_move_a_leading_bom_into_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-config-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            b"\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();

        update_file_at(&path, "onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        })
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);

        assert!(
            !written.contains('\u{feff}'),
            "unexpected BOM in {written:?}"
        );
        assert!(
            toml::from_str::<toml::Value>(&written).is_ok(),
            "written config is not valid TOML: {written:?}"
        );
    }
}
