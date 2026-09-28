//! `herdr news run | status | log [n]` (fork): the AI news desk
//! (`news.run`, `news.status`).

use crate::api::schema::{EmptyParams, Method, NewsRunRecord, NewsStatusInfo, Request};

const USAGE: &str = "usage: herdr news <run|status [--json]|log [N] [--json]>";
const STATUS_USAGE: &str = "usage: herdr news status [--json]";
const LOG_USAGE: &str = "usage: herdr news log [N] [--json]";
/// How many runs `status` lists and `log` shows by default.
const STATUS_RUNS: usize = 5;
const LOG_RUNS: usize = 10;

pub(super) fn run_news_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("run") => news_run(&args[1..]),
        Some("status") => news_status(&args[1..]),
        Some("log") => news_log(&args[1..]),
        Some("help" | "--help" | "-h") => {
            eprintln!("{USAGE}");
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

fn fetch_status(
    request_id: &str,
    method: Method,
) -> std::io::Result<Result<NewsStatusInfo, serde_json::Value>> {
    let response = super::send_request(&Request {
        id: request_id.into(),
        method,
    })?;
    if response.get("error").is_some() {
        return Ok(Err(response));
    }
    let status = response
        .pointer("/result/status")
        .cloned()
        .ok_or_else(|| std::io::Error::other("the server sent no news status"))?;
    serde_json::from_value(status)
        .map(Ok)
        .map_err(std::io::Error::other)
}

fn news_run(args: &[String]) -> std::io::Result<i32> {
    if let Some(arg) = args.first() {
        eprintln!("usage: herdr news run");
        return Ok(if matches!(arg.as_str(), "help" | "--help" | "-h") {
            0
        } else {
            2
        });
    }
    let status = match fetch_status("cli:news:run", Method::NewsRun(EmptyParams::default()))? {
        Ok(status) => status,
        Err(response) => return super::print_response(&response),
    };
    let clock = LocalClock::now();
    match &status.run {
        Some(run) => println!(
            "news run started ({}, {}) in tab {}",
            run.trigger,
            run.phase,
            status.tab_id.as_deref().unwrap_or("?")
        ),
        None => println!("news run did not start; see `herdr news log`"),
    }
    print!("{}", format_status(&status, &clock, STATUS_RUNS));
    Ok(0)
}

fn news_status(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "help" | "--help" | "-h" => {
                eprintln!("{STATUS_USAGE}");
                return Ok(0);
            }
            _ => {
                eprintln!("{STATUS_USAGE}");
                return Ok(2);
            }
        }
    }
    let status = match fetch_status(
        "cli:news:status",
        Method::NewsStatus(EmptyParams::default()),
    )? {
        Ok(status) => status,
        Err(response) => return super::print_response(&response),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(0);
    }
    print!(
        "{}",
        format_status(&status, &LocalClock::now(), STATUS_RUNS)
    );
    Ok(0)
}

fn news_log(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    let mut count = LOG_RUNS;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "help" | "--help" | "-h" => {
                eprintln!("{LOG_USAGE}");
                return Ok(0);
            }
            other => match other.parse::<usize>() {
                Ok(n) if n > 0 => count = n,
                _ => {
                    eprintln!("{LOG_USAGE}");
                    return Ok(2);
                }
            },
        }
    }
    let status = match fetch_status("cli:news:log", Method::NewsStatus(EmptyParams::default()))? {
        Ok(status) => status,
        Err(response) => return super::print_response(&response),
    };
    // The server's history is capped; the log file has everything.
    let home = std::path::Path::new(&status.home);
    let records = if status.home.is_empty() {
        status.recent.clone()
    } else {
        crate::persist::news::read_history(home, count)
    };
    let records: Vec<&NewsRunRecord> = records.iter().take(count).collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(0);
    }
    if records.is_empty() {
        println!("no news runs yet");
        return Ok(0);
    }
    let clock = LocalClock::now();
    for record in records {
        println!("{}", format_record(record, &clock, true));
    }
    Ok(0)
}

/// The local clock: the UTC offset in effect now (applied to every
/// timestamp; a DST change between then and now shifts old times by an
/// hour, which the listing tolerates).
struct LocalClock {
    now: u64,
    offset_seconds: i64,
}

