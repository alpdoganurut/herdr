use std::time::{Duration, Instant};

use crate::api::schema::{
    AgentActivateParams, AgentPromptParams, AgentReadParams, AgentRenameParams, AgentRestartParams,
    AgentSendKeysParams, AgentStartParams, AgentSuspendParams, AgentTarget, AgentWaitParams,
    EmptyParams, ErrorBody, ErrorResponse, Method, PaneProcessInfoParams, PaneTarget, Request,
};

const AGENT_START_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PANE_SHELL_READINESS_RETRY_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) mod bindings;
#[cfg(test)]
mod bindings_tests;

pub(crate) fn command() -> clap::Command {
    bindings::command()
}

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    use super::projection::ParseError;
    use bindings::Operation;

    // Fork: these subcommands parse their own arguments (their help comes
    // from spec.rs, which adds them to the upstream `agent` command).
    match args.first().map(String::as_str) {
        Some("suspend") => return agent_suspend(&args[1..]),
        Some("activate") => return agent_activate(&args[1..]),
        Some("restart") => return agent_restart(&args[1..]),
        Some("handoff") => return agent_handoff(&args[1..]),
        Some("transcripts") => return agent_transcripts(&args[1..]),
        Some("wrap") => return super::agent_wrap::run(&args[1..]),
        Some("notify") => return super::agent_notify::run(&args[1..]),
        _ => {}
    }

    // `explain` and `wait` historically treat a bare `help` operand after the target as help.
    let help_args;
    let args = match args.first().map(String::as_str) {
        Some(leaf @ ("explain" | "wait")) => {
            let operands = super::projection::operands(&command(), args);
            let skip = usize::from(leaf == "wait");
            if operands.iter().skip(skip).any(|operand| *operand == "help") {
                help_args = [leaf.to_owned(), "--help".to_owned()];
                &help_args[..]
            } else {
                args
            }
        }
        _ => args,
    };
    // Fork: the spec command (upstream's plus the fork's subcommands), so
    // `herdr agent help` lists them too.
    let operation = match super::projection::parse(
        super::spec::agent_command().bin_name("herdr agent"),
        args,
        bindings::from_matches,
    ) {
        Ok(operation) => operation,
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
    };
    match operation {
        Operation::List => agent_list(),
        Operation::Get(params) => agent_get(params),
        Operation::Focus(params) => agent_focus(params),
        Operation::Read(params) => agent_read(params),
        Operation::SendKeys(params) => agent_send_keys(params),
        Operation::Rename(params) => agent_rename(params),
        Operation::Wait(params) => agent_wait(params),
        Operation::Prompt(params) => agent_prompt(params),
        Operation::Start(params) => agent_start(params),
        Operation::Attach(params) => agent_attach(params),
        Operation::Explain(params) => agent_explain(params),
    }
}

