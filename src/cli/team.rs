//! `herdr team` (fork): teams from the command line (`team.*`). The command
//! line is the user: no caller pane is sent, except that `get` without a
//! group asks about the group this pane is in. So the changing verbs refuse
//! to run from a pane whose agent is running (an agent's shell tool, the
//! coordinator's included): agents go through `agents_team`, whose guards
//! the CLI must not get around. `herdr team hook` is the Claude per-turn
//! hook the agent wrap installs; it always exits 0.

use crate::api::schema::{
    EmptyParams, Method, Request, TeamGetParams, TeamJoinParams, TeamMakeParams, TeamPaneParams,
    TeamSetPurposeParams, TeamSetRoleParams, TeamWorkspaceParams,
};
use crate::coordinator::api::{self as coordinator_api, Api};

pub(crate) const USAGE: &str = "\
usage: herdr team <command> [--json]

  list                          every team
  get [<group>]                 a group's team (default: this pane's group)
  make <group> [--purpose T]    mark a group as a team; its agents join
  disband <group>               unmark it (agents keep their names)
  purpose <group> <text>        set the purpose (empty text clears it)
  role <pane> <role>            set a member's role; its agent is renamed after it
  join <pane> [--role R]        add a pane to its group's team
  leave <pane>                  remove a member (it is not auto-joined again)
  hook                          Claude's per-turn team hook (reads stdin)

<group> is a group id (w3), label or number; <pane> a pane id (w3:p1).
The changing commands do not run from a pane whose agent is running
(agents use the agents_team tool).";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Hook,
    List,
    Get(Option<String>),
    Make {
        group: String,
        purpose: Option<String>,
    },
    Disband(String),
    Purpose {
        group: String,
        purpose: Option<String>,
    },
    Role {
        pane: String,
        role: Option<String>,
    },
    Join {
        pane: String,
        role: Option<String>,
    },
    Leave(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    command: Command,
    json: bool,
}

fn non_empty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut json = false;
    let mut purpose: Option<String> = None;
    let mut role: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut rest = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if rest {
            positional.push(arg.clone());
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        match flag {
            "--" => rest = true,
            "-h" | "--help" if positional.is_empty() => {
                return Ok(Parsed {
                    command: Command::Help,
                    json,
                })
            }
            "--json" if inline.is_none() => json = true,
            "--purpose" | "--role" => {
                let value = match inline {
                    Some(value) => value,
                    None => iter
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))?,
                };
                if flag == "--purpose" {
                    purpose = Some(value);
                } else {
                    role = Some(value);
                }
            }
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            _ => positional.push(arg.clone()),
        }
    }
    let mut positional = positional.into_iter();
    let Some(verb) = positional.next() else {
        return Ok(Parsed {
            command: Command::Help,
            json,
        });
    };
    let mut one = |what: &str| {
        positional
            .next()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{verb}: {what} is required"))
    };
    let flag_for = |name: &str, allowed: bool| -> Result<(), String> {
        if allowed {
            Ok(())
        } else {
            Err(format!("--{name} does not apply here"))
        }
    };
    let command = match verb.as_str() {
        "help" => Command::Help,
        "hook" => Command::Hook,
        "list" => Command::List,
        "get" => Command::Get(positional.next()),
        "make" => Command::Make {
            group: one("<group>")?,
            purpose: purpose.take().and_then(non_empty),
        },
        "disband" => Command::Disband(one("<group>")?),
        "purpose" => {
            let group = one("<group>")?;
            let text = positional.by_ref().collect::<Vec<_>>().join(" ");
            Command::Purpose {
                group,
                purpose: non_empty(text),
            }
        }
        "role" => {
            let pane = one("<pane>")?;
            let text = positional.by_ref().collect::<Vec<_>>().join(" ");
            Command::Role {
                pane,
                role: non_empty(text),
            }
        }
        "join" => Command::Join {
            pane: one("<pane>")?,
            role: role.take().and_then(non_empty),
        },
        "leave" => Command::Leave(one("<pane>")?),
        other => return Err(format!("unknown team command {other:?}")),
    };
    if positional.next().is_some() {
        return Err(format!("{verb}: too many arguments (quote text)"));
    }
    flag_for("purpose", purpose.is_none())?;
    flag_for("role", role.is_none())?;
    Ok(Parsed { command, json })
}

