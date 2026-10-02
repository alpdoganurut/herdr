//! `herdr plus` (fork): herdr+ agents from the command line — the stdio MCP
//! server agents use, the watcher, the dashboard server, and the user-side
//! verbs (opt-in, status, the message log, the coordinator).
//!
//! This file is also the only socket adapter for `crate::plus`: [`SocketApi`]
//! implements `plus::api::Api` over the protocol-checked [`super::send_request`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::api::schema::{Method, Request};
use crate::plus::api::{self as plus_api, Api, ApiError, Verdict};
use crate::plus::launch::LaunchCtx;
use crate::plus::live::{self, LiveData};
use crate::plus::registry::{self, ManagePatch, Registry};
use crate::plus::{self, messages, DEFAULT_PORT};

const USAGE: &str =
    "usage: herdr plus <mcp|run|serve|seed|coordinator|manage|unmanage|status|messages> [--dir D]";

const HELP: &str = "herdr plus — herdr+ agents: managed agents, agent messages, a coordinator agent and its dashboard.

  herdr plus mcp [--dir D] [--port N]          stdio MCP server; started per launch by Claude and Codex
  herdr plus run [--dir D] [--port 7718] [--no-serve] [--no-coordinator] [--interval-ms 2000]
                                               singleton watcher: dashboard, coordinator, live data, wake-ups
  herdr plus serve [--dir D] [--port 7718]     dashboard HTTP server only
  herdr plus seed [--dir D]                    seed or refresh the herdr+ files and print their paths
  herdr plus coordinator start [--dir D] [--resume UUID] [--port N]
  herdr plus coordinator status [--dir D] [--json]
  herdr plus coordinator wake [--dir D]        ask the watcher for a wake-up on its next tick
  herdr plus manage <pane|name> [--role R] [--project P] [--note N] [--dir D]
  herdr plus unmanage <pane|name|session> [--dir D]
  herdr plus status [--dir D] [--json]
  herdr plus messages [--dir D] [--limit N] [--json]

The directory defaults to $HERDR_PLUS_DIR, else <config dir>/plus.";

const DEFAULT_INTERVAL_MS: u64 = 2000;
/// live.json older than this means no watcher is running.
const STALE_LIVE_S: u64 = 15;

pub(super) fn run_plus_command(args: &[String]) -> std::io::Result<i32> {
    let Some(verb) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let rest = &args[1..];
    let result = match verb {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            return Ok(0);
        }
        "mcp" => mcp(rest),
        "run" => run(rest),
        "serve" => serve(rest),
        "seed" => seed(rest),
        "coordinator" => coordinator(rest),
        "manage" => manage(rest),
        "unmanage" => unmanage(rest),
        "status" => status(rest),
        "messages" => messages_cmd(rest),
        other => Err(Fail::Usage(format!("unknown herdr plus command: {other}"))),
    };
    match result {
        Ok(code) => Ok(code),
        Err(Fail::Usage(message)) => {
            eprintln!("{message}\n{USAGE}");
            Ok(2)
        }
        Err(Fail::Error(message)) => {
            eprintln!("error: {message}");
            Ok(1)
        }
        Err(Fail::Io(err)) => Err(err),
    }
}