/// List the native transcript backups kept under the session directory.
/// Reads the store directly; no server is needed.
fn agent_transcripts(args: &[String]) -> std::io::Result<i32> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent transcripts [--json]");
                return Ok(0);
            }
            _ => {
                eprintln!("usage: herdr agent transcripts [--json]");
                return Ok(2);
            }
        }
    }
    let store_dir = crate::persist::agent_transcripts::store_dir();
    let entries = crate::persist::agent_transcripts::list_backups(&store_dir)?;
    if json {
        let value = serde_json::json!({
            "store_dir": store_dir,
            "transcripts": entries,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }
    println!("{}", format_agent_transcripts(&store_dir, &entries));
    Ok(0)
}

fn format_agent_transcripts(
    store_dir: &std::path::Path,
    entries: &[crate::persist::agent_transcripts::TranscriptBackupEntry],
) -> String {
    let mut out = format!("store: {}\n", store_dir.display());
    if entries.is_empty() {
        out.push_str("no agent transcript backups");
        return out;
    }
    let id_width = entries
        .iter()
        .map(|entry| entry.session_id.len())
        .max()
        .unwrap_or(0)
        .max("SESSION".len());
    let agent_width = entries
        .iter()
        .map(|entry| entry.agent.len())
        .max()
        .unwrap_or(0)
        .max("AGENT".len());
    out.push_str(&format!(
        "{:agent_width$}  {:id_width$}  {:>10}  {:25}  NATIVE\n",
        "AGENT", "SESSION", "BYTES", "BACKED_UP_AT"
    ));
    for entry in entries {
        out.push_str(&format!(
            "{:agent_width$}  {:id_width$}  {:>10}  {:25}  {}\n",
            entry.agent,
            entry.session_id,
            entry.bytes,
            entry.backed_up_at,
            if entry.native_present {
                "present"
            } else {
                "missing"
            }
        ));
    }
    out.pop();
    out
}

fn agent_explain(params: bindings::Explain) -> std::io::Result<i32> {
    let bindings::Explain {
        file,
        agent,
        json,
        verbose,
        target,
    } = params;

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("herdr agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                let response = ErrorResponse {
                    id: "cli:agent:explain".into(),
                    error: ErrorBody {
                        code: "agent_explain_file_read_failed".into(),
                        message: format!("failed to read agent explain file {path}: {err}"),
                    },
                };
                let response = serde_json::to_string(&response).map_err(std::io::Error::other)?;
                eprintln!("{response}");
                return Ok(1);
            }
        };
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: herdr agent explain <target> [--json]");
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: target.to_owned(),
            }),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

/// Fork (OSC 7501): the `program_status` explain object as text lines.
/// Messages are untrusted: truncated and printed quoted.
fn print_program_status_text(status: &serde_json::Value, verbose: bool) {
    if !status.is_object() {
        return;
    }
    if !status["enabled"].as_bool().unwrap_or(true) {
        println!("program_status: off");
        return;
    }
    let Some(root) = status["root"].as_object() else {
        let probe_answered = status["probe_answered"].as_bool().unwrap_or(false);
        let rejected = status["rejected_reports"].as_u64().unwrap_or(0);
        // Most agents never speak OSC 7501: say nothing unless asked.
        if probe_answered || rejected > 0 || verbose {
            println!(
                "program_status: none (probe answered: {}, rejected reports: {rejected})",
                if probe_answered { "yes" } else { "no" }
            );
        }
        return;
    };
    let children = status["children"].as_array().map_or(0, Vec::len);
    let agrees = match status["agrees_with_screen"].as_bool() {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    };
    println!(
        "program_status: {}{} agrees={agrees} children={children}",
        program_status_state_text(root),
        program_status_msg_text(root),
    );
    if !verbose {
        return;
    }
    for child in status["children"].as_array().into_iter().flatten() {
        let Some(child) = child.as_object() else {
            continue;
        };
        println!(
            "  child {} {}{} age={:.1}s",
            child.get("id").and_then(|id| id.as_str()).unwrap_or("-"),
            program_status_state_text(child),
            program_status_msg_text(child),
            child
                .get("age_ms")
                .and_then(|age| age.as_u64())
                .unwrap_or(0) as f64
                / 1000.0,
        );
    }
    if let Some(reject) = status["last_reject"].as_str() {
        println!(
            "program_status_rejected: {} (last: {reject})",
            status["rejected_reports"].as_u64().unwrap_or(0)
        );
    }
}

fn program_status_state_text(record: &serde_json::Map<String, serde_json::Value>) -> String {
    let state = record
        .get("state")
        .and_then(|state| state.as_str())
        .unwrap_or("-");
    match record.get("kind").and_then(|kind| kind.as_str()) {
        Some(kind) => format!("{state}/{kind}"),
        None => state.to_string(),
    }
}

