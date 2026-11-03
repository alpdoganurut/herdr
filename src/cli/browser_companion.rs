//! `herdr browser companion …` (fork, companion launcher): the companion
//! browser app herdr runs, over `browser.companion`. From a herdr pane (an
//! agent, or you typing there) a start runs in the background and a focus
//! never takes the keyboard; from a plain terminal they are yours.

use crate::api::schema::{
    BrowserCompanionAction, BrowserCompanionAppInfo, BrowserCompanionParams, Method, Request,
};
use crate::browser::companion_app::duration_text;

const USAGE: &str = "usage: herdr browser companion status [--json] | start | stop | restart | focus | log [-n 50]
The companion browser herdr installs (`herdr browser setup --companion`) and runs: it starts on the
first page an agent opens (or with the server: [browser] companion_autostart = true), restarts after a
crash (1, 2, 4, 8 s; it gives up after 5 crashes in 5 minutes) and quits with the server.
  status    state, pid, uptime, restarts, version, last error (--json: the raw record)
  start     start it (from a pane: in the background)     stop     quit it
  restart   stop, then start (applies an installed update; clears a give-up)
  focus     bring its window up (from a plain terminal: with the keyboard)
  log       its log (-n lines, default 50)";

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let verb = args.first().map(String::as_str).unwrap_or("status");
    let rest = args.get(1..).unwrap_or_default();
    let action = match verb {
        "status" => BrowserCompanionAction::Status,
        "start" => BrowserCompanionAction::Start,
        "stop" => BrowserCompanionAction::Stop,
        "restart" => BrowserCompanionAction::Restart,
        "focus" => BrowserCompanionAction::Focus,
        "log" => return log(rest),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return Ok(0);
        }
        other => {
            eprintln!("unknown companion command {other:?}\n{USAGE}");
            return Ok(2);
        }
    };
    let json = rest.iter().any(|a| a == "--json");
    if let Some(bad) = rest.iter().find(|a| a.as_str() != "--json") {
        eprintln!("unknown flag {bad}\n{USAGE}");
        return Ok(2);
    }
    act(action, verb, None, json)
}

/// Whether `herdr browser start [--profile P]` starts the companion: it is
/// named, or no profile is while the herdr+ window is off (`window_off`
/// asks the server).
pub(super) fn starts_companion(
    profile: Option<&str>,
    window_off: impl FnOnce() -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    match profile.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => Ok(crate::browser::hub::is_companion(name)),
        None => window_off(),
    }
}

/// `herdr browser start` for the companion: `browser.companion start` with
/// the caller (`browser.start`'s params carry none), so a start from a
/// pane — an agent — runs in the background like `companion start`.
pub(super) fn start_for_browser_start(pane: Option<&str>, json: bool) -> std::io::Result<i32> {
    act(BrowserCompanionAction::Start, "start", pane, json)
}

/// The request for `action` (the caller: `pane`, else this pane's), then
/// its answer, raw (`--json`, an error) or in words.
fn act(
    action: BrowserCompanionAction,
    verb: &str,
    pane: Option<&str>,
    json: bool,
) -> std::io::Result<i32> {
    let response = super::super::send_request(&Request {
        id: format!("cli:browser:companion:{verb}"),
        method: Method::BrowserCompanion(BrowserCompanionParams {
            action,
            caller: super::caller(pane),
        }),
    })?;
    if response.get("error").is_some() || json {
        return super::super::print_response(&response);
    }
    let info: BrowserCompanionAppInfo =
        serde_json::from_value(response["result"]["companion"].clone()).unwrap_or_default();
    let note = response["result"]["note"].as_str();
    let now = crate::browser::unix_now();
    match (action, note) {
        (_, Some(note)) => println!("{note}"),
        (BrowserCompanionAction::Status, None) => {
            println!("{}", status_line(&info, now));
            for line in detail_lines(&info) {
                println!("  {line}");
            }
        }
        (BrowserCompanionAction::Start, None) => println!(
            "companion {}; `herdr browser companion status` follows it",
            if info.state == "running" || info.state == "external" {
                "already running"
            } else {
                "starting"
            }
        ),
        (BrowserCompanionAction::Stop, None) => println!("companion stopping"),
        (BrowserCompanionAction::Restart, None) => println!("companion restarting"),
        (BrowserCompanionAction::Focus, None) => println!(
            "{}",
            if info.state == "running" || info.state == "external" {
                "companion window raised"
            } else {
                "companion starting; its window opens"
            }
        ),
        (BrowserCompanionAction::Unknown, None) => {}
    }
    Ok(0)
}

/// `companion running · pid 51234 · up 2h 3m · 0 restarts · v0.2.0 (update ready: 0.2.1)`.
pub(crate) fn status_line(info: &BrowserCompanionAppInfo, now: u64) -> String {
    let state = match info.state.as_str() {
        "external" => "running (started by hand)",
        "not_installed" => "not installed — herdr browser setup --companion",
        "crashed" => "restarting",
        other => other,
    };
    let mut parts = vec![format!("companion {state}")];
    if let Some(pid) = info.pid {
        parts.push(format!("pid {pid}"));
    }
    if let Some(started) = info.started_unix {
        parts.push(format!("up {}", duration_text(now.saturating_sub(started))));
    }
    if let Some(at) = info.next_retry_unix {
        parts.push(format!(
            "next try in {}",
            duration_text(at.saturating_sub(now))
        ));
    }
    if info.managed || info.state != "external" {
        parts.push(format!(
            "{} restart{}",
            info.restarts,
            if info.restarts == 1 { "" } else { "s" }
        ));
    }
    let version = info.version.as_deref().or(info.installed.as_deref());
    let mut line = parts.join(" · ");
    if let Some(version) = version {
        line.push_str(&format!(" · v{version}"));
    }
    if info.update_ready {
        line.push_str(&format!(
            " (update ready: {})",
            info.installed.as_deref().unwrap_or("?")
        ));
    }
    line
}

