//! `herdr coordinator` (fork): the coordinator and its managed agents from the
//! command line. The herdr server runs the coordinator and serves its
//! dashboard; these verbs switch it on or off, start, wake and inspect it over
//! the `coordinator.*` API methods, and keep the file-based verbs (the stdio
//! MCP server agents use, seeding, the registry, the message log and the turn
//! marker).
//!
//! This file is also the only socket adapter for `crate::coordinator`: [`SocketApi`]
//! implements `coordinator::api::Api` over the protocol-checked [`super::send_request`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::api::schema::coordinator::{
    method, CoordinatorGetInfo, CoordinatorOpenDashboardParams, CoordinatorSetEnabledParams,
    CoordinatorStartParams, CoordinatorStateInfo, CoordinatorWakeParams,
};
use crate::api::schema::{Method, Request};
use crate::coordinator::api::{self as coordinator_api, Api, ApiError, Verdict};
use crate::coordinator::registry::{self, ManagePatch, Registry};
use crate::coordinator::{self, messages, DEFAULT_PORT};

const USAGE: &str = "usage: herdr coordinator <enable|disable|start|wake|status|dashboard|messages|manage|unmanage|clear-turn|seed|mcp>";

const HELP: &str = "herdr coordinator — the coordinator: a Claude Code agent the herdr server runs in a pinned
`coordinator` tab, the agents it manages, their messages and its dashboard.

  herdr coordinator enable | disable           switch the coordinator on or off ([coordinator] enabled)
  herdr coordinator start [--new]              start or restart the coordinator agent (--new: a fresh session)
  herdr coordinator wake                       wake the coordinator now (it reads what changed)
  herdr coordinator status [--json]            state, wake-ups, dashboard and the managed agents
  herdr coordinator dashboard [--print]        open the dashboard (herdr's browser, else the system browser);
                                               --print only prints its URL
  herdr coordinator messages [--dir D] [--limit N] [--json]
                                               the agent message log
  herdr coordinator manage <pane|name> [--role R] [--project P] [--note N] [--dir D]
  herdr coordinator unmanage <pane|name|session> [--dir D]
  herdr coordinator clear-turn [--dir D]       end the coordinator's turn marker (its write tools refuse
                                               while one is live; for a turn stuck working or blocked)
  herdr coordinator seed [--dir D]             seed or refresh the coordinator files and print their paths
  herdr coordinator mcp [--dir D] [--port N]   stdio MCP server; started per launch by Claude and Codex

The dashboard is served by the herdr server on 127.0.0.1:<[coordinator] dashboard_port> (default 7718).
start, wake, enable, disable, manage, unmanage and clear-turn refuse when the coordinator agent runs
them in a turn herdr started (a wake-up or an agent message).

Environment (read by the herdr server unless noted):
  HERDR_COORDINATOR_DIR                the coordinator directory (default <config dir>/coordinator;
                                       also read by these verbs)
  HERDR_COORDINATOR_WAKE_DEBOUNCE_S    wake-up debounce for normal items (default 60)
  HERDR_COORDINATOR_WAKE_GAP_S         minimum gap between wake-ups (default 120)
  HERDR_COORDINATOR_PERIODIC_S         periodic check when anything is pending (default 3600)
  HERDR_COORDINATOR_WAKE_CAP_HOUR      wake-ups per hour (overrides [coordinator] cap_hour)
  HERDR_COORDINATOR_WAKE_CAP_DAY       wake-ups per day (overrides [coordinator] cap_day)
  HERDR_COORDINATOR_SYSPROMPT_FILE=1   also pass coordinator.md as the coordinator's system prompt file
  HERDR_COORDINATOR_CODEX_NO_DAEMON=1  start Codex agents with --no-daemon (when no shell hook adds it)

Every verb takes -h/--help.";

