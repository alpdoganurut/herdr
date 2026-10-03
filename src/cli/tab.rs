use std::collections::HashMap;

use crate::api::schema::agents_model::{
    AgentsCheckAction, AgentsCloseTabParams, AgentsOpenTabParams, AgentsRenameTabParams,
};
use crate::api::schema::{Method, TabCreateParams, TabListParams, TabRenameParams};

use super::agent_route as route;

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "color" => tab_color(&args[1..]),
        "important" => tab_important(&args[1..]),
        "remind" => tab_remind(&args[1..]),
        "closed" => super::tab_closed::tab_closed(&args[1..]),
        "reopen" => super::tab_closed::tab_reopen(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::runtime::tab_list(TabListParams { workspace_id })
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --env");
                    return Ok(2);
                };
                let (key, value) = match super::parse_env_assignment(value) {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                env.insert(key, value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    // Fork (agents v2): an agent opens a shell tab through the check.
    match route::route()? {
        Err(code) => return Ok(code),
        Ok(route::Route::Agent(pane)) => {
            return route::call(
                "cli:agents.open_tab",
                Method::AgentsOpenTab(AgentsOpenTabParams {
                    caller_pane: pane,
                    group: workspace_id,
                    cwd,
                    label,
                    ..AgentsOpenTabParams::default()
                }),
            )
        }
        Ok(route::Route::User | route::Route::OldServer) => {}
    }
    super::runtime::tab_create(TabCreateParams {
        workspace_id,
        cwd,
        focus,
        label,
        env,
    })
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_get(super::normalize_tab_id(raw_tab_id))
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    super::runtime::tab_focus(super::normalize_tab_id(raw_tab_id))
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    let tab_id = super::normalize_tab_id(&args[0]);
    let label = args[1..].join(" ");
    // Fork (agents v2): an agent renames through the check.
    match route::route()? {
        Err(code) => return Ok(code),
        Ok(route::Route::Agent(pane)) => {
            return route::call(
                "cli:agents.rename_tab",
                Method::AgentsRenameTab(AgentsRenameTabParams {
                    caller_pane: pane,
                    target: tab_id,
                    name: label,
                }),
            )
        }
        Ok(route::Route::User | route::Route::OldServer) => {}
    }
    super::runtime::tab_rename(TabRenameParams { tab_id, label })
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

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab close <tab_id>");
        return Ok(2);
    }

    let tab_id = super::normalize_tab_id(raw_tab_id);
    // Fork (agents v2): an agent closes through the check (graceful exits,
    // resumable, only on its user's request).
    match route::route()? {
        Err(code) => return Ok(code),
        Ok(route::Route::Agent(pane)) => {
            return route::call(
                "cli:agents.close_tab",
                Method::AgentsCloseTab(AgentsCloseTabParams {
                    caller_pane: pane,
                    target: tab_id,
                    ..AgentsCloseTabParams::default()
                }),
            )
        }
        Ok(route::Route::User | route::Route::OldServer) => {}
    }
    super::runtime::tab_close(tab_id)
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab color <tab_id> <red|orange|yellow|green|cyan|blue|purple|none>");
    eprintln!("  herdr tab important <tab_id> <on|off>");
    eprintln!("  herdr tab remind <tab_id> <off|5m|10m|30m|1h|6h|daily>");
    eprintln!("  herdr tab closed [--json]");
    eprintln!("  herdr tab reopen <n|id|session-id-prefix>");
    eprintln!("  herdr tab close <tab_id>");
}

#[cfg(test)]
mod tests {
    use crate::api::schema::TabRemindEvery;

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
