//! The AI news desk (fork): what `news.status` reports and `news.run` returns.
//!
//! Records mirror the lines the bundled runner appends to
//! `<home>/runs/index.jsonl`; the server reads them back, it never
//! invents cost or turn figures.

use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

impl NewsRunRecord {
    /// Whether the run counts as a success for the failure counter.
    pub fn succeeded(&self) -> bool {
        matches!(self.outcome.as_str(), "ok" | "dry-run")
    }
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
}