fn method_for(command: &Command) -> Option<Method> {
    Some(match command.clone() {
        Command::Help | Command::Hook => return None,
        Command::List => Method::TeamList(EmptyParams::default()),
        Command::Get(group) => {
            let caller_pane = group
                .is_none()
                .then(super::target::caller_pane_id)
                .flatten();
            Method::TeamGet(TeamGetParams {
                workspace_id: group,
                caller_pane,
            })
        }
        Command::Make { group, purpose } => Method::TeamMake(TeamMakeParams {
            workspace_id: group,
            purpose,
            caller_pane: None,
        }),
        Command::Disband(group) => Method::TeamDisband(TeamWorkspaceParams {
            workspace_id: group,
        }),
        Command::Purpose { group, purpose } => Method::TeamSetPurpose(TeamSetPurposeParams {
            workspace_id: group,
            purpose,
            caller_pane: None,
        }),
        Command::Role { pane, role } => Method::TeamSetRole(TeamSetRoleParams {
            pane_id: pane,
            role,
            caller_pane: None,
        }),
        Command::Join { pane, role } => Method::TeamJoin(TeamJoinParams {
            pane_id: pane,
            role,
        }),
        Command::Leave(pane) => Method::TeamLeave(TeamPaneParams { pane_id: pane }),
    })
}

/// Whether `command` changes a team (everything but list, get and hook).
fn changes_a_team(command: &Command) -> bool {
    !matches!(
        command,
        Command::Help | Command::Hook | Command::List | Command::Get(_)
    )
}

/// Refuse a changing verb run from `env_pane` while an agent runs there
/// (an agent's shell tool): the server would record it as the user and
/// skip `agents_team`'s guards. A plain shell pane, a script outside herdr
/// or an unreachable server is not refused here.
fn refuse_agent_caller(api: &impl Api, env_pane: Option<&str>) -> Result<(), String> {
    let Some(env_pane) = env_pane.filter(|pane| !pane.trim().is_empty()) else {
        return Ok(());
    };
    let pane = coordinator_api::resolve_caller(api, env_pane)
        .map(|caller| caller.pane_id)
        .unwrap_or_else(|_| env_pane.to_string());
    let Ok(info) = coordinator_api::agent_get(api, &pane) else {
        return Ok(());
    };
    let text = |value: &serde_json::Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let Some(kind) = text(&info["agent"]).or_else(|| text(&info["agent_session"]["agent"])) else {
        return Ok(());
    };
    let name = text(&info["name"]).unwrap_or(kind);
    Err(format!(
        "{pane} runs an agent ({name}): changing a team stays with the user (the TUI, or herdr team from a shell pane); agents use the agents_team tool"
    ))
}

/// `herdr team hook …` (any further arguments are ignored).
fn is_hook(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("hook")
}