fn program_status_msg_text(record: &serde_json::Map<String, serde_json::Value>) -> String {
    let text = record
        .get("msg")
        .and_then(|msg| msg.as_str())
        .or_else(|| record.get("title").and_then(|title| title.as_str()));
    match text {
        Some(text) => {
            let truncated: String = text.chars().take(80).collect();
            format!(" {truncated:?}")
        }
        None => String::new(),
    }
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }
    // Fork (OSC 7501).
    print_program_status_text(&explain["program_status"], verbose);

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(params: AgentStartParams) -> std::io::Result<i32> {
    let AgentStartParams {
        name,
        kind,
        pane_id,
        args: agent_args,
        timeout_ms,
    } = params;
    let Some(expected_kind) = crate::detect::parse_agent_label(&kind) else {
        eprintln!("unsupported interactive agent kind: {kind}");
        return Ok(2);
    };
    let expected_kind = crate::detect::agent_label(expected_kind).to_string();
    // Fork (agents v2): starting an agent types into the target shell; an
    // agent may do that only where it may type (its own or a teammate's
    // shell), like `pane run`.
    match super::agent_route::route()? {
        Err(code) => return Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => {
            if let Some(code) = super::agent_route::check(
                &pane,
                crate::api::schema::agents_model::AgentsCheckAction::ShellInput,
                Some(pane_id.clone()),
            )? {
                return Ok(code);
            }
        }
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {}
    }
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    let retryable_timeout = timeout > crate::app::AGENT_START_SETTLE_DELAY
        && timeout <= crate::app::MAX_AGENT_START_TIMEOUT;
    let pinned_terminal_id = pane_terminal_id(&pane_id)?;
    let mut retry_deadline = None;
    let mut previous_busy_response = None;
    let mut response = loop {
        if let Some(previous_busy_response) = previous_busy_response.as_ref() {
            let retry_expired = retry_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if retry_expired
                || pane_terminal_id(&pane_id)? != pinned_terminal_id
                || !pane_shell_is_initializing(&pane_id)?
            {
                return super::print_response(previous_busy_response);
            }
        }

        let response = super::send_request(&Request {
            id: "cli:agent:start".into(),
            method: Method::AgentStart(AgentStartParams {
                name: name.clone(),
                kind: kind.clone(),
                pane_id: pane_id.clone(),
                args: agent_args.clone(),
                timeout_ms,
            }),
        })?;
        if response.get("error").is_none() {
            break response;
        }
        if response["error"]["code"].as_str() != Some("agent_pane_busy")
            || !retryable_timeout
            || pinned_terminal_id.is_none()
            || pane_terminal_id(&pane_id)? != pinned_terminal_id
            || !pane_shell_is_initializing(&pane_id)?
        {
            return super::print_response(&response);
        }

        let deadline = *retry_deadline
            .get_or_insert_with(|| Instant::now() + PANE_SHELL_READINESS_RETRY_TIMEOUT);
        previous_busy_response = Some(response);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if let Some(previous_busy_response) = previous_busy_response.as_ref() {
                return super::print_response(previous_busy_response);
            }
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL.min(remaining));
    };

    let Some(expected_terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        return super::print_response(&cli_agent_error(
            "cli:agent:start",
            "agent_start_failed",
            "agent start response did not include terminal_id",
        ));
    };
    if pinned_terminal_id
        .as_deref()
        .is_some_and(|pinned| pinned != expected_terminal_id)
    {
        return super::print_response(&agent_name_lost_error("cli:agent:start", &name));
    }
    let waited = wait_for_named_agent(
        &name,
        &pane_id,
        timeout,
        &expected_kind,
        expected_terminal_id,
    );
    match waited {
        Ok(Ok(agent)) => {
            response["result"]["agent"] = agent;
            super::print_response(&response)
        }
        Ok(Err(error)) => super::print_response(&error),
        Err(err) => {
            print_agent_transport_error(err, "cli:agent:start", "agent_start_transport_failed")
        }
    }
}

fn agent_list() -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_get(params: AgentTarget) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(params),
    })?)
}

fn agent_focus(params: AgentTarget) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(params),
    })?)
}