pub(super) fn run_coordinator_command(args: &[String]) -> std::io::Result<i32> {
    let Some(verb) = args.first().map(String::as_str) else {
        eprintln!("{HELP}");
        return Ok(2);
    };
    let rest = &args[1..];
    let server = Server {
        call: &socket_call,
        caller_pane: caller_pane(),
        remote: super::target::is_remote(),
    };
    let result = match verb {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            return Ok(0);
        }
        "enable" => set_enabled(&server, rest, true),
        "disable" => set_enabled(&server, rest, false),
        "start" => start(&server, rest),
        "wake" => wake(&server, rest),
        "status" => status(&server, rest),
        "dashboard" => dashboard(&server, rest),
        "mcp" => mcp(rest),
        "seed" => seed(rest),
        "clear-turn" => clear_turn(rest),
        "manage" => manage(rest),
        "unmanage" => unmanage(rest),
        "messages" => messages_cmd(rest),
        other => Err(Fail::Usage(format!(
            "unknown herdr coordinator command: {other}"
        ))),
    };
    match result {
        Ok(code) => Ok(code),
        Err(Fail::Help) => {
            println!("{HELP}");
            Ok(0)
        }
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

#[derive(Debug)]
enum Fail {
    /// `-h`/`--help` anywhere among a verb's options.
    Help,
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

/// The only socket adapter for the coordinator.
pub(super) struct SocketApi;

impl Api for SocketApi {
    fn call(&self, method: Method) -> Result<Value, ApiError> {
        let name = serde_json::to_value(&method)
            .ok()
            .and_then(|value| value["method"].as_str().map(str::to_string))
            .unwrap_or_else(|| "request".into());
        let response = super::send_request(&Request {
            id: format!("coordinator:{name}"),
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

/// Ancestry verdict, computed once at `herdr coordinator mcp` startup: is the
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
    let caller = match coordinator_api::resolve_caller(api, env_pane) {
        Ok(caller) => caller,
        Err(err) => {
            tracing::warn!(%err, env_pane, "herdr coordinator mcp: caller unresolved");
            return Verdict::Unverified;
        }
    };
    match super::browser_mcp::started_from_pane(caller.shell_pid, ancestors) {
        Some(true) => Verdict::Verified,
        Some(false) => Verdict::Wrong(format!(
            "this herdr_agents MCP server was started from outside pane {} (a Codex daemon or another pane's environment), so it cannot act for that pane; relaunch the agent inside its pane (Codex: --no-daemon)",
            caller.pane_id
        )),
        None => Verdict::Unverified,
    }
}

// ----- the coordinator.* methods ------------------------------------------

/// Sends one `coordinator.*` request; `Ok` is the response's `result`.
type Call<'a> = &'a dyn Fn(&str, Value) -> Result<Value, Fail>;

/// The server side of the verbs: the transport, the calling pane
/// (`HERDR_PANE_ID`, for the server's turn guard) and whether the server is
/// on another machine (SSH or Cloud).
struct Server<'a> {
    call: Call<'a>,
    caller_pane: Option<String>,
    remote: bool,
}

impl Server<'_> {
    /// One request; the raw `info` object every coordinator method answers with.
    fn request_raw<P: Serialize>(&self, method: &str, params: &P) -> Result<Value, Fail> {
        let params = serde_json::to_value(params).map_err(std::io::Error::other)?;
        let mut result = (self.call)(method, params)?;
        match result.get_mut("info").map(Value::take) {
            Some(info) if info.is_object() => Ok(info),
            _ => Err(Fail::Error(format!(
                "the server sent no coordinator info for {method}"
            ))),
        }
    }

    fn request<P: Serialize>(&self, method: &str, params: &P) -> Result<CoordinatorGetInfo, Fail> {
        let info = self.request_raw(method, params)?;
        serde_json::from_value(info)
            .map_err(|err| Fail::Error(format!("unreadable coordinator info: {err}")))
    }
}

/// Builds the request from its wire name, so the verbs need no `Method` arm
/// of their own: a herdr build without the `coordinator.*` methods fails here,
/// before anything reaches the socket.
fn build_request(method: &str, params: Value) -> Result<Request, Fail> {
    serde_json::from_value(json!({
        "id": format!("coordinator:{method}"),
        "method": method,
        "params": params,
    }))
    .map_err(|_| {
        Fail::Error(format!(
            "this herdr build does not know {method}: it has no coordinator.* API yet"
        ))
    })
}

fn socket_call(method: &str, params: Value) -> Result<Value, Fail> {
    let request = build_request(method, params)?;
    // Connection failures were already reported (server not running,
    // protocol mismatch): pass them through untouched.
    let response = super::send_request(&request)?;
    Ok(response_result(response)?)
}

fn set_enabled(server: &Server<'_>, args: &[String], enabled: bool) -> Result<i32, Fail> {
    let parsed = parse_server(args, &[], &[])?;
    parsed.at_most_positionals(0)?;
    let info = server.request(
        method::SET_ENABLED,
        &CoordinatorSetEnabledParams {
            enabled,
            caller_pane: server.caller_pane.clone(),
        },
    )?;
    println!(
        "coordinator {}; {}",
        if info.enabled { "enabled" } else { "disabled" },
        state_line(&info)
    );
    Ok(0)
}

fn start(server: &Server<'_>, args: &[String]) -> Result<i32, Fail> {
    let parsed = parse_server(args, &[], &["--new"])?;
    parsed.at_most_positionals(0)?;
    let fresh = parsed.flag("--new");
    let info = server.request(
        method::START,
        &CoordinatorStartParams {
            resume: !fresh,
            caller_pane: server.caller_pane.clone(),
        },
    )?;
    println!(
        "coordinator {}; {}",
        if fresh {
            "starting a new session"
        } else {
            "starting (resuming its session)"
        },
        state_line(&info)
    );
    Ok(0)
}

fn wake(server: &Server<'_>, args: &[String]) -> Result<i32, Fail> {
    let parsed = parse_server(args, &[], &[])?;
    parsed.at_most_positionals(0)?;
    let info = server.request(
        method::WAKE,
        &CoordinatorWakeParams {
            caller_pane: server.caller_pane.clone(),
        },
    )?;
    println!(
        "wake requested; it is sent once the coordinator is idle. {}",
        state_line(&info)
    );
    Ok(0)
}

fn status(server: &Server<'_>, args: &[String]) -> Result<i32, Fail> {
    let parsed = parse_server(args, &[], &["--json"])?;
    parsed.at_most_positionals(0)?;
    let raw = server.request_raw(method::GET, &json!({}))?;
    if parsed.flag("--json") {
        println!("{raw}");
        return Ok(0);
    }
    let info: CoordinatorGetInfo = serde_json::from_value(raw)
        .map_err(|err| Fail::Error(format!("unreadable coordinator info: {err}")))?;
    print!("{}", format_info(&info, coordinator::now_unix()));
    Ok(0)
}

fn dashboard(server: &Server<'_>, args: &[String]) -> Result<i32, Fail> {
    let parsed = parse_server(args, &[], &["--print"])?;
    parsed.at_most_positionals(0)?;
    // 127.0.0.1 on a remote server is not this machine, and opening it there
    // would show it on the wrong screen: only print it.
    let open = !parsed.flag("--print") && !server.remote;
    let info = server.request(
        method::OPEN_DASHBOARD,
        &CoordinatorOpenDashboardParams { open },
    )?;
    let Some(url) = info.dashboard_url.as_deref() else {
        return Err(Fail::Error(format!(
            "dashboard unavailable: {}",
            dashboard_reason(&info)
        )));
    };
    println!("{url}");
    if server.remote && !parsed.flag("--print") {
        eprintln!(
            "the herdr server runs on another machine: the dashboard listens on its 127.0.0.1 (tunnel the port to open it here)"
        );
    }
    Ok(0)
}

/// Why `coordinator.get` has no dashboard URL.
fn dashboard_reason(info: &CoordinatorGetInfo) -> String {
    if let Some(error) = &info.dashboard_error {
        return error.clone();
    }
    if !info.enabled {
        return "the coordinator is off".into();
    }
    "not listening".into()
}

fn state_name(state: CoordinatorStateInfo) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

/// `running`, `blocked (locked_elsewhere)`, `down (relaunch_cap)`, `off`.
fn state_text(info: &CoordinatorGetInfo) -> String {
    let state = state_name(info.state);
    let reason = match info.state {
        CoordinatorStateInfo::Blocked => info.blocked_reason.as_deref(),
        CoordinatorStateInfo::Down => info.down_reason.as_deref(),
        _ => None,
    };
    match reason {
        Some(reason) => format!("{state} ({reason})"),
        None => state,
    }
}

/// One line: the state and, while it runs, the coordinator's status.
fn state_line(info: &CoordinatorGetInfo) -> String {
    let mut line = format!("state: {}", state_text(info));
    if let Some(status) = &info.coordinator_status {
        line.push_str(&format!(" · {status}"));
    }
    line
}

fn format_info(info: &CoordinatorGetInfo, now: u64) -> String {
    let mut out = String::new();
    let mut head = format!(
        "coordinator: {}{}",
        state_text(info),
        if info.enabled { "" } else { " (disabled)" }
    );
    if let Some(status) = &info.coordinator_status {
        head.push_str(&format!(" · {status}"));
    }
    if let Some(pane) = &info.pane_id {
        head.push_str(&format!(" in {pane}"));
    }
    if let Some(session) = &info.coordinator_session {
        head.push_str(&format!(" · session {}", short(session)));
    }
    head.push_str(&format!(
        " · model {}",
        info.model.as_deref().unwrap_or("default")
    ));
    out.push_str(&head);
    out.push('\n');
    if let Some(notice) = &info.notice {
        out.push_str(&format!("notice: {notice}\n"));
    }
    match &info.dashboard_url {
        Some(url) => out.push_str(&format!("dashboard: {url}\n")),
        None => out.push_str(&format!(
            "dashboard: unavailable ({})\n",
            dashboard_reason(info)
        )),
    }
    match &info.turn {
        Some(turn) => out.push_str(&format!(
            "turn: {} {} for {}s (`herdr coordinator clear-turn` ends it)\n",
            turn.source,
            turn.id,
            now.saturating_sub(turn.started_at)
        )),
        None => out.push_str("turn: none (the next turn is the user's)\n"),
    }
    let wake = &info.wake;
    let mut wakes = format!(
        "wakes: #{} · {}/{} this hour · {}/{} today · {} pending · last {}",
        wake.seq,
        wake.hour,
        wake.cap_hour,
        wake.day,
        wake.cap_day,
        wake.pending,
        wake.last_at.map(clock).unwrap_or_else(|| "never".into()),
    );
    if let Some(next) = wake.next_periodic_at {
        wakes.push_str(&format!(" · periodic {}", clock(next)));
    }
    if wake.capped {
        wakes.push_str(" · capped");
    }
    out.push_str(&wakes);
    out.push('\n');
    if info.relaunches_hour > 0 {
        out.push_str(&format!(
            "relaunches: {} in the last hour\n",
            info.relaunches_hour
        ));
    }
    if let Some(board) = &info.board {
        let mut line = format!(
            "board: {} suggestion{} ({} unread)",
            board.suggestion_count,
            if board.suggestion_count == 1 { "" } else { "s" },
            info.unread_suggestions.max(board.unread)
        );
        if let Some(summary) = board.summary.as_deref().filter(|s| !s.is_empty()) {
            line.push_str(&format!(" — {}", coordinator::one_line(summary, 160)));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!(
        "notifications: {}\n",
        if info.notify { "on" } else { "off" }
    ));
    for agent in &info.managed {
        let opt = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".into());
        let role_project = match (agent.role.as_deref(), agent.project.as_deref()) {
            (None, None) => "-".to_string(),
            (role, project) => format!("{}/{}", role.unwrap_or("-"), project.unwrap_or("-")),
        };
        let since = agent
            .last_change_at
            .filter(|at| *at > 0)
            .map(|at| format!("  {}", age(now.saturating_sub(at))))
            .unwrap_or_default();
        let mut line = format!(
            "  {:<8} {:<16} {:<7} {:<18} {:<9}{}",
            opt(&agent.pane_id),
            agent.name,
            opt(&agent.agent),
            role_project,
            agent.status.as_deref().unwrap_or("offline"),
            since,
        );
        if let Some(note) = agent.note.as_deref().filter(|n| !n.is_empty()) {
            line.push_str(&format!("  \"{}\"", coordinator::one_line(note, 80)));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&format!("{} managed\n", info.managed.len()));
    if !info.coordinator_dir.is_empty() {
        out.push_str(&format!("dir: {}\n", info.coordinator_dir));
    }
    out
}

fn short(session: &str) -> String {
    session.chars().take(8).collect()
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

    /// The coordinator directory: `--dir`, else the default; always absolute.
    fn dir(&self) -> Result<PathBuf, Fail> {
        let dir = self
            .value("--dir")
            .map(PathBuf::from)
            .unwrap_or_else(coordinator::coordinator_dir);
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

/// Parse a file verb's `args` against the value options and bare flags it
/// accepts (`--dir` is accepted by every file verb); `--flag=value` works for
/// value options.
fn parse(args: &[String], value_options: &[&str], flag_options: &[&str]) -> Result<Parsed, Fail> {
    let mut values: Vec<&str> = vec!["--dir"];
    values.extend_from_slice(value_options);
    parse_with(args, &values, flag_options)
}

/// Parse a server verb's `args`: the server owns the coordinator directory,
/// so `--dir` is not an option here.
fn parse_server(
    args: &[String],
    value_options: &[&str],
    flag_options: &[&str],
) -> Result<Parsed, Fail> {
    parse_with(args, value_options, flag_options)
}

fn parse_with(args: &[String], values: &[&str], flag_options: &[&str]) -> Result<Parsed, Fail> {
    let args = super::expand_equals_args(args, values);
    let mut parsed = Parsed::default();
    let mut iter = args.into_iter();
    let mut options_ended = false;
    while let Some(arg) = iter.next() {
        if !options_ended && (arg == "--help" || arg == "-h") {
            return Err(Fail::Help);
        }
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

// ----- file verbs ---------------------------------------------------------

fn mcp(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--port"], &[])?;
    parsed.at_most_positionals(0)?;
    let env_pane = super::target::caller_pane_id();
    let verdict = caller_verdict(&SocketApi, env_pane.as_deref());
    let opts = coordinator::mcp::McpOpts {
        dir: parsed.dir()?,
        env_pane,
        verdict,
        port: parsed.port()?,
    };
    Ok(coordinator::mcp::run(SocketApi, opts)?)
}

fn seed(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &[], &[])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    coordinator::seed(&dir)?;
    println!("coordinator directory: {}", dir.display());
    for path in [
        coordinator::instructions_path(&dir),
        coordinator::dashboard_dir(&dir).join("index.html"),
        coordinator::dashboard_dir(&dir).join("template.html"),
        coordinator::board_path(&dir),
        coordinator::memory_index_path(&dir),
        coordinator::registry_path(&dir),
        coordinator::messages_path(&dir),
        coordinator::live_path(&dir),
    ] {
        println!("  {}", path.display());
    }
    Ok(0)
}

fn clear_turn(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &[], &[])?;
    parsed.at_most_positionals(0)?;
    let dir = parsed.dir()?;
    let now = coordinator::now_unix();
    refuse_in_herdr_turn(&SocketApi, &dir, caller_pane().as_deref(), now)?;
    match coordinator::turn::read_live(&dir, now) {
        Some(turn) => {
            coordinator::turn::clear_if(&dir, &turn);
            println!(
                "cleared turn {} {} ({}s old); the coordinator's next turn is the user's",
                turn.source,
                turn.id,
                now.saturating_sub(turn.started_unix)
            );
        }
        None => {
            coordinator::turn::clear(&dir);
            println!("no live turn");
        }
    }
    Ok(0)
}

/// The pane this command runs in (`HERDR_PANE_ID`), if any.
fn caller_pane() -> Option<String> {
    super::target::caller_pane_id()
}

/// Refuse a file verb run by the coordinator agent itself while herdr
/// started its turn: the CLI must not get around the non-user-turn guard of
/// the MCP write tools. Anyone else (the user in another pane, a script
/// outside herdr) is not affected. The server verbs pass the pane as
/// `caller_pane` and the server applies the same rule.
fn refuse_in_herdr_turn(
    api: &impl Api,
    dir: &Path,
    env_pane: Option<&str>,
    now: u64,
) -> Result<(), Fail> {
    let Some(env_pane) = env_pane.filter(|pane| !pane.is_empty()) else {
        return Ok(());
    };
    let Some(turn) = coordinator::turn::read_live(dir, now) else {
        return Ok(());
    };
    let pane = coordinator_api::resolve_caller(api, env_pane)
        .map(|caller| caller.pane_id)
        .unwrap_or_else(|_| env_pane.to_string());
    let registered = Registry::load(dir)
        .coordinator()
        .and_then(|entry| entry.pane_id.clone());
    if pane == turn.coordinator_pane || registered.as_deref() == Some(pane.as_str()) {
        return Err(Fail::Error(format!(
            "this turn was started by herdr ({} {}), not the user; the coordinator cannot change herdr from it. Record a suggestion on the board instead.",
            turn.source, turn.id
        )));
    }
    Ok(())
}

fn manage(args: &[String]) -> Result<i32, Fail> {
    let parsed = parse(args, &["--role", "--project", "--note"], &[])?;
    parsed.at_most_positionals(1)?;
    let Some(target) = parsed.positionals.first() else {
        return Err(Fail::Usage(
            "usage: herdr coordinator manage <pane|name> [--role R] [--project P] [--note N]"
                .into(),
        ));
    };
    let patch = ManagePatch {
        role: parsed.value("--role").map(str::to_string),
        project: parsed.value("--project").map(str::to_string),
        note: parsed.value("--note").map(str::to_string),
    };
    let dir = parsed.dir()?;
    refuse_in_herdr_turn(
        &SocketApi,
        &dir,
        caller_pane().as_deref(),
        coordinator::now_unix(),
    )?;
    let line = manage_target(&SocketApi, &dir, target, &patch).map_err(Fail::Error)?;
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
    let agent = coordinator_api::agent_get(api, target).map_err(|err| err.to_string())?;
    let pane = agent["pane_id"]
        .as_str()
        .ok_or_else(|| format!("{target} has no pane"))?
        .to_string();
    let session = session_of(&agent);
    let kind = agent_kind(&agent);
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

/// The native session id of an `agent.get` entry.
fn session_of(agent: &Value) -> Option<String> {
    agent["agent_session"]["value"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// The agent kind of an `agent.get`/`agent.list` entry.
fn agent_kind(agent: &Value) -> Option<String> {
    agent["agent"]
        .as_str()
        .or_else(|| agent["agent_session"]["agent"].as_str())
        .filter(|kind| !kind.is_empty())
        .map(str::to_string)
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
            "usage: herdr coordinator unmanage <pane|name|session>".into(),
        ));
    };
    let dir = parsed.dir()?;
    refuse_in_herdr_turn(
        &SocketApi,
        &dir,
        caller_pane().as_deref(),
        coordinator::now_unix(),
    )?;
    let line = unmanage_target(&SocketApi, &dir, target).map_err(Fail::Error)?;
    println!("{line}");
    Ok(0)
}

/// Opt an agent out: a live agent by pane or name, or an offline entry by
/// its recorded pane id or session id.
fn unmanage_target(api: &impl Api, dir: &Path, target: &str) -> Result<String, String> {
    let (session, pane, kind) = match coordinator_api::agent_get(api, target) {
        Ok(agent) => (
            session_of(&agent),
            agent["pane_id"].as_str().map(str::to_string),
            agent_kind(&agent),
        ),
        Err(err) if err.code == "server_unavailable" => return Err(err.to_string()),
        Err(_) => (None, None, None),
    };
    let removed = registry::update(dir, |registry| {
        let index = registry
            .find(session.as_deref(), pane.as_deref(), kind.as_deref())
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
        .map(|local| local.assume_utc().unix_timestamp() - coordinator::now_unix() as i64)
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
    use crate::api::schema::coordinator::{
        CoordinatorBoardInfo, CoordinatorManagedInfo, CoordinatorTurnInfo, CoordinatorWakeInfo,
    };
    use std::cell::RefCell;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-cli-coordinator-{name}-{}-{}",
            std::process::id(),
            coordinator::launch::new_uuid()
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
        let relative = parse(&strings(&["--dir", "rel"]), &[], &[]).expect("parses");
        let dir = relative.dir().expect("dir");
        assert!(dir.is_absolute() && dir.ends_with("rel"));
        let port = parse(&strings(&["--port", "x"]), &["--port"], &[]).expect("parses");
        assert!(matches!(port.port(), Err(Fail::Usage(_))));
        let default = parse(&[], &["--port"], &[]).expect("parses");
        assert_eq!(default.port().ok(), Some(DEFAULT_PORT));
        // The server owns the directory: server verbs take no --dir.
        assert!(matches!(
            parse_server(&strings(&["--dir", "/tmp/p"]), &[], &[]),
            Err(Fail::Usage(_))
        ));
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

    /// A fake server: records every `(method, params)` and answers with `info`.
    struct Recorder {
        calls: RefCell<Vec<(String, Value)>>,
        info: Value,
    }

    impl Recorder {
        fn new(info: Value) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                info,
            }
        }

        fn call(&self, method: &str, params: Value) -> Result<Value, Fail> {
            self.calls.borrow_mut().push((method.to_string(), params));
            Ok(json!({ "type": "coordinator_get", "info": self.info.clone() }))
        }

        fn last(&self) -> (String, Value) {
            self.calls.borrow().last().cloned().expect("a call")
        }
    }

    fn server<'a>(call: Call<'a>, remote: bool) -> Server<'a> {
        Server {
            call,
            caller_pane: Some("w1:p2".into()),
            remote,
        }
    }

    #[test]
    fn server_verbs_send_their_method_and_params() {
        let recorder = Recorder::new(json!({
            "enabled": true, "state": "running", "dashboard_url": "http://127.0.0.1:7718/"
        }));
        let call = |method: &str, params: Value| recorder.call(method, params);
        let local = server(&call, false);

        assert_eq!(set_enabled(&local, &[], true).ok(), Some(0));
        assert_eq!(
            recorder.last(),
            (
                "coordinator.set_enabled".into(),
                json!({ "enabled": true, "caller_pane": "w1:p2" })
            )
        );
        set_enabled(&local, &[], false).ok();
        assert_eq!(recorder.last().1["enabled"], false);

        start(&local, &[]).ok();
        assert_eq!(
            recorder.last(),
            (
                "coordinator.start".into(),
                json!({ "resume": true, "caller_pane": "w1:p2" })
            )
        );
        start(&local, &strings(&["--new"])).ok();
        assert_eq!(recorder.last().1["resume"], false);

        wake(&local, &[]).ok();
        assert_eq!(
            recorder.last(),
            ("coordinator.wake".into(), json!({ "caller_pane": "w1:p2" }))
        );

        status(&local, &strings(&["--json"])).ok();
        assert_eq!(recorder.last(), ("coordinator.get".into(), json!({})));

        dashboard(&local, &[]).ok();
        assert_eq!(
            recorder.last(),
            ("coordinator.open_dashboard".into(), json!({ "open": true }))
        );
        dashboard(&local, &strings(&["--print"])).ok();
        assert_eq!(recorder.last().1, json!({ "open": false }));

        // A remote server would open the page on its own screen.
        let remote = server(&call, true);
        dashboard(&remote, &[]).ok();
        assert_eq!(recorder.last().1, json!({ "open": false }));

        // The user outside herdr: no caller pane at all.
        let outside = Server {
            call: &call,
            caller_pane: None,
            remote: false,
        };
        wake(&outside, &[]).ok();
        assert_eq!(recorder.last().1, json!({}));

        assert!(matches!(
            start(&local, &strings(&["--resume"])),
            Err(Fail::Usage(_))
        ));
        assert!(matches!(
            wake(&local, &strings(&["now"])),
            Err(Fail::Usage(_))
        ));
        assert!(recorder
            .calls
            .borrow()
            .iter()
            .all(|(method, _)| method::ALL.contains(&method.as_str())));
    }

    #[test]
    fn dashboard_without_a_url_says_why() {
        let recorder = Recorder::new(json!({
            "enabled": true, "state": "running", "dashboard_error": "port 7718 in use"
        }));
        let call = |method: &str, params: Value| recorder.call(method, params);
        assert!(matches!(
            dashboard(&server(&call, false), &[]),
            Err(Fail::Error(m)) if m.contains("port 7718 in use")
        ));
        let off = CoordinatorGetInfo::default();
        assert_eq!(dashboard_reason(&off), "the coordinator is off");
    }

    #[test]
    fn replies_without_info_and_unknown_methods_are_errors() {
        let empty = |_: &str, _: Value| -> Result<Value, Fail> { Ok(json!({ "type": "ok" })) };
        assert!(matches!(
            server(&empty, false).request(method::GET, &json!({})),
            Err(Fail::Error(m)) if m.contains("no coordinator info")
        ));
        let refused = |_: &str, _: Value| -> Result<Value, Fail> {
            Err(ApiError::new("in_coordinator_turn", "not in a herdr turn").into())
        };
        assert!(matches!(
            wake(&server(&refused, false), &[]),
            Err(Fail::Error(m)) if m.contains("in_coordinator_turn")
        ));
        assert!(matches!(
            build_request("coordinator.no_such_method", json!({})),
            Err(Fail::Error(m)) if m.contains("does not know coordinator.no_such_method")
        ));
        let ping = build_request("ping", json!({})).expect("ping builds");
        assert_eq!(ping.id, "coordinator:ping");
    }

    #[test]
    fn status_text_summarises_the_read_model() {
        let info = CoordinatorGetInfo {
            enabled: true,
            state: CoordinatorStateInfo::Running,
            pane_id: Some("w1:p2".into()),
            coordinator_status: Some("idle".into()),
            coordinator_session: Some("0123456789abcdef".into()),
            dashboard_url: Some("http://127.0.0.1:7718/".into()),
            turn: Some(CoordinatorTurnInfo {
                source: "wake".into(),
                id: "4".into(),
                started_at: 70,
            }),
            wake: CoordinatorWakeInfo {
                seq: 7,
                hour: 2,
                day: 10,
                cap_hour: 12,
                cap_day: 80,
                capped: true,
                pending: 3,
                ..CoordinatorWakeInfo::default()
            },
            relaunches_hour: 1,
            managed: vec![CoordinatorManagedInfo {
                name: "calendar-fix".into(),
                role: Some("fixer".into()),
                project: Some("search-it".into()),
                pane_id: Some("w2:p4".into()),
                agent: Some("codex".into()),
                status: Some("working".into()),
                last_change_at: Some(40),
                ..CoordinatorManagedInfo::default()
            }],
            board: Some(CoordinatorBoardInfo {
                summary: Some("calendar sync is half done".into()),
                suggestion_count: 2,
                unread: 1,
                ..CoordinatorBoardInfo::default()
            }),
            unread_suggestions: 1,
            coordinator_dir: "/cfg/coordinator".into(),
            ..CoordinatorGetInfo::default()
        };
        let text = format_info(&info, 100);
        for expected in [
            "coordinator: running · idle in w1:p2 · session 01234567 · model default",
            "dashboard: http://127.0.0.1:7718/",
            "turn: wake 4 for 30s",
            "wakes: #7 · 2/12 this hour · 10/80 today · 3 pending · last never · capped",
            "relaunches: 1 in the last hour",
            "board: 2 suggestions (1 unread) — calendar sync is half done",
            "notifications: on",
            "w2:p4    calendar-fix     codex   fixer/search-it    working    1m",
            "1 managed",
            "dir: /cfg/coordinator",
        ] {
            assert!(text.contains(expected), "{expected:?} in\n{text}");
        }
        let blocked = CoordinatorGetInfo {
            enabled: true,
            state: CoordinatorStateInfo::Blocked,
            blocked_reason: Some("locked_elsewhere".into()),
            dashboard_error: Some("port 7718 in use".into()),
            notify: false,
            ..CoordinatorGetInfo::default()
        };
        let text = format_info(&blocked, 100);
        assert!(
            text.contains("coordinator: blocked (locked_elsewhere)"),
            "{text}"
        );
        assert!(
            text.contains("dashboard: unavailable (port 7718 in use)"),
            "{text}"
        );
        assert!(text.contains("turn: none"), "{text}");
        assert!(text.contains("notifications: off"), "{text}");
        let off = format_info(&CoordinatorGetInfo::default(), 100);
        assert!(off.starts_with("coordinator: off (disabled)"), "{off}");
        assert_eq!(
            state_name(CoordinatorStateInfo::WaitingForLock),
            "waiting_for_lock"
        );
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
            role: Some(coordinator::COORDINATOR_ROLE.into()),
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
    fn write_verbs_refuse_the_coordinator_in_a_herdr_turn_only() {
        let dir = test_dir("guard");
        registry::update(&dir, |registry| {
            registry.manage(
                Some("s-c"),
                Some("w2:p3"),
                Some("claude"),
                &ManagePatch {
                    role: Some(coordinator::COORDINATOR_ROLE.into()),
                    ..ManagePatch::default()
                },
            )
        })
        .unwrap();
        // actor_api resolves any pane to w2:p3, the coordinator.
        let api = actor_api(Some(40));
        let guard = |pane: Option<&str>| refuse_in_herdr_turn(&api, &dir, pane, 100);
        assert!(guard(Some("w2:p3")).is_ok(), "no turn: the user's");
        coordinator::turn::write(
            &dir,
            &coordinator::turn::Turn {
                source: "wake".into(),
                id: "4".into(),
                started_unix: 90,
                coordinator_pane: "w2:p3".into(),
                seen_working: true,
            },
        )
        .unwrap();
        assert!(matches!(guard(Some("w2:p3")), Err(Fail::Error(m)) if m.contains("wake 4")));
        assert!(guard(None).is_ok(), "outside herdr: the user");
        let other = |_: Method| -> Result<Value, ApiError> {
            Ok(json!({ "type": "browser_actor", "actor": {
                "kind": "pane", "pane_id": "w5:p1", "tab_id": "w5:t1", "workspace_id": "w5",
                "shell_pid": 1 } }))
        };
        assert!(
            refuse_in_herdr_turn(&other, &dir, Some("w5:p1"), 100).is_ok(),
            "the user in another pane"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn help_is_available_on_every_verb() {
        assert!(matches!(
            parse(&strings(&["--help"]), &[], &[]),
            Err(Fail::Help)
        ));
        assert!(matches!(
            parse(&strings(&["rev", "-h"]), &["--role"], &[]),
            Err(Fail::Help)
        ));
        assert!(parse(&strings(&["--", "-h"]), &[], &[]).is_ok(), "after --");
        let unreachable = |_: &str, _: Value| -> Result<Value, Fail> {
            Err(Fail::Error("no server in this test".into()))
        };
        let server = server(&unreachable, false);
        let help = strings(&["--help"]);
        assert!(matches!(start(&server, &help), Err(Fail::Help)));
        assert!(matches!(wake(&server, &help), Err(Fail::Help)));
        assert!(matches!(status(&server, &help), Err(Fail::Help)));
        assert!(matches!(dashboard(&server, &help), Err(Fail::Help)));
        assert!(matches!(set_enabled(&server, &help, true), Err(Fail::Help)));
        for verb in [clear_turn, seed, manage, unmanage, messages_cmd] {
            assert!(matches!(verb(&help), Err(Fail::Help)));
        }
        assert!(HELP.contains("HERDR_COORDINATOR_WAKE_DEBOUNCE_S") && HELP.contains("clear-turn"));
        assert!(!HELP.contains("coordinator run") && !HELP.contains("coordinator serve"));
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