/// `herdr team …`: 0 ok, 1 refused by the server, 2 usage. `hook` is always 0.
pub fn run_team_command(args: &[String]) -> std::io::Result<i32> {
    // The hook never parses arguments: a Claude hook exiting 2 would block
    // the prompt, so it is 0 whatever it is given and whatever happens.
    if is_hook(args) {
        if let Err(err) = crate::agent_wrap::team::run_hook() {
            tracing::debug!(err = %err, "herdr team hook failed");
        }
        return Ok(0);
    }
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("herdr team: {message}\n{USAGE}");
            return Ok(2);
        }
    };
    let method = match &parsed.command {
        Command::Help => {
            println!("{USAGE}");
            return Ok(0);
        }
        // Handled before parsing (see `is_hook`).
        Command::Hook => return Ok(0),
        command => match method_for(command) {
            Some(method) => method,
            None => return Ok(2),
        },
    };
    if changes_a_team(&parsed.command) {
        let env_pane = super::target::caller_pane_id();
        if let Err(message) =
            refuse_agent_caller(&super::coordinator::SocketApi, env_pane.as_deref())
        {
            eprintln!("herdr team: {message}");
            return Ok(1);
        }
    }
    if matches!(parsed.command, Command::Get(None)) {
        if let Method::TeamGet(TeamGetParams {
            caller_pane: None, ..
        }) = &method
        {
            eprintln!("herdr team get: name a group, or run it inside a herdr pane");
            return Ok(2);
        }
    }
    let response = super::send_request(&Request {
        id: "cli:team".into(),
        method,
    })?;
    if parsed.json {
        return super::print_response(&response);
    }
    if let Some(error) = response.get("error") {
        eprintln!(
            "herdr team: {}: {}",
            error["code"].as_str().unwrap_or("error"),
            error["message"].as_str().unwrap_or("")
        );
        return Ok(1);
    }
    let result = &response["result"];
    match result["type"].as_str() {
        Some("team_list") => {
            let teams = result["teams"].as_array().cloned().unwrap_or_default();
            if teams.is_empty() {
                println!("no teams");
            }
            for team in &teams {
                print!("{}", describe_team(team));
            }
        }
        _ => {
            if result["team"].is_object() {
                print!("{}", describe_team(&result["team"]));
            } else {
                println!("no team");
            }
            if result["renamed"] == serde_json::Value::Bool(false) {
                println!("(every name for that role is taken; the agent keeps its name)");
            }
        }
    }
    Ok(0)
}

