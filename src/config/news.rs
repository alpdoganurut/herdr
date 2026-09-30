//! `[news]`: the AI news desk the server schedules and runs (fork).
//!
//! Scheduling is off by default, so upstream behaviour is unchanged unless
//! `enabled = true`. A manual `herdr news run` works either way. A desk that
//! has never run starts its first run as soon as it is enabled (server start
//! or a reload turning it on), then follows `times`.

use serde::Deserialize;

/// The local times of the scheduled runs when `times` is unset.
pub const DEFAULT_NEWS_TIMES: &[&str] = &["08:00", "13:00", "19:00"];
/// The local window in which a news notification waits, when `quiet_hours`
/// is unset.
pub const DEFAULT_NEWS_QUIET_HOURS: &str = "00:00-08:00";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NewsConfig {
    /// Run the news desk on a schedule (the server's headless loop); a desk
    /// that never ran runs once right away. Default: false.
    pub enabled: bool,
    /// Local times of day (`HH:MM`, 24-hour) at which a scheduled run
    /// starts. Default: `08:00`, `13:00`, `19:00`. An invalid entry is
    /// dropped with a diagnostic, duplicates collapse and the list is kept
    /// sorted ([`Self::times`]). Empty: no scheduled runs.
    pub times: Vec<String>,
    /// Local wall-clock window `HH:MM-HH:MM` during which a news
    /// notification waits (it goes out when the window ends). Scheduling
    /// ignores it. Default: `00:00-08:00`. An empty string disables it.
    pub quiet_hours: String,
    /// Model name handed to the runner (`claude -p --model`). Unset: the
    /// runner's default.
    pub model: Option<String>,
    /// Retired: `interval_hours` was replaced by `times`. Read only to
    /// report it ([`Self::diagnostics`]); its value is ignored.
    #[serde(rename = "interval_hours")]
    pub(crate) retired_interval_hours: Option<Retired>,
}

/// A retired key: only its presence is kept, whatever its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Retired;

impl<'de> Deserialize<'de> for Retired {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Retired)
    }
}

impl Default for NewsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            times: DEFAULT_NEWS_TIMES
                .iter()
                .map(|time| (*time).to_owned())
                .collect(),
            quiet_hours: DEFAULT_NEWS_QUIET_HOURS.into(),
            model: None,
            retired_interval_hours: None,
        }
    }
}

impl NewsConfig {
    /// The scheduled times in minutes since local midnight: the valid
    /// entries, sorted, without duplicates.
    pub fn times(&self) -> Vec<u16> {
        let mut times: Vec<u16> = self
            .times
            .iter()
            .filter_map(|time| parse_hhmm(time).ok())
            .collect();
        times.sort_unstable();
        times.dedup();
        times
    }

