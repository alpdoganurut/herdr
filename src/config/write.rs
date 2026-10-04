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
    /// Fork: `news.times`, the scheduled local times (`HH:MM`, sorted).
    NewsTimes(&'a [String]),
    /// Fork: `news.quiet_hours`, `HH:MM-HH:MM` or empty for none.
    NewsQuietHours(&'a str),
    /// Fork: `coordinator.enabled`.
    CoordinatorEnabled(bool),
    /// Fork: `coordinator.cap_hour` and `coordinator.cap_day`, together.
    CoordinatorWakeCaps {
        cap_hour: u32,
        cap_day: u32,
    },
    /// Fork: `coordinator.model`; `None` removes it.
    CoordinatorModel(Option<&'a str>),
    /// Fork: `coordinator.notify`.
    CoordinatorNotify(bool),
    /// Fork: `ui.sidebar_layout = "tabs"` (the coordinator settings hint).
    SidebarLayoutTabs,
    /// Fork: a `[browser]` toggle (`browser.settings.set`).
    BrowserBool {
        key: &'static str,
        value: bool,
    },
    /// Fork: a `[browser]` string key (`activity_color`).
    BrowserString {
        key: &'static str,
        value: &'a str,
    },
    /// Fork: a `[browser]` list key (`mcp_agents`).
    BrowserList {
        key: &'static str,
        values: &'a [String],
    },
    /// Fork: an `[agents]` toggle (`tools`, `instructions`, `notices`,
    /// `team_roster`).
    AgentsBool {
        key: &'static str,
        value: bool,
    },
    /// Fork: `[agents] wrap`, dropping the legacy `[browser] wrap_agents`
    /// in the same write (the new key wins from then on).
    AgentsWrap(bool),
    /// Fork: `[agents] wrap` (legacy key dropped) plus `[browser]` toggles,
    /// one write (the browser section's `steer_wrap` compatibility key).
    AgentsWrapWithBrowser {
        wrap: bool,
        browser: &'a [(&'static str, bool)],
    },
    /// Fork: `[agents] instructions_file`; `None` removes it (the built-in text).
    AgentsInstructionsFile(Option<&'a str>),
    /// Fork: `ui.sidebar_active_agents` (the tabs sidebar's Active agents block).
    #[allow(dead_code)] // sidebar v2 S0b: the Settings → indicators toggle saves it
    SidebarActiveAgents(bool),
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
            Self::NewsEnabled(_) | Self::NewsTimes(_) | Self::NewsQuietHours(_) => "news setting",
            Self::CoordinatorEnabled(_)
            | Self::CoordinatorWakeCaps { .. }
            | Self::CoordinatorModel(_)
            | Self::CoordinatorNotify(_) => "coordinator setting",
            Self::SidebarLayoutTabs | Self::SidebarActiveAgents(_) => "sidebar setting",
            Self::BrowserBool { .. } | Self::BrowserString { .. } | Self::BrowserList { .. } => {
                "browser setting"
            }
            Self::AgentsBool { .. }
            | Self::AgentsWrap(_)
            | Self::AgentsWrapWithBrowser { .. }
            | Self::AgentsInstructionsFile(_) => "agents setting",
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
            Self::NewsTimes(times) => super::upsert_section_value(
                content,
                "news",
                "times",
                &toml::Value::Array(
                    times
                        .iter()
                        .map(|time| toml::Value::String(time.clone()))
                        .collect(),
                )
                .to_string(),
            ),
            Self::NewsQuietHours(window) => super::upsert_section_value(
                content,
                "news",
                "quiet_hours",
                &toml::Value::String(window.trim().to_owned()).to_string(),
            ),
            Self::CoordinatorEnabled(enabled) => {
                super::upsert_section_bool(content, "coordinator", "enabled", enabled)
            }
            Self::CoordinatorWakeCaps { cap_hour, cap_day } => {
                let content = super::upsert_section_value(
                    content,
                    "coordinator",
                    "cap_hour",
                    &cap_hour.to_string(),
                );
                super::upsert_section_value(
                    &content,
                    "coordinator",
                    "cap_day",
                    &cap_day.to_string(),
                )
            }
            Self::CoordinatorModel(Some(model)) => super::upsert_section_value(
                content,
                "coordinator",
                "model",
                &toml::Value::String(model.trim().to_owned()).to_string(),
            ),
            Self::CoordinatorModel(None) => {
                super::remove_section_key(content, "coordinator", "model")
            }
            Self::CoordinatorNotify(enabled) => {
                super::upsert_section_bool(content, "coordinator", "notify", enabled)
            }
            Self::SidebarLayoutTabs => {
                super::upsert_section_value(content, "ui", "sidebar_layout", "\"tabs\"")
            }
            // The browser edits go through toml_edit: `[browser]  # note`,
            // `[ browser ]`, multi-line arrays and trailing comments survive.
            Self::BrowserBool { key, value } => browser_table_edit(content, |table| {
                set_browser_item(table, key, toml_edit::value(value))
            }),
            Self::BrowserString { key, value } => browser_table_edit(content, |table| {
                set_browser_item(table, key, toml_edit::value(value.trim()))
            }),
            Self::BrowserList { key, values } => browser_table_edit(content, |table| {
                let array: toml_edit::Array = values.iter().map(|v| v.as_str()).collect();
                set_browser_item(table, key, toml_edit::value(array))
            }),
            Self::AgentsBool { key, value } => document_edit(content, |doc| {
                set_section_item(doc, "agents", key, toml_edit::value(value));
            }),
            Self::AgentsWrap(wrap) => document_edit(content, |doc| {
                set_section_item(doc, "agents", "wrap", toml_edit::value(wrap));
                remove_section_item(doc, "browser", "wrap_agents");
            }),
            Self::AgentsWrapWithBrowser { wrap, browser } => document_edit(content, |doc| {
                set_section_item(doc, "agents", "wrap", toml_edit::value(wrap));
                remove_section_item(doc, "browser", "wrap_agents");
                for (key, value) in browser {
                    set_section_item(doc, "browser", key, toml_edit::value(*value));
                }
            }),
            Self::AgentsInstructionsFile(Some(path)) => document_edit(content, |doc| {
                set_section_item(
                    doc,
                    "agents",
                    "instructions_file",
                    toml_edit::value(path.trim()),
                );
            }),
            Self::AgentsInstructionsFile(None) => document_edit(content, |doc| {
                remove_section_item(doc, "agents", "instructions_file");
            }),
            Self::SidebarActiveAgents(enabled) => {
                super::upsert_section_bool(content, "ui", "sidebar_active_agents", enabled)
            }
        }
    }
}