/// A team as a few human lines.
fn describe_team(team: &serde_json::Value) -> String {
    let text = |value: &serde_json::Value| value.as_str().unwrap_or("").to_string();
    let mut out = format!(
        "◆ {} ({})",
        text(&team["workspace_label"]),
        text(&team["workspace_id"])
    );
    match team["purpose"].as_str() {
        Some(purpose) => out.push_str(&format!(" · {purpose}\n")),
        None => out.push_str(" · (no purpose yet)\n"),
    }
    let members = team["members"].as_array().cloned().unwrap_or_default();
    if members.is_empty() {
        out.push_str("  no members\n");
    }
    for member in &members {
        let role = member["role"].as_str().unwrap_or("(no role)");
        let name = member["name"].as_str().unwrap_or("-");
        let agent = member["agent"].as_str().unwrap_or("no agent");
        let status = member["status"].as_str().unwrap_or("");
        out.push_str(&format!(
            "  {role:<16} {name:<16} {agent:<8} {:<8} {status}\n",
            text(&member["pane_id"])
        ));
    }
    if let Some(excluded) = team["excluded"].as_array().filter(|list| !list.is_empty()) {
        let panes: Vec<String> = excluded.iter().map(text).collect();
        out.push_str(&format!("  removed: {}\n", panes.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    fn command(list: &[&str]) -> Command {
        parse(&args(list)).expect("parses").command
    }

    #[test]
    fn changing_verbs_refuse_a_pane_whose_agent_runs() {
        use crate::coordinator::api::ApiError;
        use serde_json::json;
        // w3:p1 runs claude "fixer" (its launch-time id w3:p9 is an alias);
        // w1:p3 is a plain shell.
        let api = |method: Method| -> Result<serde_json::Value, ApiError> {
            match method {
                Method::BrowserResolveCaller(caller) => {
                    let pane = if caller.pane_id == "w3:p9" {
                        "w3:p1".to_string()
                    } else {
                        caller.pane_id
                    };
                    Ok(json!({ "actor": { "kind": "pane", "pane_id": pane,
                        "tab_id": "w3:t1", "workspace_id": "w3", "shell_pid": 1, "gone": false } }))
                }
                Method::AgentGet(target) if target.target == "w3:p1" => {
                    Ok(json!({ "agent": { "agent": "claude", "name": "fixer" } }))
                }
                _ => Err(ApiError::new("agent_not_found", "no agent")),
            }
        };
        let refused = refuse_agent_caller(&api, Some("w3:p9")).unwrap_err();
        assert!(refused.contains("w3:p1 runs an agent (fixer)"), "{refused}");
        assert!(refused.contains("agents_team"), "{refused}");
        assert_eq!(refuse_agent_caller(&api, Some("w1:p3")), Ok(()));
        assert_eq!(refuse_agent_caller(&api, None), Ok(()));
        for verb in [
            &["make", "w3"][..],
            &["disband", "w3"],
            &["purpose", "w3", "x"],
            &["role", "w3:p1", "x"],
            &["join", "w3:p1"],
            &["leave", "w3:p1"],
        ] {
            assert!(changes_a_team(&command(verb)), "{verb:?}");
        }
        for verb in [&["list"][..], &["get"], &["get", "w3"], &["hook"]] {
            assert!(!changes_a_team(&command(verb)), "{verb:?}");
        }
    }

    #[test]
    fn parses_every_command() {
        assert_eq!(command(&[]), Command::Help);
        assert_eq!(command(&["hook"]), Command::Hook);
        assert_eq!(command(&["list"]), Command::List);
        assert_eq!(command(&["get"]), Command::Get(None));
        assert_eq!(command(&["get", "w3"]), Command::Get(Some("w3".into())));
        assert_eq!(
            command(&["make", "w3", "--purpose", "fix the demo greeting"]),
            Command::Make {
                group: "w3".into(),
                purpose: Some("fix the demo greeting".into())
            }
        );
        assert_eq!(command(&["disband", "w3"]), Command::Disband("w3".into()));
        assert_eq!(
            command(&["purpose", "w3", "ship", "the", "fix"]),
            Command::Purpose {
                group: "w3".into(),
                purpose: Some("ship the fix".into())
            }
        );
        assert_eq!(
            command(&["purpose", "w3", ""]),
            Command::Purpose {
                group: "w3".into(),
                purpose: None
            }
        );
        assert_eq!(
            command(&["role", "w3:p1", "Code", "Reviewer"]),
            Command::Role {
                pane: "w3:p1".into(),
                role: Some("Code Reviewer".into())
            }
        );
        assert_eq!(
            command(&["join", "w3:p2", "--role=fixer"]),
            Command::Join {
                pane: "w3:p2".into(),
                role: Some("fixer".into())
            }
        );
        assert_eq!(command(&["leave", "w3:p2"]), Command::Leave("w3:p2".into()));
        let parsed = parse(&args(&["list", "--json"])).unwrap();
        assert!(parsed.json);
    }

    #[test]
    fn the_hook_is_recognized_before_any_parsing() {
        assert!(is_hook(&args(&["hook"])));
        assert!(is_hook(&args(&["hook", "--bogus", "x"])));
        assert!(
            parse(&args(&["hook", "--bogus"])).is_err(),
            "parse would refuse"
        );
        assert!(!is_hook(&args(&["list", "hook"])));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&args(&["make"])).is_err());
        assert!(parse(&args(&["make", "w3", "--purpose"])).is_err());
        assert!(parse(&args(&["leave", "w3:p1", "extra"])).is_err());
        assert!(parse(&args(&["disband", "w3", "--role", "x"])).is_err());
        assert!(parse(&args(&["frobnicate"])).is_err());
        assert!(parse(&args(&["list", "--loud"])).is_err());
    }

    #[test]
    fn methods_never_carry_a_caller_for_the_user() {
        let Some(Method::TeamMake(params)) = method_for(&Command::Make {
            group: "w3".into(),
            purpose: None,
        }) else {
            panic!("expected team.make");
        };
        assert_eq!(params.caller_pane, None);
        assert!(method_for(&Command::Hook).is_none());
        assert!(matches!(
            method_for(&Command::Get(Some("w3".into()))),
            Some(Method::TeamGet(TeamGetParams {
                caller_pane: None,
                ..
            }))
        ));
    }

    #[test]
    fn a_team_reads_as_lines() {
        let team = serde_json::json!({
            "workspace_id": "w3", "workspace_label": "demo-team", "purpose": "fix it",
            "members": [
                {"pane_id": "w3:p1", "name": "fixer", "agent": "claude", "role": "fixer", "status": "idle"},
                {"pane_id": "w3:p2"}
            ],
            "excluded": ["w3:p5"]
        });
        let text = describe_team(&team);
        assert!(text.starts_with("◆ demo-team (w3) · fix it\n"), "{text}");
        assert!(
            text.contains("fixer") && text.contains("(no role)"),
            "{text}"
        );
        assert!(text.contains("removed: w3:p5"), "{text}");
    }
}