/// The lines under the status: the last error, the log, autostart.
pub(crate) fn detail_lines(info: &BrowserCompanionAppInfo) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(error) = info.last_error.as_deref() {
        out.push(format!("last error: {error}"));
    }
    if info.state == "failed" {
        out.push(
            "herdr stopped restarting it; `herdr browser companion restart` tries again".into(),
        );
    }
    if info.update_ready {
        out.push("an update is installed; `herdr browser companion restart` applies it".into());
    }
    if let Some(log) = info.log.as_deref() {
        out.push(format!("log: {log}"));
    }
    if info.autostart {
        out.push("starts with the server ([browser] companion_autostart)".into());
    }
    out
}

fn log(args: &[String]) -> std::io::Result<i32> {
    let mut lines = 50usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--lines" => {
                i += 1;
                match args.get(i).and_then(|n| n.parse().ok()) {
                    Some(n) => lines = n,
                    None => {
                        eprintln!("-n needs a number\n{USAGE}");
                        return Ok(2);
                    }
                }
            }
            n if n.starts_with('-') && n[1..].parse::<usize>().is_ok() => {
                lines = n[1..].parse().unwrap_or(50);
            }
            other => {
                eprintln!("unknown flag {other}\n{USAGE}");
                return Ok(2);
            }
        }
        i += 1;
    }
    // The server says where its log is; without a server, this machine's.
    let path = super::super::send_request_unchecked(&Request {
        id: "cli:browser:companion:log".into(),
        method: Method::BrowserCompanion(BrowserCompanionParams {
            action: BrowserCompanionAction::Status,
            caller: None,
        }),
    })
    .ok()
    .and_then(|response| {
        response["result"]["companion"]["log"]
            .as_str()
            .map(str::to_string)
    })
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| {
        crate::browser::companion_install::log_path(
            &crate::browser::companion_install::companion_home(&crate::browser::browser_home()),
        )
    });
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let all: Vec<&str> = text.lines().collect();
            for line in &all[all.len().saturating_sub(lines)..] {
                println!("{line}");
            }
            Ok(0)
        }
        Err(err) => {
            eprintln!("no companion log at {} ({err})", path.display());
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_start_goes_to_the_companion_when_named_or_the_window_is_off() {
        let asked = std::cell::Cell::new(false);
        let window_off = |off: bool| {
            let asked = &asked;
            move || {
                asked.set(true);
                Ok(off)
            }
        };
        assert!(starts_companion(Some("companion"), window_off(false)).unwrap());
        assert!(starts_companion(Some(" companion "), window_off(false)).unwrap());
        assert!(!asked.get(), "a named profile needs no question");
        assert!(!starts_companion(Some("main"), window_off(true)).unwrap());
        assert!(!asked.get());
        // no profile: the companion while the herdr+ window is off
        assert!(starts_companion(None, window_off(true)).unwrap());
        assert!(asked.get());
        assert!(!starts_companion(None, window_off(false)).unwrap());
        assert!(!starts_companion(Some("  "), window_off(false)).unwrap());
        assert!(starts_companion(None, || Err(std::io::Error::other("no server"))).is_err());
    }

    #[test]
    fn the_status_line_names_pid_uptime_restarts_and_an_update() {
        let info = BrowserCompanionAppInfo {
            state: "running".into(),
            managed: true,
            pid: Some(51234),
            version: Some("0.2.0".into()),
            installed: Some("0.2.1".into()),
            update_ready: true,
            started_unix: Some(1000),
            ..Default::default()
        };
        assert_eq!(
            status_line(&info, 1000 + 2 * 3600 + 3 * 60),
            "companion running · pid 51234 · up 2h 3m · 0 restarts · v0.2.0 (update ready: 0.2.1)"
        );
        let crashed = BrowserCompanionAppInfo {
            state: "crashed".into(),
            restarts: 2,
            next_retry_unix: Some(1004),
            last_error: Some("exited with status 1".into()),
            log: Some("/tmp/companion.log".into()),
            installed: Some("0.2.0".into()),
            ..Default::default()
        };
        assert_eq!(
            status_line(&crashed, 1000),
            "companion restarting · next try in 4s · 2 restarts · v0.2.0"
        );
        assert_eq!(
            detail_lines(&crashed),
            vec![
                "last error: exited with status 1".to_string(),
                "log: /tmp/companion.log".to_string()
            ]
        );
        let external = BrowserCompanionAppInfo {
            state: "external".into(),
            pid: Some(7),
            version: Some("0.2.0".into()),
            ..Default::default()
        };
        assert_eq!(
            status_line(&external, 0),
            "companion running (started by hand) · pid 7 · v0.2.0"
        );
    }
}