impl ConfigEdit<'_> {
    /// The browser edits are written atomically (temp + rename next to the
    /// real file, mode kept); the others keep the plain write.
    fn atomic(self) -> bool {
        matches!(
            self,
            Self::BrowserBool { .. }
                | Self::BrowserString { .. }
                | Self::BrowserList { .. }
                | Self::AgentsBool { .. }
                | Self::AgentsWrap(_)
                | Self::AgentsWrapWithBrowser { .. }
                | Self::AgentsInstructionsFile(_)
        )
    }
}

/// `content` as a toml_edit document with `edit` applied: comments, spacing
/// and untouched tables stay; a file that does not parse comes back
/// unchanged (the caller notices the value did not apply).
pub(crate) fn document_edit(
    content: &str,
    edit: impl FnOnce(&mut toml_edit::DocumentMut),
) -> String {
    let (bom, body) = match content.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", content),
    };
    let Ok(mut doc) = body.parse::<toml_edit::DocumentMut>() else {
        return content.to_string();
    };
    edit(&mut doc);
    format!("{bom}{doc}")
}

/// Set `key` in the top-level table `section` (appended when missing),
/// keeping an existing value's decor. A `section` that is not a table is
/// left alone.
fn set_section_item(
    doc: &mut toml_edit::DocumentMut,
    section: &str,
    key: &str,
    item: toml_edit::Item,
) {
    use toml_edit::{Item, Table};
    if doc.get(section).is_none() {
        doc.insert(section, Item::Table(Table::new()));
    }
    if let Some(table) = doc.get_mut(section).and_then(Item::as_table_like_mut) {
        set_browser_item(table, key, item);
    }
}

