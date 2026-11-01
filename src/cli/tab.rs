#[path = "tab_bindings.rs"]
mod bindings;

pub(crate) use bindings::command;

use crate::api::schema::agents_model::{
    AgentsCheckAction, AgentsCloseTabParams, AgentsOpenTabParams, AgentsRenameTabParams,
    AgentsSetQuietParams,
};
use crate::api::schema::{Method, Request};

use super::agent_route as route;

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    // Fork: these subcommands parse their own arguments (their help comes
    // from spec.rs, which adds them to the upstream `tab` command).
    match args.first().map(String::as_str) {
        Some("color") => return tab_color(&args[1..]),
        Some("important") => return tab_important(&args[1..]),
        Some("remind") => return tab_remind(&args[1..]),
        Some(verb @ ("mute" | "unmute" | "dnd")) => return tab_quiet(verb, &args[1..]),
        Some("closed") => return super::tab_closed::tab_closed(&args[1..]),
        Some("reopen") => return super::tab_closed::tab_reopen(&args[1..]),
        // clap's help subcommand over the spec command, so the listing
        // includes the fork's subcommands.
        Some("help") => {
            return match super::spec::tab_command()
                .bin_name("herdr tab")
                .no_binary_name(true)
                .try_get_matches_from(args)
            {
                Err(error) => super::print_clap_error(&error),
                Ok(_) => Ok(2),
            };
        }
        _ => {}
    }
    let request = bindings::request(args);
    // Fork (agents v2): an agent creates, renames and closes tabs through
    // the agents model (checked, graceful, resumable).
    if let Ok(request) = &request {
        if let Some(code) = route_agent_request(request)? {
            return Ok(code);
        }
    }
    super::run_json_request(request)
}

/// Fork (agents v2): the agent route for a parsed `tab` request; `None`
/// keeps the upstream path (the user, an old server, or an unrouted method).
fn route_agent_request(request: &Request) -> std::io::Result<Option<i32>> {
    if !matches!(
        request.method,
        Method::TabCreate(_) | Method::TabRename(_) | Method::TabClose(_)
    ) {
        return Ok(None);
    }
    let pane = match route::route()? {
        Err(code) => return Ok(Some(code)),
        Ok(route::Route::Agent(pane)) => pane,
        Ok(route::Route::User | route::Route::OldServer) => return Ok(None),
    };
    let code = match &request.method {
        Method::TabCreate(params) => route::call(
            "cli:agents.open_tab",
            Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: pane,
                group: params.workspace_id.clone(),
                cwd: params.cwd.clone(),
                label: params.label.clone(),
                ..AgentsOpenTabParams::default()
            }),
        )?,
        Method::TabRename(params) => route::call(
            "cli:agents.rename_tab",
            Method::AgentsRenameTab(AgentsRenameTabParams {
                caller_pane: pane,
                target: params.tab_id.clone(),
                name: params.label.clone(),
            }),
        )?,
        Method::TabClose(params) => route::call(
            "cli:agents.close_tab",
            Method::AgentsCloseTab(AgentsCloseTabParams {
                caller_pane: pane,
                target: params.tab_id.clone(),
                ..AgentsCloseTabParams::default()
            }),
        )?,
        _ => return Ok(None),
    };
    Ok(Some(code))
}

fn tab_color(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str =
        "usage: herdr tab color <tab_id> <red|orange|yellow|green|cyan|blue|purple|none>";
    let [raw_tab_id, raw_color] = args else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let color = if raw_color.eq_ignore_ascii_case("none") {
        None
    } else if let Some(color) = crate::api::schema::TabColor::from_name(raw_color) {
        Some(color)
    } else {
        eprintln!("unknown tab color: {raw_color}");
        eprintln!("{USAGE}");
        return Ok(2);
    };

    let tab_id = super::normalize_tab_id(raw_tab_id);
    route::checked(AgentsCheckAction::Cosmetic, Some(tab_id.clone()), || {
        super::print_response(&super::send_request(&crate::api::schema::Request {
            id: "cli:tab:color".into(),
            method: crate::api::schema::Method::TabSetColor(
                crate::api::schema::TabSetColorParams { tab_id, color },
            ),
        })?)
    })
}

/// What `herdr tab mute|unmute|dnd` asks for: `(muted, dnd, targets)`.
type QuietRequest = (Option<bool>, Option<bool>, Vec<String>);

/// Parse `mute <TARGET>...`, `unmute <TARGET>...` and
/// `dnd <on|off> <TARGET>...`; `Err` is the usage line.
fn parse_quiet(verb: &str, args: &[String]) -> Result<QuietRequest, &'static str> {
    const MUTE_USAGE: &str = "usage: herdr tab mute|unmute <TARGET>...";
    const DND_USAGE: &str = "usage: herdr tab dnd <on|off> <TARGET>...";
    let (muted, dnd, targets) = match verb {
        "mute" | "unmute" => (Some(verb == "mute"), None, args),
        _ => {
            let Some((state, targets)) = args.split_first() else {
                return Err(DND_USAGE);
            };
            let on = match state.to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(DND_USAGE),
            };
            (None, Some(on), targets)
        }
    };
    // A flag (`--help`) is no target.
    if targets.is_empty()
        || targets
            .iter()
            .any(|target| target.trim().is_empty() || target.starts_with('-'))
    {
        return Err(if dnd.is_some() { DND_USAGE } else { MUTE_USAGE });
    }
    Ok((muted, dnd, targets.to_vec()))
}

