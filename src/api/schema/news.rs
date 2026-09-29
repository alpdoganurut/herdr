//! The AI news desk (fork): what `news.status` reports and `news.run` returns,
//! what `news.get` gives the client shell, `news.history`'s editions and the
//! parameters of `news.open` and `news.set_enabled`.
//!
//! Records mirror the lines the bundled runner appends to
//! `<home>/runs/index.jsonl`; the server reads them back, it never
//! invents cost or turn figures.

use serde::{Deserialize, Serialize};

/// The editor's notification request (`decision.notify`) on a run that
/// changed the page. The server's policy decides whether it is delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewsNotifyInfo {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// `low` or `high` (high may exceed the daily cap once).
    #[serde(default = "default_urgency")]
    pub urgency: String,
}

fn default_urgency() -> String {
    "low".into()
}

/// One news run as recorded in `runs/index.jsonl`, newest first in
/// [`NewsStatusInfo::recent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct NewsRunRecord {
    /// When the run started, ISO 8601 UTC (`YYYY-MM-DDTHH:MM:SS+00:00`).
    pub started: String,
    /// When the run ended, same form; absent for a record written without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// `manual` or `scheduled`.
    pub trigger: String,
    /// The runner's per-run directory name under `runs/` (local stamp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// `ok`, `invalid`, `timeout`, `failed` or `dry-run`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub turns: u32,
    /// The edition the run published, on `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<u32>,
    /// The editor's decision that the page changed (only meaningful on `ok`).
    #[serde(default)]
    pub changed: bool,
    /// The editor's one-line summary of the run, when it gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The editor's notification request (`decision.notify`), when it made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<NewsNotifyInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl NewsRunRecord {
    /// Whether the run counts as a success for the failure counter.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome.as_str(), "ok" | "dry-run")
    }

    /// The last run as `news.get` reports it.
    pub fn last_run(&self) -> NewsLastRun {
        NewsLastRun {
            started_at: crate::persist::news::iso_to_unix(&self.started).unwrap_or(0),
            ended_at: self
                .ended
                .as_deref()
                .and_then(crate::persist::news::iso_to_unix),
            trigger: self.trigger.clone(),
            outcome: self.outcome.clone(),
            edition: self.edition,
            changed: self.changed,
            summary: self.summary.clone(),
            error: self.errors.first().cloned(),
        }
    }
}

/// The last finished run, in `news.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewsLastRun {
    /// Seconds since the Unix epoch.
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    pub trigger: String,
    /// `ok`, `invalid`, `timeout`, `failed` or `dry-run`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<u32>,
    #[serde(default)]
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The first recorded error of a run that did not succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One published edition from `<home>/editions/index.json` (`news.history`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct NewsEditionInfo {
    pub edition: u32,
    /// The edition file, relative to `<home>/editions/`.
    #[serde(default)]
    pub path: String,
    /// When it was published, ISO 8601 UTC.
    #[serde(default)]
    pub at: String,
    /// The local day, `YYYY-MM-DD`.
    #[serde(default)]
    pub day: String,
    /// `manual` or `scheduled`.
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub stories: u32,
    #[serde(default)]
    pub changed: bool,
}

/// `news.get`: what the client shell shows (the pinned row, the settings
/// section) and what `news.open` / `news.set_enabled` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewsGetInfo {
    /// Whether scheduled runs are on (`news.enabled`).
    pub enabled: bool,
    pub interval_hours: u32,
    /// The configured quiet window, `HH:MM-HH:MM`, or empty.
    pub quiet_hours: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The News tab and its pane, while the tab exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The next scheduled run, seconds since the Unix epoch; absent while
    /// scheduling is off or the server has no news home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<u64>,
    /// The run in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<NewsRunInfo>,
    /// The last finished run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<NewsLastRun>,
    /// The News tab is marked important: a run changed the page and the tab
    /// has not been focused since.
    pub unread: bool,
    pub consecutive_failures: u32,
}

/// `news.open`: focus the News tab; with `edition`, show that edition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct NewsOpenParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edition: Option<u32>,
}

/// `news.history`: the editions index, optionally only the last `days` days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct NewsHistoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
}

/// `news.set_enabled`: turn scheduled runs on or off (written to the config).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct NewsSetEnabledParams {
    pub enabled: bool,
}

/// The run in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewsRunInfo {
    /// Seconds since the Unix epoch.
    pub started_at: u64,
    /// `manual` or `scheduled`.
    pub trigger: String,
    /// `starting` (waiting for the pane's shell prompt) or `running`.
    pub phase: String,
}