impl LocalClock {
    fn now() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_secs())
            .unwrap_or(0);
        let offset_seconds = crate::platform::local_datetime()
            .map(|local| {
                let utc = time::OffsetDateTime::now_utc();
                let utc = time::PrimitiveDateTime::new(utc.date(), utc.time());
                (local - utc).whole_seconds()
            })
            .unwrap_or(0);
        Self {
            now,
            offset_seconds,
        }
    }

    /// `YYYY-MM-DD HH:MM` local.
    fn format(&self, unix: u64) -> String {
        let shifted = i64::try_from(unix).unwrap_or(0) + self.offset_seconds;
        let date = time::OffsetDateTime::from_unix_timestamp(shifted)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            date.year(),
            u8::from(date.month()),
            date.day(),
            date.hour(),
            date.minute()
        )
    }

    fn format_iso(&self, iso: &str) -> String {
        iso_to_unix(iso)
            .map(|unix| self.format(unix))
            .unwrap_or_else(|| iso.to_string())
    }
}

/// `YYYY-MM-DDTHH:MM:SS` (any suffix; assumed UTC, which is what the
/// runner writes) to seconds since the epoch.
fn iso_to_unix(iso: &str) -> Option<u64> {
    let bytes = iso.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3_600 + minute * 60 + second).ok()
}

fn format_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

fn format_until(now: u64, at: u64) -> String {
    if at <= now {
        return "due now".into();
    }
    let seconds = at - now;
    if seconds < 60 {
        return "in under a minute".into();
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("in {minutes} min")
    } else {
        format!("in {}h {:02}m", minutes / 60, minutes % 60)
    }
}

fn format_record(record: &NewsRunRecord, clock: &LocalClock, with_details: bool) -> String {
    let mut line = format!(
        "{}  {:<9}  {:<7}  {:>7}  ${:>5.2}  {:>3} turns",
        clock.format_iso(&record.started),
        record.trigger,
        record.outcome,
        record.seconds.map(format_duration).unwrap_or_default(),
        record.cost_usd,
        record.turns
    );
    if let Some(edition) = record.edition {
        line.push_str(&format!("  edition {edition}"));
    }
    if record.changed {
        line.push_str("  changed");
    }
    if with_details {
        if let Some(summary) = &record.summary {
            line.push_str(&format!("\n    {summary}"));
        }
        for error in &record.errors {
            line.push_str(&format!("\n    - {error}"));
        }
    }
    line
}

