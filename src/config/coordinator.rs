//! `[coordinator]`: the coordinator agent the herdr server runs (fork).
//!
//! Off by default, so upstream behaviour is unchanged unless
//! `enabled = true`. The wake-up timing constants (debounce, gaps, settle,
//! missing) are not config keys; they keep their `HERDR_COORDINATOR_*`
//! environment overrides, which also apply on top of the caps and the
//! periodic check configured here.

use serde::{Deserialize, Deserializer};

use super::news::{parse_quiet_hours, QuietHours};
use crate::coordinator::watch::WakeCfg;

pub const DEFAULT_CAP_HOUR: u32 = 12;
pub const DEFAULT_CAP_DAY: u32 = 80;
pub const DEFAULT_PERIODIC_MINUTES: u64 = 60;
pub const DEFAULT_RELAUNCH_CAP_HOUR: u32 = 3;
pub const DEFAULT_NOTIFY_DAILY_CAP: u32 = 6;
/// The dashboard's port on 127.0.0.1 when `dashboard_port` is unset.
pub const DEFAULT_DASHBOARD_PORT: u16 = crate::coordinator::DEFAULT_PORT;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "RawCoordinatorConfig")]
pub struct CoordinatorConfig {
    /// Run the coordinator agent in a pinned `coordinator` tab. Default: false.
    pub enabled: bool,
    /// Claude model for the coordinator (`--model`); applies at the next
    /// launch. Unset: Claude's default.
    pub model: Option<String>,
    /// Wake-ups per hour. Default: 12.
    pub cap_hour: u32,
    /// Wake-ups per day. Default: 80.
    pub cap_day: u32,
    /// The slow periodic check, in minutes, when changes are pending.
    /// Default: 60.
    pub periodic_minutes: u64,
    /// Relaunches of a coordinator that went missing per hour before it is
    /// reported down. Default: 3.
    pub relaunch_cap_hour: u32,
    /// Coordinator notifications (new suggestions, down, blocked). Default: true.
    pub notify: bool,
    /// Suggestion notifications per day. Default: 6.
    pub notify_daily_cap: u32,
    /// Local `HH:MM-HH:MM` window during which coordinator notifications
    /// wait (a `down` alert does not). Default: empty (off).
    pub quiet_hours: String,
    /// The dashboard's port on 127.0.0.1; `0` disables serving. Default:
    /// 7718. A value that is not a port is reported
    /// ([`Self::diagnostics`]) and the default used.
    pub dashboard_port: u16,
    /// The `dashboard_port` value as written when it was not a port (not
    /// a key of its own: filled from `dashboard_port`).
    #[serde(skip)]
    pub invalid_dashboard_port: Option<String>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: None,
            cap_hour: DEFAULT_CAP_HOUR,
            cap_day: DEFAULT_CAP_DAY,
            periodic_minutes: DEFAULT_PERIODIC_MINUTES,
            relaunch_cap_hour: DEFAULT_RELAUNCH_CAP_HOUR,
            notify: true,
            notify_daily_cap: DEFAULT_NOTIFY_DAILY_CAP,
            quiet_hours: String::new(),
            dashboard_port: DEFAULT_DASHBOARD_PORT,
            invalid_dashboard_port: None,
        }
    }
}

/// `[coordinator]` as written: like [`CoordinatorConfig`], with a lenient
/// `dashboard_port` so one bad value does not discard the whole section.
#[derive(Deserialize)]
#[serde(default)]
struct RawCoordinatorConfig {
    enabled: bool,
    model: Option<String>,
    cap_hour: u32,
    cap_day: u32,
    periodic_minutes: u64,
    relaunch_cap_hour: u32,
    notify: bool,
    notify_daily_cap: u32,
    quiet_hours: String,
    dashboard_port: PortSetting,
}

impl Default for RawCoordinatorConfig {
    fn default() -> Self {
        let config = CoordinatorConfig::default();
        Self {
            enabled: config.enabled,
            model: config.model,
            cap_hour: config.cap_hour,
            cap_day: config.cap_day,
            periodic_minutes: config.periodic_minutes,
            relaunch_cap_hour: config.relaunch_cap_hour,
            notify: config.notify,
            notify_daily_cap: config.notify_daily_cap,
            quiet_hours: config.quiet_hours,
            dashboard_port: PortSetting::Port(config.dashboard_port),
        }
    }
}

impl From<RawCoordinatorConfig> for CoordinatorConfig {
    fn from(raw: RawCoordinatorConfig) -> Self {
        let (dashboard_port, invalid_dashboard_port) = match raw.dashboard_port {
            PortSetting::Port(port) => (port, None),
            PortSetting::Invalid(value) => (DEFAULT_DASHBOARD_PORT, Some(value)),
        };
        Self {
            enabled: raw.enabled,
            model: raw.model,
            cap_hour: raw.cap_hour,
            cap_day: raw.cap_day,
            periodic_minutes: raw.periodic_minutes,
            relaunch_cap_hour: raw.relaunch_cap_hour,
            notify: raw.notify,
            notify_daily_cap: raw.notify_daily_cap,
            quiet_hours: raw.quiet_hours,
            dashboard_port,
            invalid_dashboard_port,
        }
    }
}

/// `dashboard_port` as written: a port, or anything else (kept for the
/// diagnostic).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PortSetting {
    Port(u16),
    Invalid(String),
}