fn agent_suspend(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), 1) = (args.first(), args.len()) else {
        eprintln!("usage: herdr agent suspend <target>");
        return Ok(2);
    };

    // Fork (agents v2): an agent goes through agents.suspend (checked and
    // recorded as that agent).
    match super::agent_route::route()? {
        Err(code) => Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => super::agent_route::call(
            "cli:agents.suspend",
            Method::AgentsSuspend(crate::api::schema::agents_model::AgentsLifecycleParams {
                caller_pane: pane,
                target: target.clone(),
            }),
        ),
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {
            super::print_response(&super::send_request(&Request {
                id: "cli:agent:suspend".into(),
                method: Method::AgentSuspend(AgentSuspendParams {
                    target: target.clone(),
                }),
            })?)
        }
    }
}

fn agent_activate(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), 1) = (args.first(), args.len()) else {
        eprintln!("usage: herdr agent activate <target>");
        return Ok(2);
    };

    // Fork (agents v2): an agent goes through agents.activate (checked and
    // recorded as that agent).
    match super::agent_route::route()? {
        Err(code) => Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => super::agent_route::call(
            "cli:agents.activate",
            Method::AgentsActivate(crate::api::schema::agents_model::AgentsLifecycleParams {
                caller_pane: pane,
                target: target.clone(),
            }),
        ),
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {
            super::print_response(&super::send_request(&Request {
                id: "cli:agent:activate".into(),
                method: Method::AgentActivate(AgentActivateParams {
                    target: target.clone(),
                }),
            })?)
        }
    }
}

fn agent_restart(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), 1) = (args.first(), args.len()) else {
        eprintln!("usage: herdr agent restart <target>");
        return Ok(2);
    };

    // Fork (agents v2): an agent goes through agents.restart (checked and
    // recorded as that agent).
    match super::agent_route::route()? {
        Err(code) => Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => super::agent_route::call(
            "cli:agents.restart",
            Method::AgentsRestart(crate::api::schema::agents_model::AgentsLifecycleParams {
                caller_pane: pane,
                target: target.clone(),
            }),
        ),
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {
            super::print_response(&super::send_request(&Request {
                id: "cli:agent:restart".into(),
                method: Method::AgentRestart(AgentRestartParams {
                    target: target.clone(),
                }),
            })?)
        }
    }
}

/// Fork: handoff, then compact (`agents.handoff`). An agent calls as itself
/// (its own agent freely, a teammate in its user's turn); the user without
/// a caller.
fn agent_handoff(args: &[String]) -> std::io::Result<i32> {
    let (Some(target), 1) = (args.first(), args.len()) else {
        eprintln!("usage: herdr agent handoff <target>");
        return Ok(2);
    };
    let params = |caller_pane| crate::api::schema::agents_model::AgentsHandoffParams {
        caller_pane,
        target: target.clone(),
    };
    match super::agent_route::route()? {
        Err(code) => Ok(code),
        Ok(super::agent_route::Route::Agent(pane)) => super::agent_route::call(
            "cli:agents.handoff",
            Method::AgentsHandoff(params(Some(pane))),
        ),
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {
            super::print_response(&super::send_request(&Request {
                id: "cli:agents.handoff".into(),
                method: Method::AgentsHandoff(params(None)),
            })?)
        }
    }
}

fn agent_attach(params: bindings::Attach) -> std::io::Result<i32> {
    let bindings::Attach { target, takeover } = params;

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(params: AgentWaitParams) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:wait".into(),
        method: Method::AgentWait(params),
    })?)
}