enum Fail {
    Usage(String),
    Error(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for Fail {
    fn from(err: std::io::Error) -> Self {
        Fail::Io(err)
    }
}

impl From<ApiError> for Fail {
    fn from(err: ApiError) -> Self {
        Fail::Error(err.to_string())
    }
}

// ----- the socket adapter -------------------------------------------------

/// The only socket adapter for herdr+.
pub(super) struct SocketApi;

impl Api for SocketApi {
    fn call(&self, method: Method) -> Result<Value, ApiError> {
        let name = serde_json::to_value(&method)
            .ok()
            .and_then(|value| value["method"].as_str().map(str::to_string))
            .unwrap_or_else(|| "request".into());
        let response = super::send_request(&Request {
            id: format!("plus:{name}"),
            method,
        })
        .map_err(|err| ApiError::new("server_unavailable", err.to_string()))?;
        response_result(response)
    }
}

fn response_result(mut response: Value) -> Result<Value, ApiError> {
    if let Some(error) = response.get("error").filter(|e| !e.is_null()) {
        return Err(ApiError::new(
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or_default(),
        ));
    }
    Ok(response
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

/// Ancestry verdict, computed once at `herdr plus mcp` startup: is the
/// environment's pane shell among this process's ancestors?
fn caller_verdict(api: &SocketApi, env_pane: Option<&str>) -> Verdict {
    let Some(env_pane) = env_pane else {
        return Verdict::Unverified;
    };
    verdict_for(
        api,
        env_pane,
        &super::browser_mcp::ancestors(std::process::id()),
    )
}

fn verdict_for(api: &impl Api, env_pane: &str, ancestors: &[u32]) -> Verdict {
    let caller = match plus_api::resolve_caller(api, env_pane) {
        Ok(caller) => caller,
        Err(err) => {
            tracing::warn!(%err, env_pane, "herdr plus mcp: caller unresolved");
            return Verdict::Unverified;
        }
    };
    match super::browser_mcp::started_from_pane(caller.shell_pid, ancestors) {
        Some(true) => Verdict::Verified,
        Some(false) => Verdict::Wrong(format!(
            "this herdr_plus MCP server was started from outside pane {} (a Codex daemon or another pane's environment), so it cannot act for that pane; relaunch the agent inside its pane (Codex: --no-daemon)",
            caller.pane_id
        )),
        None => Verdict::Unverified,
    }
}

// ----- argument parsing ---------------------------------------------------

#[derive(Debug, Default)]
struct Parsed {
    positionals: Vec<String>,
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Parsed {
    fn value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }

    fn flag(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }

    fn u64(&self, flag: &str, default: u64) -> Result<u64, Fail> {
        match self.value(flag) {
            None => Ok(default),
            Some(raw) => raw
                .parse()
                .map_err(|_| Fail::Usage(format!("invalid value for {flag}: {raw}"))),
        }
    }

    fn port(&self) -> Result<u16, Fail> {
        match self.value("--port") {
            None => Ok(DEFAULT_PORT),
            Some(raw) => raw
                .parse()
                .map_err(|_| Fail::Usage(format!("invalid value for --port: {raw}"))),
        }
    }

    /// The herdr+ directory: `--dir`, else the default; always absolute.
    fn dir(&self) -> Result<PathBuf, Fail> {
        let dir = self
            .value("--dir")
            .map(PathBuf::from)
            .unwrap_or_else(plus::plus_dir);
        Ok(absolute(dir)?)
    }

    fn at_most_positionals(&self, n: usize) -> Result<(), Fail> {
        if self.positionals.len() > n {
            return Err(Fail::Usage(format!(
                "unexpected argument: {}",
                self.positionals[n]
            )));
        }
        Ok(())
    }
}

fn absolute(dir: PathBuf) -> std::io::Result<PathBuf> {
    if dir.is_absolute() {
        Ok(dir)
    } else {
        Ok(std::env::current_dir()?.join(dir))
    }
}

/// Parse `args` against the value options and bare flags a verb accepts
/// (`--dir` is accepted everywhere); `--flag=value` works for value options.
fn parse(args: &[String], value_options: &[&str], flag_options: &[&str]) -> Result<Parsed, Fail> {
    let mut values: Vec<&str> = vec!["--dir"];
    values.extend_from_slice(value_options);
    let args = super::expand_equals_args(args, &values);
    let mut parsed = Parsed::default();
    let mut iter = args.into_iter();
    let mut options_ended = false;
    while let Some(arg) = iter.next() {
        if options_ended || !arg.starts_with("--") {
            parsed.positionals.push(arg);
        } else if arg == "--" {
            options_ended = true;
        } else if values.contains(&arg.as_str()) {
            let Some(value) = iter.next() else {
                return Err(Fail::Usage(format!("missing value for {arg}")));
            };
            parsed.values.insert(arg, value);
        } else if flag_options.contains(&arg.as_str()) {
            parsed.flags.insert(arg);
        } else {
            return Err(Fail::Usage(format!("unknown option: {arg}")));
        }
    }
    Ok(parsed)
}

// ----- delegated verbs ----------------------------------------------------

fn mcp(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--port"], &[])?;
    parsed.at_most_positionals(0)?;
    let env_pane = super::target::caller_pane_id();
    let verdict = caller_verdict(&SocketApi, env_pane.as_deref());
    let opts = plus::mcp::McpOpts {
        dir: parsed.dir()?,
        env_pane,
        verdict,
        port: parsed.port()?,
    };
    Ok(plus::mcp::run(SocketApi, opts)?)
}

fn run(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(
        args,
        &["--port", "--interval-ms"],
        &["--no-serve", "--no-coordinator"],
    )?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    let port = parsed.port()?;
    let opts = plus::watch::WatchOpts {
        ctx: LaunchCtx::current(dir.clone(), port)?,
        dir,
        port,
        serve: !parsed.flag("--no-serve"),
        coordinator: !parsed.flag("--no-coordinator"),
        interval_ms: parsed.u64("--interval-ms", DEFAULT_INTERVAL_MS)?.max(200),
    };
    Ok(plus::watch::run(SocketApi, opts)?)
}

fn serve(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--port"], &[])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    let port = parsed.port()?;
    plus::seed(&dir)?;
    // Bind before announcing the URL, so a taken port fails without a
    // misleading "dashboard on" line.
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    println!(
        "herdr+ dashboard on http://127.0.0.1:{port}/ ({})",
        dir.display()
    );
    plus::serve::serve_on(dir, listener)?;
    Ok(0)
}

// ----- direct verbs -------------------------------------------------------

fn seed(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &[], &[])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    plus::seed(&dir)?;
    println!("herdr+ directory: {}", dir.display());
    for path in [
        plus::instructions_path(&dir),
        plus::dashboard_dir(&dir).join("index.html"),
        plus::dashboard_dir(&dir).join("template.html"),
        plus::board_path(&dir),
        plus::memory_index_path(&dir),
        plus::registry_path(&dir),
        plus::messages_path(&dir),
        plus::live_path(&dir),
    ] {
        println!("  {}", path.display());
    }
    Ok(0)
}

fn coordinator(args: &[String]) -> Result<i32, Fail> {
    let Some(verb) = args.first().map(String::as_str) else {
        return Err(Fail::Usage(
            "usage: herdr plus coordinator <start|status|wake>".into(),
        ));
    };
    let rest = &args[1..];
    match verb {
        "start" => {
            let parsed = parse(rest, &["--resume", "--port"], &[])?;
            parsed.at_most_positionals(0)?;
            let dir = parsed.dir()?;
            plus::seed(&dir)?;
            let ctx = LaunchCtx::current(dir.clone(), parsed.port()?)?;
            let pane =
                plus::watch::coordinator_start(&SocketApi, &dir, &ctx, parsed.value("--resume"))
                    .map_err(Fail::Error)?;
            println!("coordinator starting in {pane}");
            Ok(0)
        }
        "status" => {
            let parsed = parse(rest, &[], &["--json"])?;
            parsed.at_most_positionals(0)?;
            let dir = parsed.dir()?;
            let report = coordinator_report(&SocketApi, &dir, plus::now_unix());
            if parsed.flag("--json") {
                println!("{report}");
            } else {
                print!("{}", format_coordinator_report(&report));
            }
            Ok(0)
        }
        "wake" => {
            let parsed = parse(rest, &[], &[])?;
            parsed.at_most_positionals(0)?;
            let dir = parsed.dir()?;
            let now = plus::now_unix();
            plus::write_atomically(
                &plus::wake_request_path(&dir),
                format!("{now}\n").as_bytes(),
            )?;
            println!("wake requested; the watcher sends it once the coordinator is idle");
            if watcher_age(&dir, now).is_none_or(|age| age > STALE_LIVE_S) {
                eprintln!("warning: live.json is missing or stale; is `herdr plus run` running?");
            }
            Ok(0)
        }
        other => Err(Fail::Usage(format!(
            "unknown herdr plus coordinator command: {other}"
        ))),
    }
}

/// Seconds since the watcher last wrote live.json.
fn watcher_age(dir: &Path, now: u64) -> Option<u64> {
    let live = read_json(&plus::live_path(dir))?;
    Some(now.saturating_sub(live["generated_unix"].as_u64()?))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The coordinator's registry entry, live status, turn marker and the
/// watcher's wake summary, as one JSON object.
fn coordinator_report(api: &impl Api, dir: &Path, now: u64) -> Value {
    let registry = Registry::load(dir);
    let entry = registry.coordinator().cloned();
    let live_agent = entry.as_ref().and_then(|entry| {
        let agents = plus_api::agents(api).ok()?;
        agents.into_iter().find(|agent| {
            let session = live::session_of(agent);
            (entry.session.is_some() && session == entry.session)
                || (entry.pane_id.is_some()
                    && agent["pane_id"].as_str() == entry.pane_id.as_deref()
                    && (entry.session.is_none() || session.is_none()))
        })
    });
    let turn = plus::turn::read_live(dir, now);
    let live_file = read_json(&plus::live_path(dir));
    json!({
        "registered": entry.is_some(),
        "entry": entry,
        "live": live_agent.as_ref().map(|agent| json!({
            "pane_id": agent["pane_id"],
            "status": agent["agent_status"],
            "session": live::session_of(agent),
        })),
        "turn": turn.map(|turn| json!({
            "source": turn.source,
            "id": turn.id,
            "age_s": now.saturating_sub(turn.started_unix),
            "seen_working": turn.seen_working,
        })),
        "watcher_age_s": live_file.as_ref().and_then(|live| live["generated_unix"].as_u64()).map(|at| now.saturating_sub(at)),
        "watch": live_file.as_ref().map(|live| live["watch"].clone()).unwrap_or(Value::Null),
        "wake_requested": plus::wake_request_path(dir).exists(),
    })
}

fn format_coordinator_report(report: &Value) -> String {
    let mut out = String::new();
    let entry = &report["entry"];
    if report["registered"].as_bool() == Some(true) {
        out.push_str(&format!(
            "coordinator: pane {} session {} ({})\n",
            entry["pane_id"].as_str().unwrap_or("?"),
            entry["session"].as_str().unwrap_or("?"),
            entry["agent"].as_str().unwrap_or("?"),
        ));
    } else {
        out.push_str(
            "coordinator: none registered (`herdr plus run` or `herdr plus coordinator start`)\n",
        );
    }
    match report["live"].as_object() {
        Some(live) => out.push_str(&format!(
            "live: {} in {}\n",
            live.get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            live.get("pane_id").and_then(Value::as_str).unwrap_or("?"),
        )),
        None => out.push_str("live: not running\n"),
    }
    match report["turn"].as_object() {
        Some(turn) => out.push_str(&format!(
            "turn: {} {} for {}s{}\n",
            turn.get("source").and_then(Value::as_str).unwrap_or("?"),
            turn.get("id").and_then(Value::as_str).unwrap_or("?"),
            turn.get("age_s").and_then(Value::as_u64).unwrap_or(0),
            if turn.get("seen_working").and_then(Value::as_bool) == Some(true) {
                ", working"
            } else {
                ""
            },
        )),
        None => out.push_str("turn: none (the next turn is the user's)\n"),
    }
    match report["watcher_age_s"].as_u64() {
        Some(age) if age <= STALE_LIVE_S => {
            out.push_str(&format!("watcher: live.json {age}s old\n"))
        }
        Some(age) => out.push_str(&format!(
            "watcher: live.json {age}s old (stale; is `herdr plus run` running?)\n"
        )),
        None => out.push_str("watcher: no live.json (`herdr plus run` not started)\n"),
    }
    let watch = &report["watch"];
    if watch.is_object() {
        let n = |key: &str| watch[key].as_u64().unwrap_or(0);
        let mut flags = String::new();
        if watch["capped"].as_bool() == Some(true) {
            flags.push_str(" capped");
        }
        if watch["coordinator_down"].as_bool() == Some(true) {
            flags.push_str(" coordinator_down");
        }
        let last = match n("last_wake_unix") {
            0 => "never".to_string(),
            at => clock(at),
        };
        out.push_str(&format!(
            "wakes: #{} last {} · {} pending · {} in the last hour{}\n",
            n("wake_seq"),
            last,
            n("pending"),
            n("wakes_last_hour"),
            flags
        ));
    }
    if report["wake_requested"].as_bool() == Some(true) {
        out.push_str("wake requested (waiting for the watcher)\n");
    }
    out
}

fn manage(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--role", "--project", "--note"], &[])?;
    parsed.at_most_positionals(1)?;
    let Some(target) = parsed.positionals.first() else {
        return Err(Fail::Usage(
            "usage: herdr plus manage <pane|name> [--role R] [--project P] [--note N]".into(),
        ));
    };
    let patch = ManagePatch {
        role: parsed.value("--role").map(str::to_string),
        project: parsed.value("--project").map(str::to_string),
        note: parsed.value("--note").map(str::to_string),
    };
    let line = manage_target(&SocketApi, &parsed.dir()?, target, &patch).map_err(Fail::Error)?;
    println!("{line}");
    Ok(0)
}

/// Opt a live agent in (or update it); the user-side path, so it may also
/// hand out the coordinator role.
fn manage_target(
    api: &impl Api,
    dir: &Path,
    target: &str,
    patch: &ManagePatch,
) -> Result<String, String> {
    let agent = plus_api::agent_get(api, target).map_err(|err| err.to_string())?;
    let pane = agent["pane_id"]
        .as_str()
        .ok_or_else(|| format!("{target} has no pane"))?
        .to_string();
    let session = live::session_of(&agent);
    let kind = agent["agent"]
        .as_str()
        .or_else(|| agent["agent_session"]["agent"].as_str())
        .map(str::to_string);
    let entry = registry::update(dir, |registry| {
        registry.manage(session.as_deref(), Some(&pane), kind.as_deref(), patch)
    })?;
    let name = agent["name"].as_str().unwrap_or(&pane);
    Ok(format!(
        "managed {name} ({pane}, {}){}{}{}",
        entry.agent.as_deref().unwrap_or("agent"),
        tag(" role=", &entry.role),
        tag(" project=", &entry.project),
        tag(" note=", &entry.note),
    ))
}

fn tag(prefix: &str, value: &Option<String>) -> String {
    value
        .as_deref()
        .map(|value| format!("{prefix}{value}"))
        .unwrap_or_default()
}

fn unmanage(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &[], &[])?;
    parsed.at_most_positionals(1)?;
    let Some(target) = parsed.positionals.first() else {
        return Err(Fail::Usage(
            "usage: herdr plus unmanage <pane|name|session>".into(),
        ));
    };
    let line = unmanage_target(&SocketApi, &parsed.dir()?, target).map_err(Fail::Error)?;
    println!("{line}");
    Ok(0)
}

/// Opt an agent out: a live agent by pane or name, or an offline entry by
/// its recorded pane id or session id.
fn unmanage_target(api: &impl Api, dir: &Path, target: &str) -> Result<String, String> {
    let (session, pane) = match plus_api::agent_get(api, target) {
        Ok(agent) => (
            live::session_of(&agent),
            agent["pane_id"].as_str().map(str::to_string),
        ),
        Err(err) if err.code == "server_unavailable" => return Err(err.to_string()),
        Err(_) => (None, None),
    };
    let removed = registry::update(dir, |registry| {
        let index = registry
            .find(session.as_deref(), pane.as_deref())
            .or_else(|| {
                registry.agents.iter().position(|entry| {
                    entry.pane_id.as_deref() == Some(target)
                        || entry.session.as_deref() == Some(target)
                })
            });
        Ok(index.map(|index| registry.agents.remove(index)))
    })?;
    match removed {
        Some(entry) => Ok(format!(
            "unmanaged {} ({})",
            entry.pane_id.as_deref().unwrap_or("?"),
            entry.session.as_deref().unwrap_or("no session"),
        )),
        None => Err(format!("{target} is not a managed agent")),
    }
}

fn status(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &[], &["--json"])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    let now = plus::now_unix();
    let (live, fresh) = match live_now(&SocketApi, &dir, now) {
        Ok(live) => (live, true),
        Err(err) => {
            eprintln!("warning: {err}; showing the watcher's live.json");
            match read_json(&plus::live_path(&dir)) {
                Some(live) => (live, false),
                None => return Err(Fail::Error(format!("{err} and no live.json"))),
            }
        }
    };
    if parsed.flag("--json") {
        println!("{live}");
    } else {
        print!("{}", format_status(&live, fresh, now));
    }
    Ok(0)
}

/// Fresh live data from the API (managed agents only), with the watcher's
/// wake summary and change times taken from live.json when present. Read-only:
/// relinks are left to the watcher.
fn live_now(api: &impl Api, dir: &Path, now: u64) -> Result<Value, ApiError> {
    let agents = plus_api::agents(api)?;
    let workspaces = plus_api::workspaces(api)?;
    let tabs = plus_api::tabs(api)?;
    let file = read_json(&plus::live_path(dir));
    let last_change: HashMap<String, u64> = file
        .as_ref()
        .and_then(|live| live["agents"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|agent| {
            Some((
                agent["pane_id"].as_str()?.to_string(),
                agent["last_change_unix"].as_u64()?,
            ))
        })
        .collect();
    let mut registry = Registry::load(dir);
    let inputs = live::Inputs {
        agents: &agents,
        workspaces: &workspaces,
        tabs: &tabs,
    };
    let recent = messages::recent(dir, 30, None);
    let (data, _relinked): (LiveData, bool) =
        live::build(&inputs, &mut registry, recent, &last_change, false, now);
    let mut value = serde_json::to_value(&data)
        .map_err(|err| ApiError::new("bad_response", err.to_string()))?;
    if let (Some(object), Some(watch)) = (
        value.as_object_mut(),
        file.as_ref().map(|live| live["watch"].clone()),
    ) {
        if !watch.is_null() {
            object.insert("watch".into(), watch);
        }
    }
    Ok(value)
}

fn format_status(live: &Value, fresh: bool, now: u64) -> String {
    let mut out = String::new();
    if !fresh {
        let at = live["generated_unix"].as_u64().unwrap_or(0);
        out.push_str(&format!(
            "(live.json from {}, {}s ago)\n",
            clock(at),
            now.saturating_sub(at)
        ));
    }
    let empty = Vec::new();
    let agents = live["agents"].as_array().unwrap_or(&empty);
    for agent in agents {
        let s = |key: &str| agent[key].as_str().unwrap_or("");
        let role_project = match (s("role"), s("project")) {
            ("", "") => "-".to_string(),
            (role, "") => role.to_string(),
            ("", project) => format!("-/{project}"),
            (role, project) => format!("{role}/{project}"),
        };
        let since = match agent["last_change_unix"].as_u64() {
            Some(at) if at > 0 => format!("  {}", age(now.saturating_sub(at))),
            _ => String::new(),
        };
        let session = s("session");
        let mut line = format!(
            "{:<8} {:<14} {:<7} {:<18} {:<9}{}  grp={}",
            s("pane_id"),
            s("name"),
            if s("agent").is_empty() {
                "-"
            } else {
                s("agent")
            },
            role_project,
            s("status"),
            since,
            if s("group").is_empty() {
                "-"
            } else {
                s("group")
            },
        );
        if !session.is_empty() {
            line.push_str(&format!("  sess={}", &session[..session.len().min(8)]));
        }
        if agent["coordinator"].as_bool() == Some(true) {
            line.push_str("  [coordinator]");
        }
        if !s("note").is_empty() {
            line.push_str(&format!("  \"{}\"", s("note")));
        }
        out.push_str(&line);
        out.push('\n');
    }
    let offline = live["offline"].as_array().unwrap_or(&empty);
    for entry in offline {
        let s = |key: &str| entry[key].as_str().unwrap_or("-");
        out.push_str(&format!(
            "{:<8} offline  {} {}/{}\n",
            s("pane_id"),
            s("agent"),
            s("role"),
            s("project")
        ));
    }
    out.push_str(&format!(
        "{} managed, {} offline, {} unmanaged\n",
        agents.len(),
        offline.len(),
        live["unmanaged_count"].as_u64().unwrap_or(0)
    ));
    let watch = &live["watch"];
    if watch.is_object() {
        out.push_str(&format!(
            "wake #{} · {} pending · {} this hour{}{}\n",
            watch["wake_seq"].as_u64().unwrap_or(0),
            watch["pending"].as_u64().unwrap_or(0),
            watch["wakes_last_hour"].as_u64().unwrap_or(0),
            if watch["capped"].as_bool() == Some(true) {
                " · capped"
            } else {
                ""
            },
            if watch["turn_live"].as_bool() == Some(true) {
                " · turn live"
            } else {
                ""
            },
        ));
    }
    out
}

fn messages_cmd(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--limit"], &["--json"])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    let limit = parsed.u64("--limit", 20)?.clamp(1, 10_000) as usize;
    let log = messages::recent(&dir, limit, None);
    if parsed.flag("--json") {
        for message in &log {
            println!(
                "{}",
                serde_json::to_string(message).map_err(std::io::Error::other)?
            );
        }
        return Ok(0);
    }
    if log.is_empty() {
        println!("no agent messages yet");
    }
    for message in &log {
        println!("{}", format_message(message));
    }
    Ok(0)
}

fn format_message(message: &messages::AgentMessage) -> String {
    let party = |name: Option<&str>, pane: Option<&str>| match (name, pane) {
        (Some(name), Some(pane)) => format!("{name}({pane})"),
        (None, Some(pane)) => pane.to_string(),
        (Some(name), None) => name.to_string(),
        (None, None) => "outside".to_string(),
    };
    let text = message.text.replace('\n', " ");
    let cut: String = text.chars().take(160).collect();
    let ellipsis = if text.chars().count() > 160 {
        "…"
    } else {
        ""
    };
    format!(
        "{} {} -> {} [{}] {cut}{ellipsis}",
        clock(message.unix),
        party(message.from_name.as_deref(), message.from_pane.as_deref()),
        party(message.to_name.as_deref(), Some(&message.to_pane)),
        message.outcome,
    )
}

/// `hh:mm` in local time.
fn clock(unix: u64) -> String {
    let offset = crate::platform::local_datetime()
        .map(|local| local.assume_utc().unix_timestamp() - plus::now_unix() as i64)
        .map(|seconds| (seconds as f64 / 60.0).round() as i64 * 60)
        .unwrap_or(0);
    let seconds = (unix as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

fn age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-cli-plus-{name}-{}-{}",
            std::process::id(),
            plus::launch::new_uuid()
        ));
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    #[test]
    fn parses_values_flags_equals_forms_and_rejects_unknown_options() {
        let parsed = parse(
            &strings(&[
                "rev",
                "--role=reviewer",
                "--dir",
                "/tmp/p",
                "--json",
                "--",
                "--x",
            ]),
            &["--role"],
            &["--json"],
        )
        .ok()
        .expect("parses");
        assert_eq!(parsed.positionals, ["rev", "--x"]);
        assert_eq!(parsed.value("--role"), Some("reviewer"));
        assert!(parsed.flag("--json"));
        assert_eq!(parsed.dir().ok(), Some(PathBuf::from("/tmp/p")));
        assert!(matches!(
            parse(&strings(&["--bogus"]), &[], &[]),
            Err(Fail::Usage(_))
        ));
        assert!(matches!(
            parse(&strings(&["--role"]), &["--role"], &[]),
            Err(Fail::Usage(_))
        ));
        let relative = parse(&strings(&["--dir", "rel"]), &[], &[])
            .ok()
            .expect("parses");
        let dir = relative.dir().ok().expect("dir");
        assert!(dir.is_absolute() && dir.ends_with("rel"));
        let port = parse(&strings(&["--port", "x"]), &["--port"], &[])
            .ok()
            .expect("parses");
        assert!(matches!(port.port(), Err(Fail::Usage(_))));
        let default = parse(&[], &["--port"], &[]).ok().expect("parses");
        assert_eq!(default.port().ok(), Some(DEFAULT_PORT));
    }

    #[test]
    fn response_errors_map_to_api_errors() {
        let ok = response_result(json!({ "id": "x", "result": { "type": "ok" } })).unwrap();
        assert_eq!(ok["type"], "ok");
        let err = response_result(
            json!({ "id": "x", "error": { "code": "agent_blocked", "message": "blocked" } }),
        )
        .unwrap_err();
        assert_eq!(err, ApiError::new("agent_blocked", "blocked"));
    }

    fn actor_api(shell_pid: Option<u32>) -> impl Fn(Method) -> Result<Value, ApiError> {
        move |method| match method {
            Method::BrowserResolveCaller(_) => Ok(json!({ "type": "browser_actor", "actor": {
                "kind": "pane", "pane_id": "w2:p3", "tab_id": "w2:t1", "workspace_id": "w2",
                "shell_pid": shell_pid } })),
            _ => Err(ApiError::new("unexpected", "")),
        }
    }

    #[test]
    fn verdict_follows_the_process_ancestry() {
        assert_eq!(
            verdict_for(&actor_api(Some(40)), "w2:p3", &[50, 40, 1]),
            Verdict::Verified
        );
        assert!(matches!(
            verdict_for(&actor_api(Some(41)), "w2:p3", &[50, 40]),
            Verdict::Wrong(text) if text.contains("w2:p3")
        ));
        assert_eq!(
            verdict_for(&actor_api(Some(40)), "w2:p3", &[]),
            Verdict::Unverified
        );
        assert_eq!(
            verdict_for(&actor_api(None), "w2:p3", &[50]),
            Verdict::Unverified
        );
        let down =
            |_: Method| -> Result<Value, ApiError> { Err(ApiError::new("server_unavailable", "")) };
        assert_eq!(verdict_for(&down, "w2:p3", &[50]), Verdict::Unverified);
    }

    fn agent_api() -> impl Fn(Method) -> Result<Value, ApiError> {
        |method| match method {
            Method::AgentGet(target) if target.target == "rev" || target.target == "w2:p4" => {
                Ok(json!({ "type": "agent_info", "agent": {
                    "pane_id": "w2:p4", "name": "rev", "agent": "codex", "agent_status": "idle",
                    "agent_session": { "agent": "codex", "value": "s-rev" } } }))
            }
            Method::AgentGet(_) => Err(ApiError::new("agent_not_found", "no such agent")),
            _ => Err(ApiError::new("unexpected", "")),
        }
    }

    #[test]
    fn manage_and_unmanage_go_through_the_registry() {
        let dir = test_dir("manage");
        let patch = ManagePatch {
            role: Some("reviewer".into()),
            project: Some("demo".into()),
            note: None,
        };
        assert_eq!(
            manage_target(&agent_api(), &dir, "rev", &patch).unwrap(),
            "managed rev (w2:p4, codex) role=reviewer project=demo"
        );
        let registry = Registry::load(&dir);
        assert_eq!(registry.agents.len(), 1);
        assert_eq!(registry.agents[0].session.as_deref(), Some("s-rev"));
        // The user-side path may hand out the coordinator role.
        let coordinator = ManagePatch {
            role: Some(plus::COORDINATOR_ROLE.into()),
            ..ManagePatch::default()
        };
        manage_target(&agent_api(), &dir, "w2:p4", &coordinator).unwrap();
        assert!(Registry::load(&dir).coordinator().is_some());
        assert!(manage_target(&agent_api(), &dir, "ghost", &patch)
            .unwrap_err()
            .contains("agent_not_found"));
        // Offline entries are removed by their recorded session id.
        assert_eq!(
            unmanage_target(&agent_api(), &dir, "s-rev").unwrap(),
            "unmanaged w2:p4 (s-rev)"
        );
        assert!(Registry::load(&dir).agents.is_empty());
        assert!(unmanage_target(&agent_api(), &dir, "rev")
            .unwrap_err()
            .contains("not a managed agent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_builds_fresh_live_data_with_the_watchers_summary() {
        let dir = test_dir("status");
        registry::update(&dir, |registry| {
            registry.manage(
                Some("s-rev"),
                Some("w2:p4"),
                Some("codex"),
                &ManagePatch {
                    role: Some("reviewer".into()),
                    project: Some("demo".into()),
                    note: None,
                },
            )
        })
        .unwrap();
        plus::write_atomically(
            &plus::live_path(&dir),
            br#"{"generated_unix":90,"agents":[{"pane_id":"w2:p4","last_change_unix":40}],"watch":{"wake_seq":3,"pending":2,"wakes_last_hour":1}}"#,
        )
        .unwrap();
        let api = |method: Method| -> Result<Value, ApiError> {
            Ok(match method {
                Method::AgentList(_) => json!({ "agents": [
                    { "pane_id": "w2:p4", "tab_id": "w2:t1", "workspace_id": "w2", "name": "rev",
                      "agent": "codex", "agent_status": "working",
                      "agent_session": { "agent": "codex", "value": "s-rev" } },
                    { "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "agent_status": "idle" } ] }),
                Method::WorkspaceList(_) => {
                    json!({ "workspaces": [{ "workspace_id": "w2", "label": "demo", "tab_count": 1 }] })
                }
                Method::TabList(_) => json!({ "tabs": [] }),
                _ => json!({}),
            })
        };
        let live = live_now(&api, &dir, 100).unwrap();
        assert_eq!(live["agents"].as_array().map(Vec::len), Some(1));
        assert_eq!(live["agents"][0]["last_change_unix"], 40);
        assert_eq!(live["watch"]["wake_seq"], 3);
        let text = format_status(&live, true, 100);
        assert!(text.contains("w2:p4"), "{text}");
        assert!(text.contains("reviewer/demo"), "{text}");
        assert!(text.contains("working    1m  grp=demo"), "{text}");
        assert!(text.contains("sess=s-rev"), "{text}");
        assert!(text.contains("1 managed, 0 offline, 1 unmanaged"), "{text}");
        assert!(text.contains("wake #3 · 2 pending · 1 this hour"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordinator_report_reads_registry_live_status_and_watch() {
        let dir = test_dir("coordinator");
        registry::update(&dir, |registry| {
            registry.manage(
                Some("s-c"),
                Some("w1:p1"),
                Some("claude"),
                &ManagePatch {
                    role: Some(plus::COORDINATOR_ROLE.into()),
                    project: Some("herdr+".into()),
                    note: None,
                },
            )
        })
        .unwrap();
        plus::write_atomically(
            &plus::live_path(&dir),
            br#"{"generated_unix":95,"watch":{"wake_seq":7,"last_wake_unix":0,"capped":true}}"#,
        )
        .unwrap();
        std::fs::write(plus::wake_request_path(&dir), "1").unwrap();
        let api = |method: Method| -> Result<Value, ApiError> {
            Ok(match method {
                Method::AgentList(_) => json!({ "agents": [
                    { "pane_id": "w5:p2", "agent_status": "idle",
                      "agent_session": { "agent": "claude", "value": "s-c" } } ] }),
                _ => json!({}),
            })
        };
        let report = coordinator_report(&api, &dir, 100);
        assert_eq!(report["live"]["pane_id"], "w5:p2", "matched by session");
        assert_eq!(report["watcher_age_s"], 5);
        let text = format_coordinator_report(&report);
        assert!(
            text.contains("coordinator: pane w1:p1 session s-c (claude)"),
            "{text}"
        );
        assert!(text.contains("live: idle in w5:p2"), "{text}");
        assert!(text.contains("watcher: live.json 5s old"), "{text}");
        assert!(text.contains("wakes: #7 last never"), "{text}");
        assert!(text.contains("capped"), "{text}");
        assert!(text.contains("wake requested"), "{text}");
        let empty = test_dir("coordinator-none");
        let none = coordinator_report(&api, &empty, 100);
        assert!(format_coordinator_report(&none).contains("none registered"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn message_rows_are_compact() {
        let message = messages::AgentMessage {
            from_pane: Some("w2:p3".into()),
            from_name: Some("lead".into()),
            to_pane: "w2:p4".into(),
            to_name: Some("rev".into()),
            text: format!("line\n{}", "x".repeat(200)),
            outcome: "sent".into(),
            ..messages::AgentMessage::default()
        };
        let row = format_message(&message);
        assert!(
            row.contains(" lead(w2:p3) -> rev(w2:p4) [sent] line x"),
            "{row}"
        );
        assert!(row.ends_with('…'));
        assert_eq!(age(59), "59s");
        assert_eq!(age(61), "1m");
        assert_eq!(age(7200), "2h");
    }
}