    /// The quiet window, or `None` when unset or invalid (an invalid value is
    /// reported by [`Self::diagnostics`] and treated as no quiet hours).
    pub fn quiet_hours(&self) -> Option<QuietHours> {
        parse_quiet_hours(&self.quiet_hours).ok().flatten()
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        if self.retired_interval_hours.is_some() {
            diagnostics.push(
                "news.interval_hours is no longer used (replaced by news.times); remove it"
                    .to_string(),
            );
        }
        for time in &self.times {
            if let Err(err) = parse_hhmm(time) {
                diagnostics.push(format!(
                    "news.times entry {time:?} is invalid ({err}); expected HH:MM; dropped"
                ));
            }
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

/// Parse a 24-hour `HH:MM` (one or two hour digits, two minute digits,
/// surrounding whitespace ignored) to minutes since midnight.
pub fn parse_hhmm(text: &str) -> Result<u16, String> {
    let text = text.trim();
    let (hours, minutes) = text
        .split_once(':')
        .ok_or_else(|| format!("{text:?} is not HH:MM"))?;
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if hours.len() > 2 || minutes.len() != 2 || !digits(hours) || !digits(minutes) {
        return Err(format!("{text:?} is not HH:MM"));
    }
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

/// Minutes since midnight as `HH:MM`.
pub fn format_hhmm(minute_of_day: u16) -> String {
    let minute = minute_of_day % MINUTES_PER_DAY;
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The canonical form of a list of times: every entry parsed (the first
/// invalid one is the error), sorted, without duplicates.
pub fn normalize_times<'a>(texts: impl IntoIterator<Item = &'a str>) -> Result<Vec<u16>, String> {
    let mut times = texts
        .into_iter()
        .map(parse_hhmm)
        .collect::<Result<Vec<u16>, String>>()?;
    times.sort_unstable();
    times.dedup();
    Ok(times)
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
    let start = parse_hhmm(start)?;
    let end = parse_hhmm(end)?;
    if start == end {
        return Err("start and end are the same time".into());
    }
    Ok(Some(QuietHours { start, end }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_three_times_a_day_and_the_night() {
        let config = NewsConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.times, ["08:00", "13:00", "19:00"]);
        assert_eq!(config.times(), [8 * 60, 13 * 60, 19 * 60]);
        assert_eq!(
            config.quiet_hours(),
            Some(QuietHours { start: 0, end: 480 })
        );
        assert!(config.model.is_none());
        assert!(config.diagnostics().is_empty());
    }

    #[test]
    fn times_parse_format_and_normalize() {
        assert_eq!(parse_hhmm("08:00"), Ok(480));
        assert_eq!(parse_hhmm(" 8:05 "), Ok(485));
        assert_eq!(parse_hhmm("23:59"), Ok(23 * 60 + 59));
        for invalid in [
            "", "8", "08:0", "24:00", "12:60", "+8:00", "a:b", "08:00:00", "08-00",
        ] {
            assert!(parse_hhmm(invalid).is_err(), "{invalid:?} parses");
        }
        assert_eq!(format_hhmm(0), "00:00");
        assert_eq!(format_hhmm(485), "08:05");
        assert_eq!(format_hhmm(23 * 60 + 59), "23:59");
        assert_eq!(
            normalize_times(["19:00", "8:00", "13:00", "08:00"]),
            Ok(vec![480, 780, 1140])
        );
        assert_eq!(normalize_times([]), Ok(Vec::new()));
        assert!(normalize_times(["08:00", "nope"]).is_err());
    }

    #[test]
    fn invalid_times_are_dropped_duplicates_collapse_and_the_list_is_sorted() {
        let config = NewsConfig {
            times: vec![
                "19:00".into(),
                "25:00".into(),
                "08:00".into(),
                "8:00".into(),
                "".into(),
                "13:00".into(),
            ],
            ..NewsConfig::default()
        };
        assert_eq!(config.times(), [480, 780, 1140]);
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(diagnostics[0].contains("news.times entry \"25:00\""));
        assert!(diagnostics[1].contains("news.times entry \"\""));

        let empty = NewsConfig {
            times: Vec::new(),
            ..NewsConfig::default()
        };
        assert!(empty.times().is_empty());
        assert!(empty.diagnostics().is_empty());
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
    fn invalid_quiet_hours_are_off_with_a_diagnostic() {
        let config = NewsConfig {
            quiet_hours: "nope".into(),
            ..NewsConfig::default()
        };
        assert_eq!(config.quiet_hours(), None);
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("news.quiet_hours"));
    }

    #[test]
    fn section_deserializes_from_toml() {
        let config: NewsConfig =
            toml::from_str("enabled = true\ntimes = [\"9:00\", \"21:30\"]\nmodel = \"opus\"\n")
                .unwrap();
        assert!(config.enabled);
        assert_eq!(config.times, ["9:00", "21:30"]);
        assert_eq!(config.times(), [9 * 60, 21 * 60 + 30]);
        assert_eq!(config.model.as_deref(), Some("opus"));
        assert_eq!(config.quiet_hours, DEFAULT_NEWS_QUIET_HOURS);
        assert!(config.diagnostics().is_empty());

        let unsorted: NewsConfig =
            toml::from_str("times = [\"19:00\", \"08:00\", \"19:00\", \"bad\"]\n").unwrap();
        assert_eq!(unsorted.times(), [480, 1140]);
        assert_eq!(unsorted.diagnostics().len(), 1);

        let empty: NewsConfig = toml::from_str("times = []\n").unwrap();
        assert!(empty.times().is_empty());
    }

    #[test]
    fn the_retired_interval_key_is_a_diagnostic_not_an_error() {
        let config: NewsConfig = toml::from_str("enabled = true\ninterval_hours = 3\n").unwrap();
        assert!(config.enabled);
        assert_eq!(config.times(), [480, 780, 1140], "the default times apply");
        let diagnostics = config.diagnostics();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("news.interval_hours"));
        assert!(diagnostics[0].contains("replaced by news.times"));

        let odd: NewsConfig = toml::from_str("interval_hours = \"six\"\n").unwrap();
        assert_eq!(odd.diagnostics().len(), 1, "any value is only reported");
    }
}
