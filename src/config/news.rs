//! `[news]`: the AI news desk the server schedules and runs (fork).
//!
//! Scheduling is off by default, so upstream behaviour is unchanged unless
//! `enabled = true`. A manual `herdr news run` works either way.

use serde::Deserialize;

/// Hours between scheduled runs when `interval_hours` is unset.
pub const DEFAULT_NEWS_INTERVAL_HOURS: u32 = 6;
/// `interval_hours` is clamped to `1..=MAX_NEWS_INTERVAL_HOURS` with a diagnostic.
pub const MAX_NEWS_INTERVAL_HOURS: u32 = 24 * 7;
/// The local window in which no scheduled run starts, when `quiet_hours` is unset.
pub const DEFAULT_NEWS_QUIET_HOURS: &str = "00:00-08:00";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NewsConfig {
    /// Run the news desk on a schedule (the server's headless loop). Default: false.
    pub enabled: bool,
    /// Hours between scheduled runs. Default: 6 (1 through 168).
    pub interval_hours: u32,
    /// Local wall-clock window `HH:MM-HH:MM` during which a due scheduled run
    /// waits (it starts when the window ends). Default: `00:00-08:00`. An
    /// empty string disables quiet hours.
    pub quiet_hours: String,
    /// Model name handed to the runner (`claude -p --model`). Unset: the
    /// runner's default.
    pub model: Option<String>,
}

impl Default for NewsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: DEFAULT_NEWS_INTERVAL_HOURS,
            quiet_hours: DEFAULT_NEWS_QUIET_HOURS.into(),
            model: None,
        }
    }
}

impl NewsConfig {
    /// The interval as configured, clamped to the accepted range.
    pub fn effective_interval_hours(&self) -> u32 {
        self.interval_hours.clamp(1, MAX_NEWS_INTERVAL_HOURS)
    }

    /// The quiet window, or `None` when unset or invalid (an invalid value is
    /// reported by [`Self::diagnostics`] and treated as no quiet hours).
    pub fn quiet_hours(&self) -> Option<QuietHours> {
        parse_quiet_hours(&self.quiet_hours).ok().flatten()
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        if self.interval_hours != self.effective_interval_hours() {
            diagnostics.push(format!(
                "news.interval_hours = {} is out of range; using {} (allowed 1 through {})",
                self.interval_hours,
                self.effective_interval_hours(),
                MAX_NEWS_INTERVAL_HOURS
            ));
        }
        if let Err(err) = parse_quiet_hours(&self.quiet_hours) {
            diagnostics.push(format!(
                "news.quiet_hours = {:?} is invalid ({err}); expected HH:MM-HH:MM; quiet hours are off",
                self.quiet_hours
            ));
        }
        diagnostics
    }
}

/// A daily local window `[start, end)` in minutes since midnight. `end <=
/// start` wraps past midnight (`22:00-06:00`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    pub start: u16,
    pub end: u16,
}

const MINUTES_PER_DAY: u16 = 24 * 60;

impl QuietHours {
    /// Whether `minute_of_day` (local) falls in the window.
    pub fn contains(&self, minute_of_day: u16) -> bool {
        let minute = minute_of_day % MINUTES_PER_DAY;
        if self.start < self.end {
            (self.start..self.end).contains(&minute)
        } else {
            minute >= self.start || minute < self.end
        }
    }

    /// Whole minutes from `minute_of_day` until the window ends; zero when
    /// outside the window.
    pub fn minutes_until_end(&self, minute_of_day: u16) -> u16 {
        if !self.contains(minute_of_day) {
            return 0;
        }
        let minute = minute_of_day % MINUTES_PER_DAY;
        (self.end + MINUTES_PER_DAY - minute) % MINUTES_PER_DAY
    }
}

fn parse_minute_of_day(text: &str) -> Result<u16, String> {
    let (hours, minutes) = text
        .split_once(':')
        .ok_or_else(|| format!("{text:?} is not HH:MM"))?;
    let hours: u16 = hours
        .parse()
        .map_err(|_| format!("{hours:?} is not an hour"))?;
    let minutes: u16 = minutes
        .parse()
        .map_err(|_| format!("{minutes:?} is not a minute"))?;
    if hours > 23 || minutes > 59 {
        return Err(format!("{text} is not a time of day"));
    }
    Ok(hours * 60 + minutes)
}

/// Parse `HH:MM-HH:MM`. Empty (after trimming) is `Ok(None)`: no quiet
/// hours. Equal start and end is a full-day window and is rejected.
pub fn parse_quiet_hours(text: &str) -> Result<Option<QuietHours>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (start, end) = text
        .split_once('-')
        .ok_or_else(|| "missing the '-' between start and end".to_string())?;
    let start = parse_minute_of_day(start.trim())?;
    let end = parse_minute_of_day(end.trim())?;
    if start == end {
        return Err("start and end are the same time".into());
    }
    Ok(Some(QuietHours { start, end }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_six_hours_and_the_night() {
        let config = NewsConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.effective_interval_hours(), 6);
        assert_eq!(config.quiet_hours(), Some(QuietHours { start: 0, end: 480 }));
        assert!(config.model.is_none());
        assert!(config.diagnostics().is_empty());
    }

    #[test]
    fn quiet_hours_parse_and_wrap() {
        let night = parse_quiet_hours("22:00-06:00").unwrap().unwrap();
        assert!(night.contains(22 * 60));
        assert!(night.contains(0));
        assert!(night.contains(5 * 60 + 59));
        assert!(!night.contains(6 * 60));
        assert!(!night.contains(12 * 60));
        assert_eq!(night.minutes_until_end(23 * 60), 7 * 60);
        assert_eq!(night.minutes_until_end(30), 5 * 60 + 30);
        assert_eq!(night.minutes_until_end(12 * 60), 0);

        let day = parse_quiet_hours(" 09:30 - 17:00 ").unwrap().unwrap();
        assert!(day.contains(9 * 60 + 30));
        assert!(!day.contains(17 * 60));
        assert_eq!(day.minutes_until_end(16 * 60), 60);

        assert_eq!(parse_quiet_hours("").unwrap(), None);
        assert_eq!(parse_quiet_hours("   ").unwrap(), None);
        for invalid in ["08:00", "24:00-08:00", "00:00-08:60", "a-b", "08:00-08:00"] {
            assert!(parse_quiet_hours(invalid).is_err(), "{invalid} parses");
        }
    }

    #[test]
    fn out_of_range_values_are_clamped_with_diagnostics() {
        let config = NewsConfig {
            interval_hours: 0,
            quiet_hours: "nope".into(),
            ..NewsConfig::default()
        };
        assert_eq!(config.effective_interval_hours(), 1);
        assert_eq!(config.quiet_hours(), None);
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics[0].contains("news.interval_hours = 0"));
        assert!(diagnostics[1].contains("news.quiet_hours"));

        let config = NewsConfig {
            interval_hours: 1_000,
            ..NewsConfig::default()
        };
        assert_eq!(config.effective_interval_hours(), MAX_NEWS_INTERVAL_HOURS);
    }

    #[test]
    fn section_deserializes_from_toml() {
        let config: NewsConfig =
            toml::from_str("enabled = true\ninterval_hours = 3\nmodel = \"opus\"\n").unwrap();
        assert!(config.enabled);
        assert_eq!(config.interval_hours, 3);
        assert_eq!(config.model.as_deref(), Some("opus"));
        assert_eq!(config.quiet_hours, DEFAULT_NEWS_QUIET_HOURS);
    }
}