impl<'de> Deserialize<'de> for PortSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = PortSetting;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a port number")
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<PortSetting, E> {
                Ok(u16::try_from(value)
                    .map(PortSetting::Port)
                    .unwrap_or_else(|_| PortSetting::Invalid(value.to_string())))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<PortSetting, E> {
                Ok(u16::try_from(value)
                    .map(PortSetting::Port)
                    .unwrap_or_else(|_| PortSetting::Invalid(value.to_string())))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<PortSetting, E> {
                Ok(PortSetting::Invalid(value.to_string()))
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<PortSetting, E> {
                Ok(PortSetting::Invalid(value.to_string()))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<PortSetting, E> {
                Ok(PortSetting::Invalid(format!("{value:?}")))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Why a pair of wake caps is refused (`coordinator.set_wake_caps`).
pub fn validate_caps(cap_hour: u32, cap_day: u32) -> Result<(), String> {
    if cap_hour == 0 || cap_day == 0 {
        return Err("wake caps must be at least 1".into());
    }
    if cap_hour > cap_day {
        return Err(format!(
            "the hourly cap ({cap_hour}) cannot exceed the daily cap ({cap_day})"
        ));
    }
    Ok(())
}

impl CoordinatorConfig {
    /// The effective dashboard port (`0`: not served).
    pub fn dashboard_port(&self) -> u16 {
        self.dashboard_port
    }

    /// The trimmed model name, `None` when unset or blank.
    pub fn model(&self) -> Option<String> {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_string)
    }

    /// The caps in effect: invalid ones fall back to the defaults.
    pub fn caps(&self) -> (u32, u32) {
        if validate_caps(self.cap_hour, self.cap_day).is_ok() {
            (self.cap_hour, self.cap_day)
        } else {
            (DEFAULT_CAP_HOUR, DEFAULT_CAP_DAY)
        }
    }

    /// The watcher's wake policy: the configured caps and periodic check,
    /// with the `HERDR_COORDINATOR_*` overrides on top.
    pub fn wake_cfg(&self) -> WakeCfg {
        let (cap_hour, cap_day) = self.caps();
        WakeCfg::from_config(
            cap_hour,
            cap_day,
            self.periodic_minutes.max(1).saturating_mul(60),
        )
    }

    /// The quiet window, `None` when unset or invalid.
    pub fn quiet_hours(&self) -> Option<QuietHours> {
        parse_quiet_hours(&self.quiet_hours).ok().flatten()
    }

    pub fn diagnostics(&self) -> Vec<String> {
        let mut diagnostics = Vec::new();
        if let Some(value) = &self.invalid_dashboard_port {
            diagnostics.push(format!(
                "coordinator.dashboard_port = {value} is not a port (0-65535); using {DEFAULT_DASHBOARD_PORT}"
            ));
        }
        if let Err(err) = validate_caps(self.cap_hour, self.cap_day) {
            diagnostics.push(format!(
                "coordinator.cap_hour/cap_day: {err}; using {DEFAULT_CAP_HOUR}/{DEFAULT_CAP_DAY}"
            ));
        }
        if self.periodic_minutes == 0 {
            diagnostics.push("coordinator.periodic_minutes must be at least 1; using 1".into());
        }
        if let Err(err) = parse_quiet_hours(&self.quiet_hours) {
            diagnostics.push(format!(
                "coordinator.quiet_hours = {:?} is invalid ({err}); expected HH:MM-HH:MM; quiet hours are off",
                self.quiet_hours
            ));
        }
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> CoordinatorConfig {
        toml::from_str(text).expect("parses")
    }

    #[test]
    fn defaults_are_off_with_the_documented_values() {
        let config = parse("");
        assert_eq!(config, CoordinatorConfig::default());
        assert!(!config.enabled);
        assert_eq!(config.dashboard_port(), 7718);
        assert_eq!(config.caps(), (12, 80));
        assert!(config.notify);
        assert_eq!(config.notify_daily_cap, 6);
        assert_eq!(config.relaunch_cap_hour, 3);
        assert_eq!(config.quiet_hours(), None);
        assert!(config.diagnostics().is_empty());
        let wake = WakeCfg {
            periodic_s: 3600,
            ..WakeCfg::default()
        };
        assert_eq!(config.wake_cfg().periodic_s, wake.periodic_s);
    }

    #[test]
    fn values_parse_and_a_bad_port_falls_back_with_a_diagnostic() {
        let config = parse(
            "enabled = true\nmodel = ' opus '\ncap_hour = 4\ncap_day = 20\nperiodic_minutes = 10\ndashboard_port = 7728\nquiet_hours = '22:00-07:00'\n",
        );
        assert!(config.enabled);
        assert_eq!(config.model().as_deref(), Some("opus"));
        assert_eq!(config.caps(), (4, 20));
        assert_eq!(config.dashboard_port(), 7728);
        assert!(config.quiet_hours().is_some());
        let zero = parse("dashboard_port = 0\n");
        assert_eq!(zero.dashboard_port(), 0, "0 disables serving");
        for bad in [
            "dashboard_port = 70000\n",
            "dashboard_port = -1\n",
            "dashboard_port = 'x'\n",
        ] {
            let config = parse(bad);
            assert_eq!(config.dashboard_port(), 7718, "{bad}");
            assert_eq!(config.diagnostics().len(), 1, "{bad}");
        }
        assert_eq!(parse("model = '  '\n").model(), None);
    }

    #[test]
    fn invalid_caps_fall_back_together() {
        assert!(validate_caps(4, 20).is_ok());
        assert!(validate_caps(0, 20).is_err());
        assert!(validate_caps(30, 20).is_err());
        let config = parse("cap_hour = 30\ncap_day = 20\n");
        assert_eq!(config.caps(), (12, 80));
        assert_eq!(config.diagnostics().len(), 1);
        let config = parse("quiet_hours = 'soon'\n");
        assert_eq!(config.quiet_hours(), None);
        assert_eq!(config.diagnostics().len(), 1);
    }
}