/// Fork: `herdr tab mute|unmute <TARGET>...` and `herdr tab dnd <on|off>
/// <TARGET>...` through `agents.set_quiet` (a target is a tab id, a pane
/// id, an agent name or a tab label). An agent calls as itself (its own
/// team's tabs); the user without a caller.
fn tab_quiet(verb: &str, args: &[String]) -> std::io::Result<i32> {
    let (muted, dnd, targets) = match parse_quiet(verb, args) {
        Ok(request) => request,
        Err(usage) => {
            eprintln!("{usage}");
            return Ok(2);
        }
    };
    let caller_pane = match route::route()? {
        Err(code) => return Ok(code),
        Ok(route::Route::Agent(pane)) => Some(pane),
        Ok(route::Route::User | route::Route::OldServer) => None,
    };
    let mut worst = 0;
    for target in targets {
        let method = Method::AgentsSetQuiet(AgentsSetQuietParams {
            caller_pane: caller_pane.clone(),
            target,
            muted,
            dnd,
        });
        let code = if caller_pane.is_some() {
            route::call("cli:agents.set_quiet", method)?
        } else {
            super::print_response(&super::send_request(&Request {
                id: "cli:agents.set_quiet".into(),
                method,
            })?)?
        };
        worst = worst.max(code);
    }
    Ok(worst)
}

fn tab_important(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr tab important <tab_id> <on|off>";
    let [raw_tab_id, raw_state] = args else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let important = match raw_state.to_ascii_lowercase().as_str() {
        "on" => true,
        "off" => false,
        _ => {
            eprintln!("expected on or off: {raw_state}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    send_reminder(raw_tab_id, Some(important), None, "cli:tab:important")
}

fn tab_remind(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr tab remind <tab_id> <off|5m|10m|30m|1h|6h|daily>";
    let [raw_tab_id, raw_every] = args else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let Some(every) = crate::api::schema::TabRemindEvery::from_name(raw_every) else {
        eprintln!("expected off, 5m, 10m, 30m, 1h, 6h or daily: {raw_every}");
        eprintln!("{USAGE}");
        return Ok(2);
    };
    send_reminder(raw_tab_id, None, Some(every), "cli:tab:remind")
}

fn send_reminder(
    raw_tab_id: &str,
    important: Option<bool>,
    every: Option<crate::api::schema::TabRemindEvery>,
    id: &str,
) -> std::io::Result<i32> {
    use crate::api::schema::{Request, TabSetReminderParams};
    let tab_id = super::normalize_tab_id(raw_tab_id);
    route::checked(AgentsCheckAction::Cosmetic, Some(tab_id.clone()), || {
        super::print_response(&super::send_request(&Request {
            id: id.into(),
            method: Method::TabSetReminder(TabSetReminderParams {
                tab_id,
                important,
                every,
            }),
        })?)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::api::schema::{TabCreateParams, TabListParams};

    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn simple_families_tab_rejects_invalid_grammar_before_transport() {
        for values in [
            vec![],
            vec!["unknown"],
            vec!["list", "--workspace"],
            vec!["create", "--cwd"],
            vec!["create", "--label"],
            vec!["create", "--env"],
            vec!["create", "--env", "NO_EQUALS"],
            vec!["create", "--focus=true"],
            vec!["get"],
            vec!["get", "2", "extra"],
            vec!["focus"],
            vec!["focus", "2", "extra"],
            vec!["rename", "2"],
            vec!["close"],
            vec!["close", "2", "extra"],
        ] {
            assert!(
                bindings::request(&args(&values)).is_err(),
                "argv: {values:?}"
            );
        }
    }

    #[test]
    fn simple_families_tab_dto_literal_defaults() {
        let create: TabCreateParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            create,
            TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: false,
                label: None,
                env: HashMap::new(),
            }
        );
        assert_eq!(
            serde_json::to_value(create).unwrap(),
            serde_json::json!({"focus": false})
        );
        assert_eq!(
            serde_json::to_value(TabListParams::default()).unwrap(),
            serde_json::json!({})
        );
    }
}

#[cfg(test)]
mod fork_tests {
    use crate::api::schema::TabRemindEvery;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn mute_unmute_and_dnd_parse_their_targets() {
        assert_eq!(
            super::parse_quiet("mute", &args(&["fixer", "w2:t3"])),
            Ok((Some(true), None, args(&["fixer", "w2:t3"])))
        );
        assert_eq!(
            super::parse_quiet("unmute", &args(&["fixer"])),
            Ok((Some(false), None, args(&["fixer"])))
        );
        assert_eq!(
            super::parse_quiet("dnd", &args(&["ON", "w1:p2"])),
            Ok((None, Some(true), args(&["w1:p2"])))
        );
        assert_eq!(
            super::parse_quiet("dnd", &args(&["off", "fixer", "rev"])),
            Ok((None, Some(false), args(&["fixer", "rev"])))
        );
        for (verb, values) in [
            ("mute", vec![]),
            ("unmute", vec![" "]),
            ("dnd", vec![]),
            ("dnd", vec!["on"]),
            ("dnd", vec!["maybe", "fixer"]),
            ("dnd", vec!["fixer"]),
            ("mute", vec!["--help"]),
        ] {
            assert!(
                super::parse_quiet(verb, &args(&values)).is_err(),
                "{verb} {values:?}"
            );
        }
    }

    #[test]
    fn remind_intervals_parse_by_name() {
        for (raw, every) in [
            ("off", TabRemindEvery::Off),
            ("5m", TabRemindEvery::M5),
            ("10M", TabRemindEvery::M10),
            ("30m", TabRemindEvery::M30),
            ("1h", TabRemindEvery::H1),
            ("6h", TabRemindEvery::H6),
            ("Daily", TabRemindEvery::Daily),
        ] {
            assert_eq!(TabRemindEvery::from_name(raw), Some(every), "{raw}");
        }
        assert_eq!(TabRemindEvery::from_name("2h"), None);
    }
}
