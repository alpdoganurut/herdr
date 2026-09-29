//! `herdr news run | status | log [n] | open [--edition N] | history [--days N]
//! | enable | disable | times [HH:MM ...|--clear]` (fork): the AI news desk
//! (`news.run`, `news.status`, `news.open`, `news.history`,
//! `news.set_enabled`, `news.set_times`).

use crate::api::schema::{
    EmptyParams, Method, NewsEditionInfo, NewsGetInfo, NewsHistoryParams, NewsOpenParams,
    NewsRunRecord, NewsSetEnabledParams, NewsSetTimesParams, NewsStatusInfo, Request,
};
use crate::persist::news::iso_to_unix;

const USAGE: &str = "usage: herdr news <run|status [--json]|log [N] [--json]|open [--edition N]|history [--days N] [--json]|enable|disable|times [HH:MM ...|--clear]>";
const TIMES_USAGE: &str = "usage: herdr news times [HH:MM ...] [--clear]";
const STATUS_USAGE: &str = "usage: herdr news status [--json]";
const LOG_USAGE: &str = "usage: herdr news log [N] [--json]";
const OPEN_USAGE: &str = "usage: herdr news open [--edition N]";
const HISTORY_USAGE: &str = "usage: herdr news history [--days N] [--json]";
/// How many runs `status` lists and `log` shows by default.
const STATUS_RUNS: usize = 5;
const LOG_RUNS: usize = 10;

pub(super) fn run_news_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("run") => news_run(&args[1..]),
        Some("status") => news_status(&args[1..]),
        Some("log") => news_log(&args[1..]),
        Some("open") => news_open(&args[1..]),
        Some("history") => news_history(&args[1..]),
        Some("enable") => news_set_enabled(&args[1..], true),
        Some("disable") => news_set_enabled(&args[1..], false),
        Some("times") => news_times(&args[1..]),
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
    // The server's history is capped; the log file has everything, but it
    // lives on the server's machine: read it only when that is this one.
    let home = std::path::Path::new(&status.home);
    let records = if super::target::is_remote() || !home.is_dir() {
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

fn fetch_get(
    request_id: &str,
    method: Method,
) -> std::io::Result<Result<NewsGetInfo, serde_json::Value>> {
    let response = super::send_request(&Request {
        id: request_id.into(),
        method,
    })?;
    if response.get("error").is_some() {
        return Ok(Err(response));
    }
    let news = response
        .pointer("/result/news")
        .cloned()
        .ok_or_else(|| std::io::Error::other("the server sent no news record"))?;
    serde_json::from_value(news)
        .map(Ok)
        .map_err(std::io::Error::other)
}

fn news_open(args: &[String]) -> std::io::Result<i32> {
    let mut edition = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--edition" => match args.next().and_then(|n| n.parse::<u32>().ok()) {
                Some(n) if n > 0 => edition = Some(n),
                _ => {
                    eprintln!("{OPEN_USAGE}");
                    return Ok(2);
                }
            },
            "help" | "--help" | "-h" => {
                eprintln!("{OPEN_USAGE}");
                return Ok(0);
            }
            _ => {
                eprintln!("{OPEN_USAGE}");
                return Ok(2);
            }
        }
    }
    let news = match fetch_get(
        "cli:news:open",
        Method::NewsOpen(NewsOpenParams { edition }),
    )? {
        Ok(news) => news,
        Err(response) => return super::print_response(&response),
    };
    match edition {
        Some(edition) => println!(
            "News tab {} focused, showing edition {edition}",
            news.tab_id.as_deref().unwrap_or("?")
        ),
        None => println!("News tab {} focused", news.tab_id.as_deref().unwrap_or("?")),
    }
    Ok(0)
}

