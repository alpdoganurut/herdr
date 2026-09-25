use std::collections::HashMap;

use crate::api::schema::{TabCreateParams, TabListParams, TabRenameParams};

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
        "remind" => tab_remind(&args[1..]),
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

    super::runtime::tab_rename(TabRenameParams {
        tab_id: super::normalize_tab_id(&args[0]),
        label: args[1..].join(" "),
    })
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

    super::print_response(&super::send_request(&crate::api::schema::Request {
        id: "cli:tab:color".into(),
        method: crate::api::schema::Method::TabSetColor(crate::api::schema::TabSetColorParams {
            tab_id: super::normalize_tab_id(raw_tab_id),
            color,
        }),
    })?)
}

fn tab_remind(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr tab remind <tab_id> <on|off>";
    let [raw_tab_id, raw_state] = args else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let remind = match raw_state.to_ascii_lowercase().as_str() {
        "on" => true,
        "off" => false,
        _ => {
            eprintln!("expected on or off: {raw_state}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&crate::api::schema::Request {
        id: "cli:tab:remind".into(),
        method: crate::api::schema::Method::TabSetRemind(crate::api::schema::TabSetRemindParams {
            tab_id: super::normalize_tab_id(raw_tab_id),
            remind,
        }),
    })?)
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

    super::runtime::tab_close(super::normalize_tab_id(raw_tab_id))
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
    eprintln!("  herdr tab remind <tab_id> <on|off>");
    eprintln!("  herdr tab close <tab_id>");
}