/// Remove `key` from the top-level table `section`, if both are there.
fn remove_section_item(doc: &mut toml_edit::DocumentMut, section: &str, key: &str) {
    if let Some(table) = doc
        .get_mut(section)
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        table.remove(key);
    }
}

/// `content` with `edit` applied to its `[browser]` table as a toml_edit
/// document: comments, spacing and every other table stay; a missing table
/// is appended; a file that does not parse comes back unchanged (the
/// caller notices the value did not apply).
pub(crate) fn browser_table_edit(
    content: &str,
    edit: impl FnOnce(&mut dyn toml_edit::TableLike),
) -> String {
    use toml_edit::{DocumentMut, Item, Table};
    let (bom, body) = match content.strip_prefix('\u{feff}') {
        Some(body) => ("\u{feff}", body),
        None => ("", content),
    };
    let Ok(mut doc) = body.parse::<DocumentMut>() else {
        return content.to_string();
    };
    if doc.get("browser").is_none() {
        doc.insert("browser", Item::Table(Table::new()));
    }
    let Some(table) = doc.get_mut("browser").and_then(Item::as_table_like_mut) else {
        return content.to_string();
    };
    edit(table);
    format!("{bom}{doc}")
}

/// Set `key` in a `[browser]` table, keeping an existing value's decor (a
/// trailing `# comment` stays on its line).
pub(crate) fn set_browser_item(
    table: &mut dyn toml_edit::TableLike,
    key: &str,
    mut item: toml_edit::Item,
) {
    if let Some(old) = table.get(key).and_then(toml_edit::Item::as_value) {
        if let Some(new) = item.as_value_mut() {
            *new.decor_mut() = old.decor().clone();
        }
    }
    table.insert(key, item);
}