/// `news.status` (and the reply to `news.run`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewsStatusInfo {
    /// Whether scheduled runs are on (`news.enabled`).
    pub enabled: bool,
    pub interval_hours: u32,
    /// The configured quiet window, `HH:MM-HH:MM`, or empty.
    pub quiet_hours: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The news home directory (`<session data dir>/news`).
    pub home: String,
    /// The next scheduled run, seconds since the Unix epoch; absent while
    /// scheduling is off or the server has no news home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<u64>,
    /// The News tab and its pane, once created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<NewsRunInfo>,
    pub consecutive_failures: u32,
    /// Recent runs, newest first (at most 50).
    #[serde(default)]
    pub recent: Vec<NewsRunRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{EmptyParams, Method, Request, ResponseResult};

    #[test]
    fn news_requests_round_trip() {
        for (method, name) in [
            (Method::NewsRun(EmptyParams::default()), "news.run"),
            (Method::NewsStatus(EmptyParams::default()), "news.status"),
            (Method::NewsGet(EmptyParams::default()), "news.get"),
            (
                Method::NewsHistory(NewsHistoryParams { days: Some(7) }),
                "news.history",
            ),
            (
                Method::NewsOpen(NewsOpenParams { edition: Some(3) }),
                "news.open",
            ),
            (
                Method::NewsSetEnabled(NewsSetEnabledParams { enabled: true }),
                "news.set_enabled",
            ),
        ] {
            let request = Request {
                id: "req".into(),
                method,
            };
            let json = serde_json::to_value(&request).unwrap();
            assert_eq!(json["method"], name);
            assert_eq!(crate::api::api_method_name(&request.method), name);
            assert_eq!(serde_json::from_value::<Request>(json).unwrap(), request);
        }
    }

    #[test]
    fn status_response_round_trips_with_optional_fields_absent() {
        let status = NewsStatusInfo {
            enabled: true,
            interval_hours: 6,
            quiet_hours: "00:00-08:00".into(),
            model: None,
            home: "/tmp/news".into(),
            next_run_at: Some(1_800_000_000),
            tab_id: Some("w_1:t_2".into()),
            pane_id: None,
            run: Some(NewsRunInfo {
                started_at: 1_799_999_000,
                trigger: "manual".into(),
                phase: "running".into(),
            }),
            consecutive_failures: 0,
            recent: vec![NewsRunRecord {
                started: "2026-09-29T11:05:07+00:00".into(),
                trigger: "manual".into(),
                outcome: "ok".into(),
                cost_usd: 0.91,
                turns: 31,
                edition: Some(3),
                changed: true,
                ..NewsRunRecord::default()
            }],
        };
        let result = ResponseResult::NewsStatus {
            status: status.clone(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "news_status");
        assert!(json["status"].get("model").is_none());
        assert!(json["status"].get("pane_id").is_none());
        assert!(json["status"]["recent"][0].get("errors").is_none());
        assert_eq!(
            serde_json::from_value::<ResponseResult>(json).unwrap(),
            result
        );

        let minimal = serde_json::json!({
            "enabled": false, "interval_hours": 6, "quiet_hours": "", "home": "/x",
            "consecutive_failures": 2
        });
        let decoded: NewsStatusInfo = serde_json::from_value(minimal).unwrap();
        assert!(decoded.recent.is_empty());
        assert!(decoded.run.is_none());
    }

    #[test]
    fn get_and_history_responses_round_trip_and_open_params_default() {
        let record = NewsRunRecord {
            started: "2026-09-29T06:09:22+00:00".into(),
            ended: Some("2026-09-29T06:15:51+00:00".into()),
            trigger: "manual".into(),
            outcome: "ok".into(),
            edition: Some(1),
            changed: true,
            notify: Some(NewsNotifyInfo {
                title: "A launch".into(),
                body: None,
                urgency: "high".into(),
            }),
            ..NewsRunRecord::default()
        };
        let last = record.last_run();
        assert_eq!(last.started_at, 1_790_662_162);
        assert_eq!(last.ended_at, Some(1_790_662_551));
        assert_eq!(last.outcome, "ok");
        assert_eq!(last.edition, Some(1));
        assert_eq!(record.notify.as_ref().unwrap().urgency, "high");

        let info = NewsGetInfo {
            enabled: false,
            interval_hours: 6,
            quiet_hours: String::new(),
            model: None,
            tab_id: Some("w_1:t_2".into()),
            pane_id: None,
            next_run_at: None,
            run: None,
            last_run: Some(last),
            unread: true,
            consecutive_failures: 0,
        };
        let result = ResponseResult::NewsGet { news: info.clone() };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "news_get");
        assert_eq!(json["news"]["unread"], true);
        assert!(json["news"].get("run").is_none());
        assert_eq!(
            serde_json::from_value::<ResponseResult>(json).unwrap(),
            result
        );

        let result = ResponseResult::NewsHistory {
            editions: vec![NewsEditionInfo {
                edition: 1,
                path: "2026-09-29/0915-e0001.json".into(),
                at: "2026-09-29T06:15:51+00:00".into(),
                day: "2026-09-29".into(),
                trigger: "manual".into(),
                stories: 30,
                changed: true,
            }],
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "news_history");
        assert_eq!(json["editions"][0]["edition"], 1);
        assert_eq!(
            serde_json::from_value::<ResponseResult>(json).unwrap(),
            result
        );

        let open: NewsOpenParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(open, NewsOpenParams::default());
        let notify: NewsNotifyInfo =
            serde_json::from_value(serde_json::json!({"title": "t"})).unwrap();
        assert_eq!(notify.urgency, "low");
    }
}