fn wait_for_named_agent(
    name: &str,
    fallback_pane_id: &str,
    timeout: Duration,
    expected_kind: &str,
    expected_terminal_id: &str,
) -> std::io::Result<Result<serde_json::Value, serde_json::Value>> {
    let deadline = Instant::now().checked_add(timeout);
    let mut first_poll = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Let the server reconcile its matching startup deadline before
            // returning so the pending name is immediately reusable.
            let _ = resolve_agent_target_unchecked(name, "cli:agent:start:timeout");
            return Ok(Err(agent_wait_timeout()));
        }
        let poll_id = "cli:agent:start";
        let mut response = if first_poll {
            first_poll = false;
            resolve_agent_target(name, poll_id)?
        } else {
            resolve_agent_target_unchecked(name, poll_id)?
        };
        if response.get("error").is_some() {
            response = resolve_agent_target_unchecked(fallback_pane_id, poll_id)?;
            if response.get("error").is_some() {
                std::thread::sleep(AGENT_START_POLL_INTERVAL);
                continue;
            }
        }
        let agent = &response["result"]["agent"];
        let outcome = if agent["terminal_id"].as_str() != Some(expected_terminal_id) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else if let Some(actual) = agent["agent"]
            .as_str()
            .filter(|actual| *actual != expected_kind)
        {
            Some(Err(cli_agent_error(
                "cli:agent:start",
                "agent_kind_mismatch",
                format!("expected {expected_kind}, detected {actual}"),
            )))
        } else if agent["name"].as_str() != Some(name) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else {
            match agent["agent_status"].as_str() {
                Some("blocked") => Some(Err(cli_agent_error(
                    "cli:agent:start",
                    "agent_not_ready",
                    format!("agent {name} is blocked during startup and is not ready for prompts"),
                ))),
                Some("unknown")
                    if expected_kind == "codex"
                        && agent["interactive_ready"].as_bool() == Some(true) =>
                {
                    Some(Ok(agent.clone()))
                }
                Some("working" | "unknown") => None,
                Some("idle" | "done") if agent["interactive_ready"].as_bool() == Some(true) => {
                    Some(Ok(agent.clone()))
                }
                Some("idle" | "done") if !agent["launch_pending"].as_bool().unwrap_or(false) => {
                    Some(Err(cli_agent_error(
                        "cli:agent:start",
                        "agent_start_failed",
                        "agent process exited before becoming interactive",
                    )))
                }
                _ => None,
            }
        };
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        std::thread::sleep(AGENT_START_POLL_INTERVAL);
    }
}

fn pane_terminal_id(pane_id: &str) -> std::io::Result<Option<String>> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:pane".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_owned(),
        }),
    })?;
    Ok(response["result"]["pane"]["terminal_id"]
        .as_str()
        .map(str::to_owned))
}

fn pane_shell_is_initializing(pane_id: &str) -> std::io::Result<bool> {
    let response = super::send_request(&Request {
        id: "cli:agent:start:process_info".into(),
        method: Method::PaneProcessInfo(PaneProcessInfoParams {
            pane_id: Some(pane_id.to_owned()),
        }),
    })?;
    Ok(process_info_shows_shell_initialization(
        &response["result"]["process_info"],
    ))
}

#[cfg(unix)]
fn process_info_shows_shell_initialization(process_info: &serde_json::Value) -> bool {
    let Some(shell_pid) = process_info["shell_pid"].as_u64() else {
        return false;
    };
    if process_info["foreground_process_group_id"].as_u64() != Some(shell_pid) {
        return false;
    }
    process_info["foreground_processes"]
        .as_array()
        .is_some_and(|processes| {
            processes.iter().any(|process| {
                process["pid"].as_u64() == Some(shell_pid)
                    && (process["name"]
                        .as_str()
                        .is_some_and(crate::platform::is_pane_shell_process_name)
                        || process["argv"]
                            .as_array()
                            .and_then(|argv| argv.first())
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(crate::platform::is_pane_shell_process_name))
            })
        })
}

// Windows exposes no foreground process group, so shell initialization is not
// observable and a busy `agent.start` is not retried there.
#[cfg(not(unix))]
fn process_info_shows_shell_initialization(_process_info: &serde_json::Value) -> bool {
    false
}