/// Like `update_file_at`, through a temp file renamed over the real file
/// (a symlink is followed; the mode is kept).
pub(crate) fn update_file_atomic_at(
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
    let next = update(&content);
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = real
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".into());
    let tmp = real.with_file_name(format!(".{name}.herdr-{}", std::process::id()));
    let mode = std::fs::metadata(&real).ok().map(|m| m.permissions());
    let written = std::fs::write(&tmp, next).and_then(|()| {
        if let Some(mode) = mode {
            std::fs::set_permissions(&tmp, mode)?;
        }
        std::fs::rename(&tmp, &real)
    });
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("failed to save {description}: {error}"));
    }
    Ok(())
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
    if edit.atomic() {
        return update_file_atomic_at(&super::config_path(), edit.description(), |content| {
            edit.apply(content)
        });
    }
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

        let times = ["07:30".to_string(), "18:00".to_string()];
        let edited = ConfigEdit::NewsTimes(&times).apply(&edited);
        let edited = ConfigEdit::NewsQuietHours(" 22:00-08:00 ").apply(&edited);
        assert!(
            edited.contains("times = [\"07:30\", \"18:00\"]"),
            "{edited}"
        );
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.news.times, times);
        assert_eq!(config.news.quiet_hours, "22:00-08:00");
        let edited = ConfigEdit::NewsTimes(&[]).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(config.news.times.is_empty(), "{edited}");
        assert_eq!(edited.matches("times").count(), 1);
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
    fn sidebar_active_agents_edit_writes_the_ui_key_and_parses_back() {
        let content = "[ui]\nsidebar_width = 30\n";
        assert!(crate::config::Config::default().ui.sidebar_active_agents);
        let edited = ConfigEdit::SidebarActiveAgents(false).apply(content);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(!config.ui.sidebar_active_agents);
        assert_eq!(config.ui.sidebar_width, 30);
        let edited = ConfigEdit::SidebarActiveAgents(true).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(config.ui.sidebar_active_agents);
        assert_eq!(edited.matches("sidebar_active_agents").count(), 1);
        assert_eq!(
            ConfigEdit::SidebarActiveAgents(true).description(),
            "sidebar setting"
        );
    }

    #[test]
    fn browser_edits_keep_comments_spacing_and_other_tables() {
        let original = "# top\n[ui]\nsidebar_layout = \"tabs\" # keep\n\n[ browser ]  # my browser\nexecutable = \"auto\"\npin_dashboard = true  # trailing\nmcp_agents = [\n  \"claude\",\n  \"codex\",\n]\n\n[news]\nenabled = false\n";
        let edited = ConfigEdit::BrowserBool {
            key: "pin_dashboard",
            value: false,
        }
        .apply(original);
        assert!(edited.contains("[ browser ]  # my browser\n"), "{edited}");
        assert!(
            edited.contains("pin_dashboard = false  # trailing\n"),
            "{edited}"
        );
        assert!(
            edited.contains("sidebar_layout = \"tabs\" # keep\n"),
            "{edited}"
        );
        assert!(edited.starts_with("# top\n"), "{edited}");
        assert!(edited.contains("[news]\nenabled = false\n"), "{edited}");
        assert!(
            edited.contains("mcp_agents = [\n  \"claude\",\n  \"codex\",\n]\n"),
            "untouched keys keep their layout: {edited}"
        );
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(!config.browser.pin_dashboard);
        assert_eq!(config.browser.executable, "auto");
        // a list over a multi-line one, a string, two bools
        let agents = vec!["codex".to_string()];
        let edited = ConfigEdit::BrowserList {
            key: "mcp_agents",
            values: &agents,
        }
        .apply(&edited);
        let edited = ConfigEdit::BrowserString {
            key: "activity_color",
            value: " #00c8ff ",
        }
        .apply(&edited);
        let edited = ConfigEdit::BrowserBool {
            key: "steer_agents",
            value: false,
        }
        .apply(&edited);
        let edited = ConfigEdit::BrowserBool {
            key: "disable_native_browser",
            value: false,
        }
        .apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.browser.mcp_agents, ["codex"]);
        assert_eq!(config.browser.activity_color, "#00c8ff");
        assert!(!config.browser.steer_agents && !config.browser.disable_native_browser);
        assert_eq!(
            edited.matches("[ browser ]").count(),
            1,
            "one [browser] header: {edited}"
        );
        assert!(!edited.contains("\n[browser]"), "{edited}");
        assert!(
            edited.contains("pin_dashboard = false  # trailing\n"),
            "{edited}"
        );
        // no [browser] yet: appended; a commented header is not a header
        let fresh = ConfigEdit::BrowserBool {
            key: "enabled",
            value: false,
        }
        .apply("# [browser]\n# enabled = true\n[ui]\nsidebar_layout = \"tabs\"\n");
        assert!(fresh.contains("\n[browser]\nenabled = false\n"), "{fresh}");
        assert!(
            fresh.starts_with("# [browser]\n# enabled = true\n"),
            "{fresh}"
        );
        let config: crate::config::Config = toml::from_str(&fresh).unwrap();
        assert!(!config.browser.enabled);
        // a header with a comment and an inline `browser = {}` both take the key
        let inline = ConfigEdit::BrowserBool {
            key: "enabled",
            value: false,
        }
        .apply("browser = { executable = \"auto\" }\n");
        let config: crate::config::Config = toml::from_str(&inline).unwrap();
        assert!(!config.browser.enabled);
        assert_eq!(config.browser.executable, "auto");
        // a file that does not parse comes back unchanged
        assert_eq!(
            ConfigEdit::BrowserBool {
                key: "enabled",
                value: false
            }
            .apply("[broken\n"),
            "[broken\n"
        );
    }

    #[test]
    fn agents_wrap_writes_the_new_key_and_drops_the_legacy_one_in_one_edit() {
        let original = "# top\n[browser]  # mine\nwrap_agents = true # old\nsteer_agents = true\n\n[news]\nenabled = false\n";
        let before: crate::config::Config = toml::from_str(original).unwrap();
        assert_eq!(
            before.agents_wrap(),
            (true, crate::config::WrapSource::LegacyBrowser)
        );
        let edited = ConfigEdit::AgentsWrap(false).apply(original);
        assert!(!edited.contains("wrap_agents"), "{edited}");
        assert!(edited.contains("[browser]  # mine\n"), "{edited}");
        assert!(edited.contains("[news]\nenabled = false\n"), "{edited}");
        assert!(edited.contains("[agents]\nwrap = false\n"), "{edited}");
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(
            config.agents_wrap(),
            (false, crate::config::WrapSource::Agents)
        );
        assert!(config.browser.steer_agents);
        // again: one [agents] header, the value flips in place
        let edited = ConfigEdit::AgentsWrap(true).apply(&edited);
        assert_eq!(edited.matches("[agents]").count(), 1, "{edited}");
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.agents.wrap, Some(true));
        // the steer_wrap compatibility edit: both sections, one write
        let edited = ConfigEdit::AgentsWrapWithBrowser {
            wrap: false,
            browser: &[("steer_agents", false)],
        }
        .apply("[browser]\nwrap_agents = true\n");
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert_eq!(config.agents.wrap, Some(false));
        assert_eq!(config.browser.wrap_agents, None);
        assert!(!config.browser.steer_agents);
    }

    #[test]
    fn agents_toggles_and_the_instructions_file_round_trip() {
        let edited = ConfigEdit::AgentsBool {
            key: "tools",
            value: true,
        }
        .apply("[ui]\nsidebar_layout = \"tabs\"\n");
        let edited = ConfigEdit::AgentsBool {
            key: "notices",
            value: false,
        }
        .apply(&edited);
        let edited = ConfigEdit::AgentsBool {
            key: "team_roster",
            value: false,
        }
        .apply(&edited);
        let edited = ConfigEdit::AgentsInstructionsFile(Some(" ~/x/agents.md ")).apply(&edited);
        let config: crate::config::Config = toml::from_str(&edited).unwrap();
        assert!(config.agents.tools && !config.agents.notices);
        assert!(!config.agents.team_roster);
        assert_eq!(config.agents.instructions_file(), Some("~/x/agents.md"));
        assert_eq!(
            config.ui.sidebar_layout,
            crate::config::SidebarLayoutConfig::Tabs
        );
        let edited = ConfigEdit::AgentsInstructionsFile(None).apply(&edited);
        assert!(!edited.contains("instructions_file"), "{edited}");
        assert_eq!(edited.matches("[agents]").count(), 1, "{edited}");
        // a file that does not parse comes back unchanged
        assert_eq!(ConfigEdit::AgentsWrap(true).apply("[broken\n"), "[broken\n");
    }

    #[test]
    fn browser_edits_are_written_atomically_through_a_symlink_keeping_the_mode() {
        let dir = std::env::temp_dir().join(format!("herdr-config-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::create_dir_all(dir.join("link")).unwrap();
        let real = dir.join("real/config.toml");
        std::fs::write(&real, "[browser]\nexecutable = \"auto\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::os::unix::fs::symlink(&real, dir.join("link/config.toml")).unwrap();
        }
        #[cfg(not(unix))]
        std::fs::copy(&real, dir.join("link/config.toml")).unwrap();
        let edit = ConfigEdit::BrowserBool {
            key: "pin_dashboard",
            value: false,
        };
        update_file_atomic_at(&dir.join("link/config.toml"), edit.description(), |c| {
            edit.apply(c)
        })
        .unwrap();
        let text = std::fs::read_to_string(&real).unwrap();
        assert_eq!(
            text,
            "[browser]\nexecutable = \"auto\"\npin_dashboard = false\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                dir.join("link/config.toml")
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the symlink stays"
            );
            assert_eq!(
                std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(
            std::fs::read_dir(dir.join("real")).unwrap().count() == 1,
            "no temp file left"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