fn news_history(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    let mut days = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--days" => match args.next().and_then(|n| n.parse::<u32>().ok()) {
                Some(n) if n > 0 => days = Some(n),
                _ => {
                    eprintln!("{HISTORY_USAGE}");
                    return Ok(2);
                }
            },
            "help" | "--help" | "-h" => {
                eprintln!("{HISTORY_USAGE}");
                return Ok(0);
            }
            _ => {
                eprintln!("{HISTORY_USAGE}");
                return Ok(2);
            }
        }
    }
    let response = super::send_request(&Request {
        id: "cli:news:history".into(),
        method: Method::NewsHistory(NewsHistoryParams { days }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let editions: Vec<NewsEditionInfo> = response
        .pointer("/result/editions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(std::io::Error::other)?
        .unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(&editions)?);
        return Ok(0);
    }
    if editions.is_empty() {
        println!("no editions yet");
        return Ok(0);
    }
    let clock = LocalClock::now();
    for edition in &editions {
        println!("{}", format_edition(edition, &clock));
    }
    Ok(0)
}

fn news_set_enabled(args: &[String], enabled: bool) -> std::io::Result<i32> {
    let usage = if enabled {
        "usage: herdr news enable"
    } else {
        "usage: herdr news disable"
    };
    if let Some(arg) = args.first() {
        eprintln!("{usage}");
        return Ok(if matches!(arg.as_str(), "help" | "--help" | "-h") {
            0
        } else {
            2
        });
    }
    let news = match fetch_get(
        "cli:news:set_enabled",
        Method::NewsSetEnabled(NewsSetEnabledParams { enabled }),
    )? {
        Ok(news) => news,
        Err(response) => return super::print_response(&response),
    };
    let clock = LocalClock::now();
    if news.enabled {
        match news.next_run_at {
            Some(at) => println!(
                "scheduled news runs on; next run {} ({})",
                clock.format(at),
                format_until(clock.now, at)
            ),
            None => println!("scheduled news runs on"),
        }
    } else {
        println!("scheduled news runs off; `herdr news run` still works");
    }
    Ok(0)
}

/// What `herdr news times` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TimesArgs {
    Help,
    /// No arguments: list the times.
    Show,
    /// Times to set (empty for `--clear`), each already checked as `HH:MM`.
    Set(Vec<String>),
}

/// Parse the `times` arguments; the error is the message to print (exit 2).
fn parse_times_args(args: &[String]) -> Result<TimesArgs, String> {
    let mut clear = false;
    let mut times = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--clear" => clear = true,
            "help" | "--help" | "-h" => return Ok(TimesArgs::Help),
            other if other.starts_with('-') => return Err(TIMES_USAGE.into()),
            other => times.push(other.to_string()),
        }
    }
    if clear && !times.is_empty() {
        return Err(TIMES_USAGE.into());
    }
    if !clear && times.is_empty() {
        return Ok(TimesArgs::Show);
    }
    crate::config::normalize_times(times.iter().map(String::as_str))
        .map_err(|err| format!("{err}; expected HH:MM"))?;
    Ok(TimesArgs::Set(times))
}

/// `herdr news times`: list the scheduled times with the next one marked;
/// with times, set them; `--clear` empties the list.
fn news_times(args: &[String]) -> std::io::Result<i32> {
    let (request_id, method) = match parse_times_args(args) {
        Ok(TimesArgs::Help) => {
            eprintln!("{TIMES_USAGE}");
            return Ok(0);
        }
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Ok(TimesArgs::Show) => ("cli:news:times", Method::NewsGet(EmptyParams::default())),
        Ok(TimesArgs::Set(times)) => (
            "cli:news:set_times",
            Method::NewsSetTimes(NewsSetTimesParams { times }),
        ),
    };
    let news = match fetch_get(request_id, method)? {
        Ok(news) => news,
        Err(response) => return super::print_response(&response),
    };
    print!("{}", format_times(&news, &LocalClock::now()));
    Ok(0)
}

/// The times, one per line, the next scheduled one marked with how far off
/// it is; a line first when scheduling is off or the list is empty.
fn format_times(news: &NewsGetInfo, clock: &LocalClock) -> String {
    let mut out = String::new();
    if news.times.is_empty() {
        out.push_str("no scheduled times (no scheduled run starts)\n");
        return out;
    }
    if !news.enabled {
        out.push_str("scheduled runs are off (`herdr news enable` turns them on)\n");
    }
    let next = news.next_run_at.map(|at| (clock.format_hhmm(at), at));
    for time in &news.times {
        out.push_str(time);
        if let Some((hhmm, at)) = next.as_ref() {
            if hhmm == time {
                out.push_str(&format!("  next ({})", format_until(clock.now, *at)));
            }
        }
        out.push('\n');
    }
    out
}