fn agent_name_lost_error(request_id: &str, expected_name: &str) -> serde_json::Value {
    cli_agent_error(
        request_id,
        "agent_name_not_found",
        format!("named agent {expected_name} no longer owns the target terminal"),
    )
}

fn print_agent_transport_error(
    err: std::io::Error,
    request_id: &str,
    code: &str,
) -> std::io::Result<i32> {
    if super::protocol_mismatch_was_reported(&err) {
        return Ok(1);
    }
    // A dead-server marker reaches here from `send_request` in the agent
    // startup path; surface its deferred response exactly once instead of
    // printing a second, generic transport-error line.
    if let Some(response) = super::server_not_running_reported_response(&err) {
        let value = serde_json::to_value(response).map_err(std::io::Error::other)?;
        return super::print_response(&value);
    }
    super::print_response(&cli_agent_error(request_id, code, err.to_string()))
}

fn agent_wait_timeout() -> serde_json::Value {
    cli_agent_error(
        "cli:agent:start",
        "timeout",
        "timed out waiting for agent startup",
    )
}

fn cli_agent_error(id: &str, code: &str, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&agent_get_request(target, request_id))
}

fn resolve_agent_target_unchecked(
    target: &str,
    request_id: &str,
) -> std::io::Result<serde_json::Value> {
    super::send_request_unchecked(&agent_get_request(target, request_id))
}

fn agent_get_request(target: &str, request_id: &str) -> Request {
    Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    }
}

fn agent_rename(params: AgentRenameParams) -> std::io::Result<i32> {
    // Fork (agents v2): an agent renames only itself or a teammate (a soft
    // edit, checked and logged as that agent).
    super::agent_route::checked(
        crate::api::schema::agents_model::AgentsCheckAction::Cosmetic,
        Some(params.target.clone()),
        || {
            super::print_response(&super::send_request(&Request {
                id: "cli:agent:rename".into(),
                method: Method::AgentRename(params),
            })?)
        },
    )
}

fn agent_prompt(params: AgentPromptParams) -> std::io::Result<i32> {
    // Fork (agents v2): a prompt from an agent is a message from it (the
    // envelope, the limits, idle targets only).
    match super::agent_route::route()? {
        Err(code) => return Ok(code),
        Ok(super::agent_route::Route::Agent(_)) if params.wait.is_some() => {
            // A message is answered by a message; waiting on the target's
            // status would return before a queued message is even typed in.
            return Ok(super::agent_route::refuse(
                "invalid_params",
                "from an agent, agent prompt sends a message and cannot --wait; send it without --wait/--until/--timeout, then wait for the reply with agents_wait_for_message",
            ));
        }
        Ok(super::agent_route::Route::Agent(pane)) => {
            return super::agent_route::call(
                "cli:agents.send_message",
                Method::AgentsSendMessage(
                    crate::api::schema::agents_model::AgentsSendMessageParams {
                        caller_pane: pane,
                        to: params.target.clone(),
                        text: params.text.clone(),
                        reply_to: None,
                        urgent: false,
                        quiet: false,
                    },
                ),
            )
        }
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {}
    }
    let response = super::send_request(&Request {
        id: "cli:agent:prompt".into(),
        method: Method::AgentPrompt(params),
    })?;
    super::print_response(&response)
}

fn agent_send_keys(params: AgentSendKeysParams) -> std::io::Result<i32> {
    // Fork (agents v2): keys into another agent are refused for agents
    // (messages go through agents_send_message).
    super::agent_route::checked(
        crate::api::schema::agents_model::AgentsCheckAction::ShellInput,
        Some(params.target.clone()),
        || {
            super::print_response(&super::send_request(&Request {
                id: "cli:agent:send-keys".into(),
                method: Method::AgentSendKeys(params),
            })?)
        },
    )
}

fn agent_read(params: AgentReadParams) -> std::io::Result<i32> {
    let response = super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(params),
    })?;
    super::print_read_response(&response)
}