fn format_status(status: &NewsStatusInfo, clock: &LocalClock, runs: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "news: {}, every {} h, quiet {}{}\n",
        if status.enabled {
            "enabled"
        } else {
            "disabled (scheduled runs off)"
        },
        status.interval_hours,
        if status.quiet_hours.is_empty() {
            "off"
        } else {
            status.quiet_hours.as_str()
        },
        status
            .model
            .as_deref()
            .map(|model| format!(", model {model}"))
            .unwrap_or_default()
    ));
    if !status.home.is_empty() {
        out.push_str(&format!("home: {}\n", status.home));
    }
    if let Some(tab_id) = &status.tab_id {
        out.push_str(&format!("tab: {tab_id}\n"));
    }
    match status.next_run_at {
        Some(at) => out.push_str(&format!(
            "next run: {} ({})\n",
            clock.format(at),
            format_until(clock.now, at)
        )),
        None => out.push_str("next run: none\n"),
    }
    match &status.run {
        Some(run) => out.push_str(&format!(
            "current: {} since {} ({}, {})\n",
            run.phase,
            clock.format(run.started_at),
            run.trigger,
            format_duration(clock.now.saturating_sub(run.started_at))
        )),
        None => out.push_str("current: none\n"),
    }
    if status.consecutive_failures > 0 {
        out.push_str(&format!(
            "failures in a row: {}\n",
            status.consecutive_failures
        ));
    }
    if status.recent.is_empty() {
        out.push_str("recent runs: none\n");
    } else {
        out.push_str("recent runs:\n");
        for record in status.recent.iter().take(runs) {
            out.push_str(&format!("  {}\n", format_record(record, clock, false)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::NewsRunInfo;

    fn utc() -> LocalClock {
        LocalClock {
            now: 1_800_000_000,
            offset_seconds: 0,
        }
    }

    #[test]
    fn iso_timestamps_convert_to_unix_seconds() {
        assert_eq!(iso_to_unix("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(
            iso_to_unix("2026-09-21T14:13:20+00:00"),
            Some(1_790_000_000)
        );
        assert_eq!(iso_to_unix("2027-01-15T08:00:00Z"), Some(1_800_000_000));
        assert_eq!(iso_to_unix("2024-02-29T23:59:59"), Some(1_709_251_199));
        assert_eq!(iso_to_unix("soon"), None);
        assert_eq!(iso_to_unix("2026-13-01T00:00:00"), None);
        assert_eq!(utc().format(1_790_000_000), "2026-09-21 14:13");
        let plus_three = LocalClock {
            now: 0,
            offset_seconds: 3 * 3600,
        };
        assert_eq!(plus_three.format(1_790_000_000), "2026-09-21 17:13");
        assert_eq!(
            plus_three.format_iso("2026-09-21T14:13:20+00:00"),
            "2026-09-21 17:13"
        );
        assert_eq!(plus_three.format_iso("garbage"), "garbage");
    }

    #[test]
    fn durations_and_countdowns_are_compact() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(295), "4m55s");
        assert_eq!(format_duration(3_900), "1h05m");
        assert_eq!(format_until(100, 50), "due now");
        assert_eq!(format_until(100, 130), "in under a minute");
        assert_eq!(format_until(100, 100 + 25 * 60), "in 25 min");
        assert_eq!(format_until(100, 100 + 5 * 3600 + 12 * 60), "in 5h 12m");
    }

    #[test]
    fn status_lists_the_schedule_the_run_and_the_last_runs() {
        let status = NewsStatusInfo {
            enabled: true,
            interval_hours: 6,
            quiet_hours: "00:00-08:00".into(),
            model: Some("opus".into()),
            home: "/tmp/news".into(),
            next_run_at: Some(1_800_000_000 + 2 * 3600),
            tab_id: Some("w_1:t_3".into()),
            pane_id: None,
            run: Some(NewsRunInfo {
                started_at: 1_800_000_000 - 120,
                trigger: "manual".into(),
                phase: "running".into(),
            }),
            consecutive_failures: 2,
            recent: (0..7)
                .map(|n| NewsRunRecord {
                    started: format!("2027-01-1{n}T08:00:00+00:00"),
                    trigger: "scheduled".into(),
                    outcome: if n == 0 {
                        "ok".into()
                    } else {
                        "invalid".into()
                    },
                    seconds: Some(295),
                    cost_usd: 0.9,
                    turns: 31,
                    edition: (n == 0).then_some(4),
                    changed: n == 0,
                    summary: Some("Two launches.".into()),
                    errors: if n == 0 {
                        Vec::new()
                    } else {
                        vec!["bad lead".into()]
                    },
                    ..NewsRunRecord::default()
                })
                .collect(),
        };
        let text = format_status(&status, &utc(), STATUS_RUNS);
        assert_eq!(
            text,
            "news: enabled, every 6 h, quiet 00:00-08:00, model opus\n\
             home: /tmp/news\n\
             tab: w_1:t_3\n\
             next run: 2027-01-15 10:00 (in 2h 00m)\n\
             current: running since 2027-01-15 07:58 (manual, 2m00s)\n\
             failures in a row: 2\n\
             recent runs:\n\
             \x20 2027-01-10 08:00  scheduled  ok         4m55s  $ 0.90   31 turns  edition 4  changed\n\
             \x20 2027-01-11 08:00  scheduled  invalid    4m55s  $ 0.90   31 turns\n\
             \x20 2027-01-12 08:00  scheduled  invalid    4m55s  $ 0.90   31 turns\n\
             \x20 2027-01-13 08:00  scheduled  invalid    4m55s  $ 0.90   31 turns\n\
             \x20 2027-01-14 08:00  scheduled  invalid    4m55s  $ 0.90   31 turns\n"
        );
        let detailed = format_record(&status.recent[1], &utc(), true);
        assert!(
            detailed.ends_with("\n    Two launches.\n    - bad lead"),
            "{detailed}"
        );

        let idle = NewsStatusInfo {
            enabled: false,
            model: None,
            next_run_at: None,
            tab_id: None,
            run: None,
            consecutive_failures: 0,
            recent: Vec::new(),
            quiet_hours: String::new(),
            home: String::new(),
            ..status
        };
        assert_eq!(
            format_status(&idle, &utc(), STATUS_RUNS),
            "news: disabled (scheduled runs off), every 6 h, quiet off\nnext run: none\ncurrent: none\nrecent runs: none\n"
        );
    }
}