/// One `history` line: `  3  2026-09-29 09:15  manual     30 stories  changed`.
fn format_edition(edition: &NewsEditionInfo, clock: &LocalClock) -> String {
    let mut line = format!(
        "{:>4}  {}  {:<9}  {:>2} stories",
        edition.edition,
        clock.format_iso(&edition.at),
        edition.trigger,
        edition.stories
    );
    if edition.changed {
        line.push_str("  changed");
    }
    line
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
                // Whole minutes: the two clocks are read a fraction apart.
                ((local - utc).whole_seconds() + 30).div_euclid(60) * 60
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

    /// `HH:MM` local.
    fn format_hhmm(&self, unix: u64) -> String {
        let shifted = i64::try_from(unix).unwrap_or(0) + self.offset_seconds;
        let date = time::OffsetDateTime::from_unix_timestamp(shifted)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        format!("{:02}:{:02}", date.hour(), date.minute())
    }

    fn format_iso(&self, iso: &str) -> String {
        iso_to_unix(iso)
            .map(|unix| self.format(unix))
            .unwrap_or_else(|| iso.to_string())
    }
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
        "news: {}, times {}, quiet {}{}\n",
        if status.enabled {
            "enabled"
        } else {
            "disabled (scheduled runs off)"
        },
        if status.times.is_empty() {
            "none".to_string()
        } else {
            status.times.join(" ")
        },
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
    if status.pending_notifications > 0 {
        out.push_str(&format!(
            "pending notifications: {}\n",
            status.pending_notifications
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
    fn local_clock_formats_unix_and_iso_times() {
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
    fn history_lines_are_numbered_and_flag_changes() {
        let edition = NewsEditionInfo {
            edition: 3,
            at: "2026-09-29T06:15:51+00:00".into(),
            trigger: "manual".into(),
            stories: 30,
            changed: true,
            ..NewsEditionInfo::default()
        };
        assert_eq!(
            format_edition(&edition, &utc()),
            "   3  2026-09-29 06:15  manual     30 stories  changed"
        );
        let same = NewsEditionInfo {
            edition: 12,
            trigger: "scheduled".into(),
            stories: 7,
            changed: false,
            ..edition
        };
        assert_eq!(
            format_edition(&same, &utc()),
            "  12  2026-09-29 06:15  scheduled   7 stories"
        );
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
            times: vec!["08:00".into(), "13:00".into(), "19:00".into()],
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
            pending_notifications: 1,
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
            "news: enabled, times 08:00 13:00 19:00, quiet 00:00-08:00, model opus\n\
             home: /tmp/news\n\
             tab: w_1:t_3\n\
             next run: 2027-01-15 10:00 (in 2h 00m)\n\
             current: running since 2027-01-15 07:58 (manual, 2m00s)\n\
             failures in a row: 2\n\
             pending notifications: 1\n\
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
            pending_notifications: 0,
            recent: Vec::new(),
            quiet_hours: String::new(),
            home: String::new(),
            times: Vec::new(),
            ..status
        };
        assert_eq!(
            format_status(&idle, &utc(), STATUS_RUNS),
            "news: disabled (scheduled runs off), times none, quiet off\nnext run: none\ncurrent: none\nrecent runs: none\n"
        );
    }

    fn get_info(times: &[&str], enabled: bool, next_run_at: Option<u64>) -> NewsGetInfo {
        NewsGetInfo {
            enabled,
            times: times.iter().map(|time| (*time).to_string()).collect(),
            quiet_hours: String::new(),
            model: None,
            tab_id: None,
            pane_id: None,
            next_run_at,
            run: None,
            last_run: None,
            unread: false,
            consecutive_failures: 0,
            pending_notifications: 0,
        }
    }

    #[test]
    fn times_arguments_show_set_clear_or_refuse() {
        let args = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };
        assert_eq!(parse_times_args(&args(&[])), Ok(TimesArgs::Show));
        assert_eq!(parse_times_args(&args(&["--help"])), Ok(TimesArgs::Help));
        assert_eq!(
            parse_times_args(&args(&["19:00", "8:00"])),
            Ok(TimesArgs::Set(args(&["19:00", "8:00"]))),
            "sent as given; the server sorts and pads"
        );
        assert_eq!(
            parse_times_args(&args(&["--clear"])),
            Ok(TimesArgs::Set(Vec::new()))
        );
        assert_eq!(
            parse_times_args(&args(&["--clear", "08:00"])),
            Err(TIMES_USAGE.to_string())
        );
        assert_eq!(
            parse_times_args(&args(&["--json"])),
            Err(TIMES_USAGE.to_string())
        );
        let invalid = parse_times_args(&args(&["08:00", "8:0"])).unwrap_err();
        assert!(
            invalid.contains("8:0") && invalid.ends_with("expected HH:MM"),
            "{invalid}"
        );
    }

    #[test]
    fn times_are_listed_with_the_next_one_marked() {
        // 1_800_000_000 is 2027-01-15 08:00:00 UTC.
        let clock = utc();
        let next = clock.now + 5 * 3600;
        assert_eq!(
            format_times(
                &get_info(&["08:00", "13:00", "19:00"], true, Some(next)),
                &clock
            ),
            "08:00\n13:00  next (in 5h 00m)\n19:00\n"
        );
        assert_eq!(
            format_times(&get_info(&["08:00"], true, Some(clock.now)), &clock),
            "08:00  next (due now)\n",
            "a slot owed now is reported at its own time"
        );
        assert_eq!(
            format_times(&get_info(&["08:00", "19:00"], false, None), &clock),
            "scheduled runs are off (`herdr news enable` turns them on)\n08:00\n19:00\n"
        );
        assert_eq!(
            format_times(&get_info(&[], true, None), &clock),
            "no scheduled times (no scheduled run starts)\n"
        );
        assert_eq!(clock.format_hhmm(next), "13:00");
    }
}
