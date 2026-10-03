use std::io::Write;

use clap::{Arg, ArgAction, ArgGroup, Command, ValueHint};

mod completion;
mod machine;

pub(super) fn command() -> Command {
    let command = Command::new("herdr")
        .about("terminal workspace manager for AI coding agents")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(help_flag())
        .arg(option("session", "NAME").help("Use or create a named persistent session"))
        .arg(option("machine", "LABEL-OR-ID").help("Run an API command on a saved SSH machine"))
        .arg(option("remote", "TARGET").help("Attach through SSH to a remote Herdr server"))
        .arg(
            option("remote-keybindings", "MODE")
                .value_parser(["local", "server"])
                .help("Choose local or server keybindings for remote attach"),
        )
        .arg(flag("handoff").help("Opt into live handoff for update or remote attach"))
        .arg(flag("default-config").help("Print default configuration and exit"))
        .arg(flag("skill").help("Print the agent skill file and exit"))
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::SetTrue)
                .help("Print version and exit"),
        )
        .subcommand(completion::command())
        .subcommand(update_command())
        .subcommand(status_command())
        .subcommand(config_command())
        .subcommand(channel_command())
        .subcommand(machine::command())
        .subcommand(server_command())
        .subcommand(api_command())
        .subcommand(workspace_command())
        .subcommand(worktree_command())
        .subcommand(tab_command())
        .subcommand(notification_command())
        .subcommand(agent_command())
        .subcommand(pane_command())
        .subcommand(terminal_command())
        .subcommand(session_command())
        .subcommand(news_command())
        .subcommand(team_command())
        .subcommand(notes_command())
        .subcommand(checkpoint_command())
        .subcommand(browser_command())
        .subcommand(integration_command())
        .subcommand(plugin_command());
    configure_help(command, 0)
}

fn configure_help(command: Command, depth: usize) -> Command {
    let command = if depth == 0 {
        command
    } else {
        command.disable_help_flag(false)
    };
    let command = if depth == 1 && command.has_subcommands() {
        command.after_help(super::AGENT_HELP_FOOTER)
    } else {
        command
    };
    command
        .disable_help_subcommand(true)
        .mut_subcommands(|subcommand| configure_help(subcommand, depth + 1))
}

pub(super) fn print_requested_help(args: &[String]) -> std::io::Result<bool> {
    let mut stdout = std::io::stdout().lock();
    write_requested_help(args, &mut stdout, crate::platform::begin_cli_output)
}

fn write_requested_help(
    args: &[String],
    output: &mut impl Write,
    before_write: impl FnOnce(),
) -> std::io::Result<bool> {
    let Some(help_index) = args
        .iter()
        .position(|arg| matches!(arg.as_str(), "--help" | "-h"))
    else {
        return Ok(false);
    };
    if help_index < 2 {
        return Ok(false);
    }
    if args[1..help_index].iter().any(|arg| arg == "--") {
        return Ok(false);
    }

    let mut root = command();
    root.build();
    let mut selected = &mut root;
    let mut path = vec!["herdr".to_string()];
    for segment in &args[1..help_index] {
        if selected.find_subcommand(segment).is_none() {
            break;
        }
        path.push(segment.clone());
        selected = selected
            .find_subcommand_mut(segment)
            .expect("subcommand checked immediately before mutable lookup");
    }
    if path.len() == 1 || help_index != path.len() {
        return Ok(false);
    }

    selected.set_bin_name(path.join(" "));
    before_write();
    selected.write_long_help(&mut *output)?;
    writeln!(output)?;
    Ok(true)
}

fn update_command() -> Command {
    Command::new("update")
        .about("Download and install the latest version")
        .arg(flag("handoff").help("Try live handoff after installing"))
}

fn status_command() -> Command {
    Command::new("status")
        .about("Show local client and running server status")
        .arg(json_flag())
        .subcommand(
            Command::new("server")
                .about("Show running server status")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("client")
                .about("Show local client status")
                .arg(json_flag()),
        )
}

fn config_command() -> Command {
    Command::new("config")
        .about("Manage local configuration")
        .subcommand(Command::new("check").about("Validate config.toml and print diagnostics"))
        .subcommand(Command::new("reset-keys").about("Reset custom keybindings"))
}

fn channel_command() -> Command {
    Command::new("channel")
        .about("Manage stable and preview update channels")
        .subcommand(Command::new("show").about("Print the configured update channel"))
        .subcommand(
            Command::new("set").about("Choose the update channel").arg(
                Arg::new("channel")
                    .value_name("CHANNEL")
                    .required(true)
                    .value_parser(["stable", "preview"]),
            ),
        )
}

fn server_command() -> Command {
    Command::new("server")
        .about("Run or control the headless server")
        .subcommand(Command::new("stop").about("Stop the running server"))
        .subcommand(Command::new("reload-config").about("Reload config in the running server"))
        .subcommand(
            Command::new("agent-manifests")
                .about("Show active agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("update-agent-manifests")
                .about("Fetch and reload agent detection manifests")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("reload-agent-manifests")
                .about("Reload local agent detection manifest overrides"),
        )
}

fn api_command() -> Command {
    Command::new("api")
        .about("Inspect socket API metadata and live runtime state")
        .subcommand(Command::new("snapshot").about("Print the live session snapshot"))
        .subcommand(
            Command::new("schema")
                .about("Print or write the bundled API schema")
                .arg(json_flag())
                .arg(path_option("output", "PATH")),
        )
}

fn workspace_command() -> Command {
    Command::new("workspace")
        .about("Manage workspaces over the socket API")
        .subcommand(Command::new("list").about("List workspaces"))
        .subcommand(
            Command::new("create")
                .about("Create a workspace")
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(env_option())
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "workspace_id", "Show a workspace"))
        .subcommand(id_command("focus", "workspace_id", "Focus a workspace"))
        .subcommand(
            Command::new("rename")
                .about("Rename a workspace")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(
            Command::new("report-metadata")
                .about("Report display-only workspace metadata")
                .arg(required("workspace_id", "WORKSPACE_ID"))
                .arg(option("source", "ID").required(true))
                .arg(repeatable_option("token", "NAME=VALUE"))
                .arg(repeatable_option("clear-token", "NAME"))
                .arg(option("seq", "N"))
                .arg(option("ttl-ms", "N")),
        )
        .subcommand(id_command("close", "workspace_id", "Close a workspace"))
}

fn worktree_command() -> Command {
    Command::new("worktree")
        .about("Manage Git worktree-backed workspaces")
        .subcommand(
            Command::new("list")
                .about("List worktree workspaces")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(flag("trust-repository")),
        )
        .subcommand(
            Command::new("create")
                .about("Create and open a Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("base", "REF"))
                .arg(path_option("path", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus"))
                .arg(flag("trust-repository")),
        )
        .subcommand(
            Command::new("open")
                .about("Open an existing Git worktree")
                .arg(option("workspace", "ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(path_option("path", "PATH"))
                .arg(option("branch", "NAME"))
                .arg(option("label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus"))
                .arg(flag("trust-repository")),
        )
        .subcommand(
            Command::new("remove")
                .about("Remove a worktree checkout")
                .arg(option("workspace", "ID"))
                .arg(flag("force"))
                .arg(flag("trust-repository")),
        )
}

fn tab_command() -> Command {
    Command::new("tab")
        .about("Manage tabs over the socket API")
        .subcommand(
            Command::new("list")
                .about("List tabs")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(
            Command::new("create")
                .about("Create a tab")
                .arg(option("workspace", "WORKSPACE_ID"))
                .arg(path_option("cwd", "PATH"))
                .arg(option("label", "TEXT"))
                .arg(env_option())
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("get", "tab_id", "Show a tab"))
        .subcommand(id_command("focus", "tab_id", "Focus a tab"))
        .subcommand(
            Command::new("rename")
                .about("Rename a tab")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("label", "LABEL").num_args(1..)),
        )
        .subcommand(
            Command::new("color")
                .about("Set or clear a tab's color tag")
                .override_usage("herdr tab color <TAB_ID> <COLOR>")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("color", "COLOR").value_parser([
                    "red", "orange", "yellow", "green", "cyan", "blue", "purple", "none",
                ]))
                .after_help(
                    "Tags the tab with a named theme color (tab.set_color); `none` clears it. The color is stored with the tab, persists across server restarts and follows the tab when it moves to another space. The `tabs` sidebar layout draws the tab's name in the color.",
                ),
        )
        .subcommand(
            Command::new("important")
                .about("Mark or unmark a tab as important")
                .override_usage("herdr tab important <TAB_ID> <on|off>")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("state", "STATE").value_parser(["on", "off"]))
                .after_help(
                    "Marks the tab important (tab.set_reminder); `off` removes the mark. While an important tab's agent sits finished (unseen) or blocked, the client reminds you every ui.idle_reminder_minutes until you focus the tab. The mark is stored with the tab, persists across server restarts and follows the tab when it moves to another space.",
                ),
        )
        .subcommand(
            Command::new("remind")
                .about("Set or clear a tab's scheduled reminder")
                .override_usage("herdr tab remind <TAB_ID> <off|5m|10m|30m|1h|6h|daily>")
                .arg(required("tab_id", "TAB_ID"))
                .arg(required("every", "EVERY").value_parser([
                    "off", "5m", "10m", "30m", "1h", "6h", "daily",
                ]))
                .after_help(
                    "Sets the tab's scheduled reminder (tab.set_reminder); `off` clears it. The client reminds you about the tab every interval, whatever its agent is doing, or daily at ui.daily_reminder_time; a reminder due while the tab is focused is skipped. The interval is stored with the tab, persists across server restarts and follows the tab when it moves to another space.",
                ),
        )
        .subcommand(
            Command::new("closed")
                .about("List recently closed agent sessions")
                .override_usage("herdr tab closed [--json]")
                .arg(json_flag())
                .after_help(
                    "Lists the agent sessions of recently closed tabs, newest first (session.closed_list): number, tab label (else the agent), group, directory and how long ago it closed. Closing a tab, pane or space that holds a live or suspended agent session records it; plain shells, tabs moved to another group and server shutdowns do not. The server keeps the newest 100 in closed-sessions.json next to session.json.",
                ),
        )
        .subcommand(
            Command::new("reopen")
                .about("Reopen a recently closed agent session")
                .override_usage("herdr tab reopen <N|ID|SESSION_ID_PREFIX>")
                .arg(required("session", "N|ID|SESSION_ID_PREFIX"))
                .after_help(
                    "Reopens a session from `herdr tab closed` by its number, its record id or a unique prefix of its session id (session.closed_reopen): a new tab in the original group (the first space when that group is gone) with the tab's label, color and reminders, in the pane's directory, running the agent's native resume command. A deleted native transcript is put back from the transcript backup store first. The record is removed once the tab is open.",
                ),
        )
        .subcommand(id_command("close", "tab_id", "Close a tab"))
}

fn notification_command() -> Command {
    Command::new("notification")
        .about("Show Herdr notifications")
        .subcommand(
            Command::new("show")
                .about("Show a notification")
                .arg(required("title", "TITLE"))
                .arg(option("body", "TEXT"))
                .arg(option("position", "POSITION").value_parser([
                    "top-left",
                    "top-right",
                    "bottom-left",
                    "bottom-right",
                ]))
                .arg(option("sound", "SOUND").value_parser(["none", "done", "request"])),
        )
}

fn agent_command() -> Command {
    Command::new("agent")
        .about("Control and inspect agent panes")
        .subcommand(Command::new("list").about("List agents"))
        .subcommand(id_command("get", "target", "Show an agent"))
        .subcommand(
            Command::new("read")
                .about("Read agent terminal output")
                .override_usage("herdr agent read <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi")),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to an agent")
                .arg(required("target", "TARGET"))
                .arg(required("key", "KEY").num_args(1..))
                .after_help("Use esc as the canonical Escape key name; escape is also accepted."),
        )
        .subcommand(
            Command::new("prompt")
                .about("Submit a prompt to an agent")
                .override_usage("herdr agent prompt <TARGET> <TEXT> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(required("text", "TEXT"))
                .arg(
                    flag("wait")
                        .help("Wait for the first matching state observed after submission"),
                )
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .requires("wait")
                        .value_parser(["idle", "working", "blocked", "done", "unknown"])
                        .help("State to match after --wait; repeat for more than one state"),
                )
                .arg(
                    option("timeout", "MS")
                        .requires("wait")
                        .help("Fail after this many milliseconds"),
                )
                .after_help(
                    "If the agent is already blocked, submission is rejected with agent_blocked before any input is sent. When an accepted submission starts from another non-working state, --wait requires an observed working or blocked state within 5000ms; otherwise it returns agent_prompt_stalled. A caller timeout that expires first returns timeout. It then matches idle, done, or blocked by default, or any exact --until state. It does not track turns: if the agent is already working, that active turn's completion may match.",
                ),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename an agent")
                .override_usage("herdr agent rename <TARGET> <NAME>|--clear")
                .arg(required("target", "TARGET"))
                .arg(Arg::new("name").value_name("NAME"))
                .arg(flag("clear"))
                .group(
                    ArgGroup::new("rename")
                        .args(["name", "clear"])
                        .required(true),
                ),
        )
        .subcommand(id_command("focus", "target", "Focus an agent"))
        .subcommand(
            Command::new("wrap")
                .about("Run claude or codex with what [agents] adds (tools, instructions, browser steering)")
                .override_usage("herdr agent wrap <claude|codex> [--print] [--] [ARG]...")
                .arg(Arg::new("agent").value_parser(["claude", "codex"]).required(true))
                .arg(flag("print").help("Print the binary and argv instead of running it"))
                .arg(
                    Arg::new("args")
                        .num_args(0..)
                        .allow_hyphen_values(true)
                        .trailing_var_arg(true),
                )
                .after_help(
                    "Per launch: --no-herdr (before a --) or HERDR_NO_WRAP=1 runs the agent as typed.",
                ),
        )
        .subcommand(
            Command::new("notify")
                .about("Show your user an agent card from this pane until they dismiss it or visit its tab")
                .override_usage("herdr agent notify <TITLE> [--body TEXT] [--kind KIND] [--json]")
                .arg(required("title", "TITLE"))
                .arg(option("body", "TEXT"))
                .arg(option("kind", "KIND").value_parser(["info", "question", "done", "warning"]))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("wait")
                .about("Wait until an agent reaches one of the requested states")
                .override_usage("herdr agent wait <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(
                    option("until", "STATUS")
                        .action(ArgAction::Append)
                        .value_parser([
                            "idle",
                            "working",
                            "blocked",
                            "done",
                            "unknown",
                            "suspended",
                        ])
                        .help("State to match; repeat for more than one state"),
                )
                .arg(option("timeout", "MS").help("Fail after this many milliseconds"))
                .after_help(
                    "Without --until, matches idle, done, or blocked. Use --until unknown explicitly when needed. Without --timeout, waits indefinitely.",
                ),
        )
        .subcommand(
            Command::new("attach")
                .about("Attach directly to an agent terminal")
                .override_usage("herdr agent attach <TARGET> [OPTIONS]")
                .arg(required("target", "TARGET"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("start")
                .about("Start a supported interactive agent in an existing pane")
                .override_usage(
                    "herdr agent start <NAME> --kind <KIND> --pane <ID> [OPTIONS] [-- [AGENT_ARG]...]",
                )
                .arg(required("name", "NAME"))
                .arg(
                    option("kind", "KIND")
                        .required(true)
                        .value_parser(agent_kind_values())
                        .help("Supported agent kind and canonical executable"),
                )
                .arg(
                    option("pane", "ID")
                        .required(true)
                        .help("Existing pane at an interactive shell prompt"),
                )
                .arg(
                    option("timeout", "MS")
                        .help("Wait for interactive readiness (default: 30000; max: 300000)"),
                )
                .arg(
                    Arg::new("agent_args")
                        .value_name("AGENT_ARG")
                        .num_args(0..)
                        .last(true),
                )
                .after_help(
                    "The pane must be at its interactive shell prompt. Success means the expected agent was detected in the same terminal and is ready for input.\n\nnext: herdr agent prompt <TARGET> <TEXT> --wait",
                ),
        )
        .subcommand(
            Command::new("suspend")
                .about("Ask a running agent to exit while its pane keeps the native session")
                .override_usage("herdr agent suspend <TARGET>")
                .arg(required("target", "TARGET"))
                .after_help(
                    "The agent must be running with a known native session reference and a resume plan (currently Claude Code). Herdr submits the agent's exit command, then terminates and finally kills the foreground job if it does not exit. The pane reports `suspended`, keeps its name, and survives server restarts without being relaunched.\n\nnext: herdr agent activate <TARGET>",
                ),
        )
        .subcommand(
            Command::new("activate")
                .about("Relaunch a suspended agent in its own pane")
                .override_usage("herdr agent activate <TARGET>")
                .arg(required("target", "TARGET"))
                .after_help(
                    "The pane must be back at its interactive shell prompt. Herdr runs the stored native resume command in that pane and restores the agent name.\n\nnext: herdr agent wait <TARGET>",
                ),
        )
        .subcommand(
            Command::new("restart")
                .about("Exit an idle agent and relaunch it in place with its native resume command")
                .override_usage("herdr agent restart <TARGET>")
                .arg(required("target", "TARGET"))
                .after_help(
                    "Suspends the agent like `agent suspend`, then relaunches it in the same pane with its native resume command as soon as its exit is observed and the shell prompt is back; no `agent activate` is needed. A working agent is refused (`agent_working`), as are blocked (`agent_blocked`) and suspended (`agent_suspended`) ones. If the relaunch cannot happen (the agent had to be killed, or the shell never returns) the pane stays suspended.\n\nnext: herdr agent wait <TARGET>",
                ),
        )
        .subcommand(
            Command::new("explain")
                .about("Explain agent detection state")
                .arg(Arg::new("target").value_name("TARGET"))
                .arg(path_option("file", "PATH"))
                .arg(option("agent", "LABEL"))
                .arg(json_flag())
                .arg(text_json_format_option())
                .arg(
                    Arg::new("verbose")
                        .short('v')
                        .long("verbose")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("transcripts")
                .about("List backed-up native agent transcripts")
                .override_usage("herdr agent transcripts [--json]")
                .arg(json_flag())
                .after_help(
                    "Reads the backup store under the session directory (agent-transcripts/) directly; no server is needed. Each row shows the agent, session id, size, backup time, and whether the agent's own transcript file is currently present or missing. Backups are written for open and suspended agent panes while session.backup_agent_transcripts is enabled and are never deleted by Herdr.",
                ),
        )
}

pub(super) fn agent_kind_values() -> Vec<&'static str> {
    crate::detect::Agent::ALL
        .into_iter()
        .map(crate::detect::agent_label)
        .collect()
}

fn pane_command() -> Command {
    Command::new("pane")
        .about("Control terminal panes")
        .subcommand(
            Command::new("list")
                .about("List panes")
                .arg(option("workspace", "WORKSPACE_ID")),
        )
        .subcommand(
            Command::new("current")
                .about("Show the current pane")
                .args(current_pane_args()),
        )
        .subcommand(id_command("get", "pane_id", "Show a pane"))
        .subcommand(
            Command::new("layout")
                .about("Show pane layout information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("process-info")
                .about("Show pane process information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("neighbor")
                .about("Find a pane neighbor")
                .arg(required_direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("edges")
                .about("Show pane edge information")
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("focus")
                .about("Focus a neighboring pane")
                .arg(required_direction_option())
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("resize")
                .about("Resize a pane split")
                .arg(required_direction_option())
                .arg(option("amount", "FLOAT"))
                .args(current_pane_args()),
        )
        .subcommand(
            Command::new("zoom")
                .about("Toggle or set pane zoom")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(flag("toggle"))
                .arg(flag("on"))
                .arg(flag("off")),
        )
        .subcommand(
            Command::new("read")
                .about("Read pane terminal output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(read_source_option(true))
                .arg(option("lines", "N"))
                .arg(text_ansi_format_option())
                .arg(flag("ansi"))
                .arg(flag("raw")),
        )
        .subcommand(
            Command::new("rename")
                .about("Rename a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(Arg::new("label").value_name("LABEL").num_args(1..))
                .arg(flag("clear")),
        )
        .subcommand(
            Command::new("input")
                .about("Set pane input routing")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(
                    option("right-click", "TARGET")
                        .value_parser(["herdr", "pane"])
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("split")
                .about("Split a pane")
                .arg(Arg::new("pane_id").value_name("PANE_ID"))
                .args(current_pane_args())
                .arg(split_direction_option())
                .arg(option("ratio", "FLOAT"))
                .arg(path_option("cwd", "PATH"))
                .arg(env_option())
                .arg(option("right-click", "TARGET").value_parser(["herdr", "pane"]))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(
            Command::new("swap")
                .about("Swap panes")
                .arg(direction_option())
                .args(current_pane_args())
                .arg(option("source-pane", "ID"))
                .arg(option("target-pane", "ID")),
        )
        .subcommand(
            Command::new("move")
                .about("Move a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(option("tab", "TAB_ID"))
                .arg(option("split", "DIRECTION").value_parser(["right", "down"]))
                .arg(option("target-pane", "ID"))
                .arg(option("ratio", "FLOAT"))
                .arg(flag("new-tab"))
                .arg(option("workspace", "ID"))
                .arg(flag("new-workspace"))
                .arg(option("label", "TEXT"))
                .arg(option("tab-label", "TEXT"))
                .arg(flag("focus"))
                .arg(flag("no-focus")),
        )
        .subcommand(id_command("close", "pane_id", "Close a pane"))
        .subcommand(
            Command::new("send-text")
                .about("Send literal text to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("text", "TEXT"))
                .after_help(
                    "next: herdr pane run <PANE_ID> <COMMAND> sends text and Enter in one call",
                ),
        )
        .subcommand(
            Command::new("send-keys")
                .about("Send key presses to a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("key", "KEY").num_args(1..))
                .after_help("Use esc as the canonical Escape key name; escape is also accepted."),
        )
        .subcommand(
            Command::new("wait-output")
                .about("Wait for matching pane output")
                .arg(required("pane_id", "PANE_ID"))
                .arg(
                    option("match", "TEXT")
                        .conflicts_with("regex")
                        .required_unless_present("regex")
                        .help("Match a literal substring"),
                )
                .arg(
                    option("regex", "PATTERN")
                        .conflicts_with("match")
                        .required_unless_present("match")
                        .help("Match a Rust regular expression"),
                )
                .arg(read_source_option(false))
                .arg(option("lines", "N").help("Restrict the searched snapshot to N lines"))
                .arg(option("timeout", "MS").help("Fail after this many milliseconds"))
                .arg(flag("raw").help("Keep ANSI escape sequences while matching"))
                .group(
                    ArgGroup::new("matcher")
                        .args(["match", "regex"])
                        .required(true),
                )
                .after_help(
                    "The selected snapshot is searched immediately, including existing output, then polled. Without --timeout, this waits indefinitely.",
                ),
        )
        .subcommand(
            Command::new("run")
                .about("Run a command in a pane")
                .arg(required("pane_id", "PANE_ID"))
                .arg(required("command", "COMMAND").num_args(1..)),
        )
        .subcommand(report_agent_command())
        .subcommand(report_agent_session_command())
        .subcommand(release_agent_command())
        .subcommand(report_metadata_command())
}

fn report_agent_command() -> Command {
    Command::new("report-agent")
        .about("Report pane agent lifecycle state")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(pane_agent_state_option("state"))
        .arg(option("message", "TEXT"))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
}

fn report_agent_session_command() -> Command {
    Command::new("report-agent-session")
        .about("Report pane agent session identity")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(option("seq", "N"))
        .arg(option("agent-session-id", "ID"))
        .arg(path_option("agent-session-path", "PATH"))
        .arg(option("session-start-source", "SOURCE"))
}

fn release_agent_command() -> Command {
    Command::new("release-agent")
        .about("Release pane agent lifecycle authority")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL").required(true))
        .arg(option("seq", "N"))
}

fn report_metadata_command() -> Command {
    Command::new("report-metadata")
        .about("Report display-only pane metadata")
        .arg(required("pane_id", "PANE_ID"))
        .arg(option("source", "ID").required(true))
        .arg(option("agent", "LABEL"))
        .arg(option("applies-to-source", "ID"))
        .arg(option("title", "TEXT"))
        .arg(flag("clear-title"))
        .arg(option("display-agent", "TEXT"))
        .arg(flag("clear-display-agent"))
        .arg(option("state-label", "STATUS=TEXT"))
        .arg(flag("clear-state-labels"))
        .arg(repeatable_option("token", "NAME=VALUE"))
        .arg(repeatable_option("clear-token", "NAME"))
        .arg(option("seq", "N"))
        .arg(option("ttl-ms", "N"))
}

fn terminal_command() -> Command {
    Command::new("terminal")
        .about("Attach to or observe raw terminal streams")
        .subcommand(
            Command::new("attach")
                .about("Attach directly to a terminal stream")
                .arg(required("terminal_id", "TERMINAL_ID"))
                .arg(flag("takeover")),
        )
        .subcommand(
            Command::new("session")
                .about("Work with terminal sessions")
                .subcommand(
                    Command::new("control")
                        .about("Control a terminal stream")
                        .arg(required("target", "TARGET"))
                        .arg(flag("takeover"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                )
                .subcommand(
                    Command::new("observe")
                        .about("Observe a terminal stream")
                        .arg(required("target", "TARGET"))
                        .arg(option("cols", "N"))
                        .arg(option("rows", "N")),
                ),
        )
        .subcommand(
            Command::new("title")
                .about("Manage the outer terminal title")
                .subcommand(
                    Command::new("set")
                        .about("Set the outer terminal title")
                        .arg(required("title", "TITLE")),
                )
                .subcommand(Command::new("clear").about("Clear the outer terminal title")),
        )
}

fn browser_tab_arg() -> Arg {
    Arg::new("tab")
        .value_name("TAB")
        .required(false)
        .help("Tab id (t3 or main:t3); default: this pane's current tab")
}

fn browser_common(command: Command) -> Command {
    browser_common_without_timeout(command)
        .arg(option("timeout", "MS").help("Deadline for this operation"))
}

fn browser_common_without_timeout(command: Command) -> Command {
    command
        .arg(option("profile", "NAME").help("Browser profile (new = a fresh temporary one)"))
        .arg(
            option("pane", "PANE_ID")
                .help("Attribute the call to this pane instead of HERDR_PANE_ID"),
        )
        .arg(json_flag())
}

/// Verbs whose positional is required take the tab as `--tab` in the spec
/// (the parser also accepts a leading `t3`, but clap forbids an optional
/// positional before a required one).
fn browser_tab_option() -> Arg {
    option("tab", "TAB").help("Tab id (t3 or main:t3); default: this pane's current tab")
}

fn browser_command() -> Command {
    let verb =
        |name: &'static str, about: &'static str| browser_common(Command::new(name).about(about));
    Command::new("browser")
        .about("Drive the herdr-owned Chromium (shared with the user); browser.run and friends")
        .long_about(
            "A Chromium window herdr launches for agents and the user shares. Read loop: `browser open URL` (page card), then `browser read` (paged markdown, follow the next: offset) or `browser find TEXT`; `browser snapshot` for structure (refs eN); `browser screenshot` to see it. Each pane has a current tab (set by open/use or --tab). The user logs in by hand (`browser focus`, then ask). Every call is logged with the calling pane (`browser status`, `browser log`).",
        )
        .subcommand(
            verb("open", "Open a URL in a new background tab and make it the current tab")
                .arg(Arg::new("url").value_name("URL").required(true))
                .arg(flag("focus").help("Also select the tab and raise the window"))
                .arg(option("wait", "STATE").value_parser(["domcontentloaded", "load", "networkidle"])),
        )
        .subcommand(
            verb("navigate", "Navigate the current tab to a URL")
                .arg(browser_tab_option())
                .arg(Arg::new("url").value_name("URL").required(true))
                .arg(option("wait", "STATE").value_parser(["domcontentloaded", "load", "networkidle"])),
        )
        .subcommand(verb("back", "Go back in the current tab").arg(browser_tab_arg()))
        .subcommand(verb("forward", "Go forward in the current tab").arg(browser_tab_arg()))
        .subcommand(verb("reload", "Reload the current tab").arg(browser_tab_arg()))
        .subcommand(
            verb("read", "Read the current tab as paged markdown, text, an aria snapshot or html")
                .arg(browser_tab_arg())
                .arg(option("format", "FORMAT").value_parser(["markdown", "text", "snapshot", "html"]))
                .arg(option("selector", "CSS"))
                .arg(option("ref", "REF").help("Aria ref (e12) from a snapshot"))
                .arg(option("offset", "N"))
                .arg(option("max", "N").help("Characters per page"))
                .arg(flag("all").help("Everything, no paging"))
                .arg(flag("interactive").help("Snapshot: only actionable nodes"))
                .arg(option("out", "FILE").help("Write everything to FILE")),
        )
        .subcommand(
            verb("snapshot", "Aria snapshot with refs (read --format snapshot)")
                .arg(browser_tab_arg())
                .arg(flag("interactive"))
                .arg(option("selector", "CSS"))
                .arg(option("ref", "REF"))
                .arg(option("offset", "N"))
                .arg(option("max", "N")),
        )
        .subcommand(
            verb("find", "Find text or /regex/ in the current tab's markdown")
                .arg(browser_tab_option())
                .arg(Arg::new("query").value_name("TEXT").required(true).num_args(1..))
                .arg(option("max", "N"))
                .arg(option("context", "CHARS")),
        )
        .subcommand(
            verb("links", "List the current tab's links")
                .arg(browser_tab_arg())
                .arg(option("filter", "TEXT"))
                .arg(option("max", "N")),
        )
        .subcommand(
            verb("screenshot", "Screenshot the current tab to shots/ (or --out)")
                .arg(browser_tab_arg())
                .arg(flag("full"))
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS"))
                .arg(option("format", "FORMAT").value_parser(["jpeg", "png"]))
                .arg(option("out", "PATH"))
                .arg(flag("front").help("Select the tab and raise the window first")),
        )
        .subcommand(
            verb("console", "Console messages since attach")
                .arg(browser_tab_arg())
                .arg(option("level", "LEVEL").value_parser(["error", "warn", "all"]))
                .arg(option("since", "SEQ"))
                .arg(option("max", "N")),
        )
        .subcommand(
            verb("network", "Network requests since attach (no bodies)")
                .arg(browser_tab_arg())
                .arg(flag("failed"))
                .arg(option("match", "SUBSTR"))
                .arg(option("type", "TYPE"))
                .arg(option("since", "SEQ"))
                .arg(option("max", "N")),
        )
        .subcommand(
            browser_common_without_timeout(
                Command::new("wait").about("Wait for text, a selector, a URL or a load state"),
            )
                .arg(browser_tab_arg())
                .arg(option("text", "TEXT"))
                .arg(option("gone", "TEXT"))
                .arg(option("selector", "CSS"))
                .arg(option("url", "GLOB"))
                .arg(option("load", "STATE").value_parser(["domcontentloaded", "load", "networkidle"]))
                .arg(option("timeout", "SECONDS")),
        )
        .subcommand(
            verb("scroll", "Scroll the current tab")
                .arg(browser_tab_arg())
                .arg(option("to", "WHERE").help("top, bottom or a ref eN"))
                .arg(option("by", "PX")),
        )
        .subcommand(
            verb("eval", "Evaluate a JavaScript expression in the current tab")
                .arg(browser_tab_option())
                .arg(Arg::new("expr").value_name("EXPR").required(true).num_args(1..))
                .arg(option("max", "N")),
        )
        .subcommand(
            verb("dialog", "Accept or dismiss the open dialog")
                .arg(browser_tab_option())
                .arg(Arg::new("action").value_name("ACTION").required(true).value_parser(["accept", "dismiss"]))
                .arg(Arg::new("text").value_name("TEXT").required(false).num_args(0..)),
        )
        .subcommand(verb("tabs", "List open tabs with who opened and last used them").arg(flag("mine")))
        .subcommand(verb("use", "Make a tab the current tab").arg(Arg::new("tab").value_name("TAB").required(true)))
        .subcommand(
            verb("click", "Click an element by aria ref or CSS selector")
                .arg(browser_tab_arg())
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS")),
        )
        .subcommand(
            verb("type", "Type text into an element key by key")
                .arg(browser_tab_option())
                .arg(Arg::new("text").value_name("TEXT").required(true).num_args(1..))
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS"))
                .arg(flag("submit").help("Press Enter after the text"))
                .arg(flag("clear").help("Empty the field first")),
        )
        .subcommand(
            verb("fill", "Set an input's value at once")
                .arg(browser_tab_option())
                .arg(Arg::new("text").value_name("TEXT").required(true).num_args(1..))
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS")),
        )
        .subcommand(
            verb("press", "Press a key on an element or the focused one")
                .arg(browser_tab_option())
                .arg(Arg::new("key").value_name("KEY").required(true))
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS")),
        )
        .subcommand(
            verb("select", "Pick an option of a select by value or label")
                .arg(browser_tab_option())
                .arg(Arg::new("value").value_name("VALUE").required(true))
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS")),
        )
        .subcommand(
            verb("hover", "Hover an element by aria ref or CSS selector")
                .arg(browser_tab_arg())
                .arg(option("ref", "REF"))
                .arg(option("selector", "CSS")),
        )
        .subcommand(
            verb("batch", "Run a JSON array of steps (stdin or --file) on the current tab")
                .arg(browser_tab_arg())
                .arg(option("file", "FILE").help("Steps file; default: stdin"))
                .arg(flag("continue").help("Keep going after a failing step"))
                .arg(option("final", "WHAT").value_parser(["snapshot", "screenshot"]))
                .arg(flag("close-opened").help("Close the tabs the batch opened after the final step"))
                .arg(flag("no-animate").help("Skip the activity cursor's glide between steps")),
        )
        .subcommand(verb("close", "Close the current tab").arg(browser_tab_arg()))
        .subcommand(verb("focus", "Select the tab and raise the window for the user").arg(browser_tab_arg()))
        .subcommand(Command::new("status").about("Browser, sidecar, profiles, tabs and recent activity").arg(json_flag()))
        .subcommand(
            Command::new("log")
                .about("Browser activity, newest first")
                .arg(Arg::new("count").short('n').value_name("N").required(false))
                .arg(option("pane", "PANE_ID"))
                .arg(option("tab", "TAB"))
                .arg(json_flag()),
        )
        .subcommand(Command::new("start").about("Start a profile's browser now").arg(option("profile", "NAME")).arg(json_flag()))
        .subcommand(Command::new("stop").about("Close a profile's browser (or every running one)").arg(option("profile", "NAME")).arg(flag("all")).arg(json_flag()))
        .subcommand(
            Command::new("profile")
                .about("List, create or delete browser profiles")
                .subcommand(Command::new("list").about("List profiles").arg(json_flag()))
                .subcommand(Command::new("create").about("Create a profile (--temp marks it temporary)").arg(Arg::new("name").value_name("NAME").required(true)).arg(flag("temp")))
                .subcommand(Command::new("delete").about("Move a profile to the Trash (not the default, not a running one)").arg(Arg::new("name").value_name("NAME").required(true))),
        )
        .subcommand(
            Command::new("setup")
                .about("Install the Playwright sidecar (npm ci) and register the MCP server for Claude Code and/or Codex")
                .arg(flag("claude").help("Register with Claude Code (user scope)"))
                .arg(flag("codex").help("Register in Codex's config.toml with the pane variables forwarded"))
                .arg(flag("shell").help("Only write the managed shell file and add one guarded line to ~/.zshrc (plain codex/claude in herdr+ panes → herdr agent wrap); plain setup never edits ~/.zshrc"))
                .arg(flag("remove").help("With --shell: take the line out of ~/.zshrc again"))
                .arg(flag("no-mcp"))
                .arg(option("node", "PATH")),
        )
        .subcommand(
            Command::new("wrap")
                .about("Alias of `herdr agent wrap` (older managed shell files call it)")
                .arg(Arg::new("agent").value_parser(["codex", "claude"]).required(true))
                .arg(Arg::new("args").num_args(0..).allow_hyphen_values(true).trailing_var_arg(true)),
        )
        .subcommand(
            Command::new("install-chromium")
                .about("Install a branded copy of a built Chromium.app (name, icon; ad-hoc signed) that \"auto\" finds first")
                .arg(Arg::new("app").value_name("CHROMIUM_APP").required(true))
                .arg(option("icon", "PNG|ICNS"))
                .arg(option("name", "NAME"))
                .arg(option("dest", "DIR")),
        )
        .subcommand(Command::new("doctor").about("Check the browser setup: executable, node, sidecar, MCP registration, server"))
        .subcommand(Command::new("mcp").about("Serve the browser tools over stdio MCP (started by Claude Code)"))
}

/// `herdr team` (fork teams).
fn team_command() -> Command {
    let group = || Arg::new("group").value_name("GROUP");
    let pane = || Arg::new("pane").value_name("PANE").required(true);
    Command::new("team")
        .about("Inspect and change teams (team groups of agents)")
        .long_about(
            "A team is a group whose agents work together: every agent started in it joins, is named after its role and may message its teammates. GROUP is a group id (w3), label or number; PANE a pane id (w3:p1). The command line acts as the user, so the changing commands do not run from a pane whose agent is running (agents use the agents_team tool).",
        )
        .subcommand(
            Command::new("list")
                .about("Show every team")
                .override_usage("herdr team list [--json]")
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("get")
                .about("Show a group's team (default: this pane's group)")
                .override_usage("herdr team get [GROUP] [--json]")
                .arg(group())
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("make")
                .about("Mark a group as a team; its agents join")
                .override_usage("herdr team make GROUP [--purpose TEXT] [--json]")
                .arg(group().required(true))
                .arg(option("purpose", "TEXT"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("disband")
                .about("Unmark a team group (agents keep their names)")
                .override_usage("herdr team disband GROUP [--json]")
                .arg(group().required(true))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("purpose")
                .about("Set a team's purpose (empty text clears it)")
                .override_usage("herdr team purpose GROUP TEXT [--json]")
                .arg(group().required(true))
                .arg(Arg::new("text").value_name("TEXT").num_args(0..))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("role")
                .about("Set a member's role; its agent is renamed after it")
                .override_usage("herdr team role PANE ROLE [--json]")
                .arg(pane())
                .arg(Arg::new("role").value_name("ROLE").num_args(0..))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("join")
                .about("Add a pane to its group's team")
                .override_usage("herdr team join PANE [--role ROLE] [--json]")
                .arg(pane())
                .arg(option("role", "ROLE"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("leave")
                .about("Remove a member (it is not auto-joined again)")
                .override_usage("herdr team leave PANE [--json]")
                .arg(pane())
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("hook")
                .about("Claude's per-turn team hook (reads the hook JSON on stdin; always exits 0)")
                .override_usage("herdr team hook"),
        )
}

fn news_command() -> Command {
    Command::new("news")
        .about("Run and inspect the AI news desk")
        .subcommand(
            Command::new("run")
                .about("Start a news run now")
                .override_usage("herdr news run")
                .long_about(
                    "Starts one run of the bundled news runner in the News tab of the first space (news.run): the tab is created when missing, the page viewer is quit when showing, and `python3 <home>/bin/news_run.py --home <home> --trigger manual` is typed into its shell. Refused while a run is in flight. Works whether or not [news] enabled is set; enabled only turns on the schedule.",
                ),
        )
        .subcommand(
            Command::new("status")
                .about("Show the news schedule, the run in flight and the last runs")
                .override_usage("herdr news status [--json]")
                .arg(json_flag())
                .long_about(
                    "Prints whether scheduled runs are on, the scheduled times and quiet hours, the news home, the News tab, the next scheduled run, the run in flight and the last five runs with their outcome, duration, cost, turns and edition (news.status).",
                ),
        )
        .subcommand(
            Command::new("log")
                .about("Show the last news runs from the run log")
                .override_usage("herdr news log [N] [--json]")
                .arg(Arg::new("count").value_name("N").required(false))
                .arg(json_flag())
                .long_about(
                    "Prints the newest N runs (default 10) from <home>/runs/index.jsonl, newest first, with the editor's summary and any validation errors. On a remote target (or when the home is not on this machine) the server's own history is shown instead (the newest 50, as news.status reports them).",
                ),
        )
        .subcommand(
            Command::new("open")
                .about("Focus the News tab, or show a past edition")
                .override_usage("herdr news open [--edition N]")
                .arg(
                    Arg::new("edition")
                        .long("edition")
                        .value_name("N")
                        .required(false),
                )
                .long_about(
                    "Focuses the News tab (news.open), creating it in the first space with the page viewer when it is gone. With --edition N the viewer is quit with `q` and reopened on that edition; refused while a run is in flight.",
                ),
        )
        .subcommand(
            Command::new("history")
                .about("List the published editions")
                .override_usage("herdr news history [--days N] [--json]")
                .arg(Arg::new("days").long("days").value_name("N").required(false))
                .arg(json_flag())
                .long_about(
                    "Lists every edition from <home>/editions/index.json, oldest first (news.history): number, local time, trigger, story count and whether the page changed. --days N keeps the last N local days.",
                ),
        )
        .subcommand(
            Command::new("times")
                .about("Show or set the local times of the scheduled runs")
                .override_usage("herdr news times [HH:MM ...] [--clear]")
                .arg(
                    Arg::new("times")
                        .value_name("HH:MM")
                        .num_args(0..)
                        .required(false),
                )
                .arg(Arg::new("clear").long("clear").action(ArgAction::SetTrue))
                .long_about(
                    "Without arguments lists news.times with the next run marked (news.get). With times, writes them to news.times in the config file, sorted and without duplicates, and reloads it (news.set_times); --clear empties the list, so no scheduled run starts while `enabled` stays as it is. Each time is a local 24-hour HH:MM.",
                ),
        )
        .subcommand(
            Command::new("enable")
                .about("Turn scheduled news runs on")
                .override_usage("herdr news enable")
                .long_about(
                    "Writes news.enabled = true to the config file and reloads it (news.set_enabled); the schedule starts from the next due slot.",
                ),
        )
        .subcommand(
            Command::new("disable")
                .about("Turn scheduled news runs off")
                .override_usage("herdr news disable")
                .long_about(
                    "Writes news.enabled = false to the config file and reloads it (news.set_enabled); `herdr news run` still works.",
                ),
        )
}

/// `--tab`, `--pane` and `--key`: which notes, outside a herdr pane.
fn notes_target_args(command: Command) -> Command {
    command
        .arg(option("tab", "TAB_ID"))
        .arg(option("pane", "PANE_ID"))
        .arg(option("key", "KEY"))
}

fn notes_command() -> Command {
    Command::new("notes")
        .about("Read and write the per-session notes behind the info pane")
        .subcommand(notes_target_args(
            Command::new("read")
                .about("Print the notes (the key, revision and path go to stderr)")
                .override_usage("herdr notes read [--json]")
                .arg(json_flag())
                .long_about(
                    "Prints the notes of this pane's agent session, else of its tab (notes.get). Outside a herdr pane name them with --tab, --pane or --key. --json prints the whole record, including the revision `notes write --base` needs.",
                ),
        ))
        .subcommand(notes_target_args(
            Command::new("path")
                .about("Print where the notes file is")
                .override_usage("herdr notes path")
                .long_about(
                    "Prints the notes file's path. Outside a herdr pane and without --tab, --pane or --key it prints the notes directory, which also keeps the notes of tabs that no longer exist.",
                ),
        ))
        .subcommand(notes_target_args(
            Command::new("append")
                .about("Append a line to the notes, optionally under a section")
                .override_usage("herdr notes append [TEXT|-] [--section S] [--stamp]")
                .arg(Arg::new("text").value_name("TEXT").required(false))
                .arg(option("section", "SECTION"))
                .arg(flag("stamp"))
                .long_about(
                    "Appends TEXT (or stdin with `-` or no TEXT) to the notes (notes.append): at the end, or at the end of `## SECTION`, which is created when missing. --stamp prefixes `- HH:MM `. Never conflicts; prints the new revision.",
                ),
        ))
        .subcommand(notes_target_args(
            Command::new("write")
                .about("Replace the notes if they are still at a revision")
                .override_usage("herdr notes write --base REV [--file F | -]")
                .arg(option("base", "REV").required(true))
                .arg(option("file", "FILE"))
                .arg(Arg::new("stdin").value_name("-").required(false))
                .long_about(
                    "Replaces the notes with FILE or stdin when they are still at revision REV (notes.set; `none` for notes that do not exist yet). On a conflict prints `conflict` and exits 3; read them again and retry.",
                ),
        ))
        .subcommand(
            Command::new("hook")
                .about("Claude's SessionStart notes recall hook (reads the hook JSON on stdin; always exits 0)")
                .override_usage("herdr notes hook"),
        )
}

fn checkpoint_command() -> Command {
    Command::new("checkpoint")
        .about("Mark and list decisions, milestones, failures, bookmarks and notes")
        .subcommand(notes_target_args(
            Command::new("add")
                .about("Add a checkpoint")
                .override_usage(
                    "herdr checkpoint add <decision|milestone|failure|bookmark|note> TITLE [--detail D] [--tag T]... [--as user]",
                )
                .arg(
                    Arg::new("kind")
                        .value_name("KIND")
                        .required(true)
                        .value_parser(["decision", "milestone", "failure", "bookmark", "note"]),
                )
                .arg(Arg::new("title").value_name("TITLE").required(true).num_args(1..))
                .arg(option("detail", "DETAIL"))
                .arg(repeatable_option("tag", "TAG"))
                .arg(option("as", "AUTHOR").value_parser(["user", "agent"]))
                .long_about(
                    "Adds a checkpoint to this pane's session (checkpoints.add) and prints its id. The same kind, title and author within two minutes updates that checkpoint instead. Titles are at most 120 characters, details 2000, and at most 8 tags of 32.",
                ),
        ))
        .subcommand(notes_target_args(
            Command::new("list")
                .about("List checkpoints, oldest first")
                .override_usage("herdr checkpoint list [--kind K] [--limit N] [--json]")
                .arg(
                    repeatable_option("kind", "KIND")
                        .value_parser(["decision", "milestone", "failure", "bookmark", "note"]),
                )
                .arg(option("limit", "N"))
                .arg(json_flag()),
        ))
        .subcommand(notes_target_args(
            Command::new("show")
                .about("Show a checkpoint with the prompt and reply around it")
                .override_usage("herdr checkpoint show ID [--chars N]")
                .arg(Arg::new("id").value_name("ID").required(true))
                .arg(option("chars", "N"))
                .arg(json_flag())
                .long_about(
                    "Shows the checkpoint and the conversation around it from the agent's transcript (checkpoints.context), waiting up to five seconds while it is read. --chars sets the characters per side (default 600, at most 4000).",
                ),
        ))
        .subcommand(notes_target_args(
            Command::new("rm")
                .about("Remove a checkpoint")
                .override_usage("herdr checkpoint rm ID")
                .arg(Arg::new("id").value_name("ID").required(true)),
        ))
        .subcommand(notes_target_args(
            Command::new("edit")
                .about("Change a checkpoint's title, detail, kind or tags")
                .override_usage(
                    "herdr checkpoint edit ID [--title T] [--detail D] [--kind K] [--tag T]...",
                )
                .arg(Arg::new("id").value_name("ID").required(true))
                .arg(option("title", "TITLE"))
                .arg(option("detail", "DETAIL"))
                .arg(
                    option("kind", "KIND")
                        .value_parser(["decision", "milestone", "failure", "bookmark", "note"]),
                )
                .arg(repeatable_option("tag", "TAG")),
        ))
}

fn session_command() -> Command {
    Command::new("session")
        .about("Manage named persistent sessions")
        .subcommand(Command::new("list").about("List sessions").arg(json_flag()))
        .subcommand(
            Command::new("attach")
                .about("Attach to a session")
                .arg(required("name", "NAME")),
        )
        .subcommand(
            Command::new("stop")
                .about("Stop a session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("delete")
                .about("Delete a stopped session")
                .arg(required("name", "NAME"))
                .arg(json_flag()),
        )
}

fn integration_command() -> Command {
    Command::new("integration")
        .about("Manage built-in agent integrations")
        .subcommand(
            Command::new("install")
                .about("Install an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall an integration")
                .arg(integration_target_arg()),
        )
        .subcommand(
            Command::new("status")
                .about("Show integration status")
                .arg(flag("outdated-only")),
        )
}

fn plugin_command() -> Command {
    Command::new("plugin")
        .about("Install and run workflow plugins")
        .subcommand(
            Command::new("install")
                .about("Install a plugin from GitHub")
                .arg(required("source", "OWNER/REPO[/SUBDIR]"))
                .arg(option("ref", "REF"))
                .arg(
                    Arg::new("yes")
                        .short('y')
                        .long("yes")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Uninstall a plugin")
                .arg(required("plugin", "PLUGIN")),
        )
        .subcommand(
            Command::new("link")
                .about("Link a local plugin")
                .arg(path_arg("path", "PATH"))
                .arg(flag("disabled"))
                .arg(flag("enabled")),
        )
        .subcommand(
            Command::new("unlink")
                .about("Unlink a local plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("enable")
                .about("Enable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("disable")
                .about("Disable a plugin")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("list")
                .about("List installed plugins")
                .arg(option("plugin", "ID"))
                .arg(json_flag()),
        )
        .subcommand(
            Command::new("config-dir")
                .about("Print a plugin config directory")
                .arg(required("plugin_id", "PLUGIN_ID")),
        )
        .subcommand(
            Command::new("action")
                .about("List or invoke plugin actions")
                .subcommand(
                    Command::new("list")
                        .about("List plugin actions")
                        .arg(option("plugin", "ID")),
                )
                .subcommand(
                    Command::new("invoke")
                        .about("Invoke a plugin action")
                        .arg(required("action_id", "ACTION_ID"))
                        .arg(option("plugin", "ID")),
                ),
        )
        .subcommand(
            Command::new("log")
                .about("Inspect plugin command logs")
                .visible_alias("logs")
                .subcommand(
                    Command::new("list")
                        .about("List plugin command logs")
                        .arg(option("plugin", "ID"))
                        .arg(option("limit", "N")),
                ),
        )
        .subcommand(
            Command::new("pane")
                .about("Manage plugin-owned panes")
                .subcommand(
                    Command::new("open")
                        .about("Open a plugin pane")
                        .arg(option("plugin", "ID"))
                        .arg(option("entrypoint", "ID"))
                        .arg(
                            option("placement", "PLACEMENT")
                                .value_parser(["overlay", "split", "tab", "zoomed"]),
                        )
                        .arg(option("workspace", "ID"))
                        .arg(option("target-pane", "PANE"))
                        .arg(split_direction_option())
                        .arg(path_option("cwd", "PATH"))
                        .arg(env_option())
                        .arg(flag("focus"))
                        .arg(flag("no-focus")),
                )
                .subcommand(
                    Command::new("focus")
                        .about("Focus a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                )
                .subcommand(
                    Command::new("close")
                        .about("Close a plugin pane")
                        .arg(required("pane_id", "PANE_ID")),
                ),
        )
}

fn current_pane_args() -> [Arg; 2] {
    [option("pane", "ID"), flag("current")]
}

fn integration_target_arg() -> Arg {
    Arg::new("target")
        .value_name("TARGET")
        .required(true)
        .value_parser(integration_target_values())
}

fn integration_target_values() -> Vec<&'static str> {
    let mut values: Vec<&'static str> = crate::api::schema::IntegrationTarget::ALL
        .into_iter()
        .map(crate::integration::integration_target_label)
        .collect();
    values.extend_from_slice(crate::integration::EXPERIMENTAL_INTEGRATION_TARGET_LABELS);
    values
}

fn id_command(name: &'static str, id: &'static str, about: &'static str) -> Command {
    Command::new(name).about(about).arg(required(id, id))
}

fn direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["left", "right", "up", "down"])
}

fn required_direction_option() -> Arg {
    direction_option().required(true)
}

fn split_direction_option() -> Arg {
    option("direction", "DIRECTION").value_parser(["right", "down"])
}

fn pane_agent_state_option(name: &'static str) -> Arg {
    option(name, "STATUS")
        .required(true)
        .value_parser(["idle", "working", "blocked", "unknown"])
}

fn read_source_option(include_detection: bool) -> Arg {
    let values = if include_detection {
        vec!["visible", "recent", "recent-unwrapped", "detection"]
    } else {
        vec!["visible", "recent", "recent-unwrapped"]
    };
    option("source", "SOURCE")
        .value_parser(values)
        .help("Terminal snapshot source (default: recent)")
}

fn text_ansi_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "ansi"])
}

fn text_json_format_option() -> Arg {
    option("format", "FORMAT").value_parser(["text", "json"])
}

fn json_flag() -> Arg {
    flag("json")
}

fn help_flag() -> Arg {
    Arg::new("help")
        .short('h')
        .long("help")
        .action(ArgAction::SetTrue)
        .help("Show help")
}

fn env_option() -> Arg {
    option("env", "KEY=VALUE")
        .action(ArgAction::Append)
        .help("Set an environment variable for the launched process")
}

fn flag(name: &'static str) -> Arg {
    Arg::new(name).long(name).action(ArgAction::SetTrue)
}

fn option(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name)
        .long(name)
        .value_name(value_name)
        .action(ArgAction::Set)
}

fn repeatable_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).action(ArgAction::Append)
}

fn path_option(name: &'static str, value_name: &'static str) -> Arg {
    option(name, value_name).value_hint(ValueHint::AnyPath)
}

fn required(name: &'static str, value_name: &'static str) -> Arg {
    Arg::new(name).value_name(value_name).required(true)
}

fn path_arg(name: &'static str, value_name: &'static str) -> Arg {
    required(name, value_name).value_hint(ValueHint::AnyPath)
}

#[cfg(test)]
mod tests {
    use clap::{Arg, Command};

    fn command_path<'a>(cmd: &'a Command, path: &[&str]) -> &'a Command {
        let mut current = cmd;
        for name in path {
            current = current
                .get_subcommands()
                .find(|subcommand| subcommand.get_name() == *name)
                .unwrap_or_else(|| panic!("missing command path segment {name}"));
        }
        current
    }

    fn option_values(cmd: &Command, option: &str) -> Vec<String> {
        let arg = cmd
            .get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"));
        arg.get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .map(|value| value.get_name().to_string())
            .collect()
    }

    fn has_option(cmd: &Command, option: &str) -> bool {
        cmd.get_arguments()
            .any(|arg| arg.get_long() == Some(option))
    }

    fn option_arg<'a>(cmd: &'a Command, option: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_long() == Some(option))
            .unwrap_or_else(|| panic!("missing --{option}"))
    }

    fn argument<'a>(cmd: &'a Command, id: &str) -> &'a Arg {
        cmd.get_arguments()
            .find(|arg| arg.get_id() == id)
            .unwrap_or_else(|| panic!("missing argument {id}"))
    }

    fn collect_subcommand_paths(
        cmd: &Command,
        path: &mut Vec<String>,
        paths: &mut Vec<Vec<String>>,
    ) {
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            paths.push(path.clone());
            collect_subcommand_paths(subcommand, path, paths);
            path.pop();
        }
    }

    fn assert_command_descriptions(cmd: &Command, path: &mut Vec<String>) {
        if !path.is_empty() {
            assert!(
                cmd.get_about().is_some(),
                "missing completion description for {}",
                path.join(" ")
            );
        }
        for subcommand in cmd.get_subcommands() {
            path.push(subcommand.get_name().to_string());
            assert_command_descriptions(subcommand, path);
            path.pop();
        }
    }

    #[test]
    fn spec_describes_all_completion_commands() {
        let cmd = super::command();
        assert_command_descriptions(&cmd, &mut Vec::new());
    }

    #[test]
    fn spec_passes_clap_invariants() {
        super::command().debug_assert();
    }

    #[test]
    fn every_spec_subcommand_renders_short_and_long_help() {
        let mut paths = Vec::new();
        collect_subcommand_paths(&super::command(), &mut Vec::new(), &mut paths);

        for path in paths {
            for flag in ["-h", "--help"] {
                let mut args = vec!["herdr".to_string()];
                args.extend(path.iter().cloned());
                args.push(flag.to_string());
                let mut output = Vec::new();
                assert!(
                    super::write_requested_help(&args, &mut output, || {}).unwrap(),
                    "help was not handled for herdr {} {flag}",
                    path.join(" ")
                );
                let output = String::from_utf8(output).unwrap();
                assert!(
                    output.contains(&format!("Usage: herdr {}", path.join(" "))),
                    "unexpected help for herdr {}: {output}",
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn spec_includes_completion_alias_and_shells() {
        let cmd = super::command();
        let completion = command_path(&cmd, &["completion"]);
        assert!(completion
            .get_all_aliases()
            .any(|alias| alias == "completions"));
        let shells = completion
            .get_arguments()
            .find(|arg| arg.get_id() == "shell")
            .unwrap()
            .get_value_parser()
            .possible_values()
            .unwrap()
            .map(|value| value.get_name().to_string())
            .collect::<Vec<_>>();
        assert!(shells.contains(&"zsh".to_string()));
        assert!(shells.contains(&"fish".to_string()));
    }

    #[test]
    fn spec_matches_all_integration_targets() {
        let cmd = super::command();
        let install = command_path(&cmd, &["integration", "install"]);
        let mut expected: Vec<String> = crate::api::schema::IntegrationTarget::ALL
            .map(crate::integration::integration_target_label)
            .map(str::to_string)
            .to_vec();
        expected.extend(
            crate::integration::EXPERIMENTAL_INTEGRATION_TARGET_LABELS
                .iter()
                .map(|label| (*label).to_string()),
        );
        assert_eq!(
            argument(install, "target")
                .get_value_parser()
                .possible_values()
                .unwrap()
                .map(|value| value.get_name().to_string())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn spec_marks_runtime_required_options_as_required() {
        for (path, options) in [
            (&["workspace", "report-metadata"][..], &["source"][..]),
            (&["pane", "neighbor"][..], &["direction"][..]),
            (&["pane", "focus"][..], &["direction"][..]),
            (&["pane", "resize"][..], &["direction"][..]),
            (&["pane", "report-agent"][..], &["source", "agent"][..]),
            (
                &["pane", "report-agent-session"][..],
                &["source", "agent"][..],
            ),
            (&["pane", "release-agent"][..], &["source", "agent"][..]),
            (&["pane", "report-metadata"][..], &["source"][..]),
        ] {
            let cmd = command_path(&super::command(), path).clone();
            for option in options {
                assert!(
                    option_arg(&cmd, option).is_required_set(),
                    "herdr {} --{option} should be required",
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn agent_prompt_until_requires_wait() {
        let error = super::command()
            .try_get_matches_from([
                "herdr", "agent", "prompt", "reviewer", "hello", "--until", "idle",
            ])
            .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn agent_rename_requires_exactly_one_name_or_clear() {
        for valid in [
            &["herdr", "agent", "rename", "reviewer", "worker"][..],
            &["herdr", "agent", "rename", "reviewer", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(valid).is_ok());
        }
        for invalid in [
            &["herdr", "agent", "rename", "reviewer"][..],
            &["herdr", "agent", "rename", "reviewer", "worker", "--clear"][..],
        ] {
            assert!(super::command().try_get_matches_from(invalid).is_err());
        }

        let mut help = Vec::new();
        super::write_requested_help(
            &[
                "herdr".to_string(),
                "agent".to_string(),
                "rename".to_string(),
                "--help".to_string(),
            ],
            &mut help,
            || {},
        )
        .unwrap();
        assert!(String::from_utf8(help)
            .unwrap()
            .contains("Usage: herdr agent rename <TARGET> <NAME>|--clear"));
    }

    #[test]
    fn worktree_json_compatibility_flag_stays_out_of_public_spec() {
        let cmd = super::command();
        for subcommand in ["list", "create", "open", "remove"] {
            let worktree_command = command_path(&cmd, &["worktree", subcommand]);
            assert!(
                !has_option(worktree_command, "json"),
                "herdr worktree {subcommand} should not advertise --json"
            );
        }
    }

    #[test]
    fn spec_includes_nested_plugin_pane_open_options() {
        let cmd = super::command();
        let open = command_path(&cmd, &["plugin", "pane", "open"]);
        assert!(open
            .get_arguments()
            .any(|arg| arg.get_long() == Some("entrypoint")));
        assert!(option_values(open, "placement").contains(&"zoomed".to_string()));
    }

    #[test]
    fn spec_keeps_agent_wait_status_free() {
        let cmd = super::command();
        let wait = command_path(&cmd, &["agent", "wait"]);
        assert!(!has_option(wait, "status"));
        assert_eq!(
            option_values(wait, "until"),
            ["idle", "working", "blocked", "done", "unknown", "suspended"]
        );
        assert!(has_option(wait, "timeout"));
    }

    #[test]
    fn spec_matches_refactored_agent_and_pane_commands() {
        let cmd = super::command();
        assert!(cmd
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "wait"));

        let agent = command_path(&cmd, &["agent"]);
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "send-keys"));
        assert!(agent
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait"));
        assert!(agent
            .get_subcommands()
            .all(|subcommand| subcommand.get_name() != "send"));

        let pane = command_path(&cmd, &["pane"]);
        assert!(pane
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == "wait-output"));
    }

    #[test]
    fn spec_includes_pane_read_raw_flag() {
        let cmd = super::command();
        let pane_read = command_path(&cmd, &["pane", "read"]);
        assert!(has_option(pane_read, "raw"));
    }

    #[test]
    fn spec_matches_pane_split_direction_flag() {
        let cmd = super::command();
        let pane_split = command_path(&cmd, &["pane", "split"]);
        assert!(has_option(pane_split, "direction"));
        assert!(!has_option(pane_split, "split"));
        assert_eq!(option_values(pane_split, "direction"), ["right", "down"]);
    }

    #[test]
    fn spec_models_agent_start_target_and_trailing_args() {
        let cmd = super::command();
        let agent_start = command_path(&cmd, &["agent", "start"]);
        assert!(has_option(agent_start, "kind"));
        assert_eq!(
            option_values(agent_start, "kind"),
            crate::detect::Agent::ALL
                .map(crate::detect::agent_label)
                .map(str::to_string)
        );
        assert!(has_option(agent_start, "pane"));
        for legacy in ["cwd", "workspace", "tab", "split", "focus", "env", "argv"] {
            assert!(!has_option(agent_start, legacy), "legacy option --{legacy}");
        }
        assert!(agent_start
            .get_arguments()
            .any(|arg| arg.get_id() == "agent_args"));
    }

    #[test]
    fn spec_models_fork_agent_wrap_and_notify() {
        let cmd = super::command();
        let wrap = command_path(&cmd, &["agent", "wrap"]);
        assert!(has_option(wrap, "print"));
        assert!(wrap.get_arguments().any(|arg| arg.get_id() == "args"));
        let notify = command_path(&cmd, &["agent", "notify"]);
        assert!(has_option(notify, "body"));
        assert_eq!(
            option_values(notify, "kind"),
            ["info", "question", "done", "warning"]
        );
        command_path(&cmd, &["browser", "wrap"]);
    }

    #[test]
    fn spec_models_agent_suspend_and_activate_targets() {
        let cmd = super::command();
        for name in ["suspend", "activate", "restart"] {
            let command = command_path(&cmd, &["agent", name]);
            let target = command
                .get_arguments()
                .find(|arg| arg.get_id() == "target")
                .unwrap_or_else(|| panic!("agent {name} is missing its TARGET argument"));
            assert!(target.is_required_set(), "agent {name} target is optional");
            assert!(!has_option(command, "pane"), "agent {name} takes no --pane");
            assert!(!has_option(command, "kind"), "agent {name} takes no --kind");
        }
        let agent_wait = command_path(&cmd, &["agent", "wait"]);
        assert!(option_values(agent_wait, "until").contains(&"suspended".to_string()));
    }

    #[test]
    fn spec_models_tab_color_values() {
        let cmd = super::command();
        let color = command_path(&cmd, &["tab", "color"]);
        let values: Vec<String> = color
            .get_arguments()
            .find(|arg| arg.get_id() == "color")
            .expect("tab color takes a COLOR argument")
            .get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .map(|value| value.get_name().to_string())
            .collect();
        let mut expected: Vec<String> = crate::api::schema::TabColor::ALL
            .iter()
            .map(|color| color.name().to_string())
            .collect();
        expected.push("none".into());
        assert_eq!(values, expected);
    }

    #[test]
    fn spec_models_tab_closed_and_reopen() {
        let cmd = super::command();
        let closed = command_path(&cmd, &["tab", "closed"]);
        assert!(has_option(closed, "json"));
        let reopen = command_path(&cmd, &["tab", "reopen"]);
        assert!(reopen
            .get_arguments()
            .any(|arg| arg.get_id() == "session" && arg.is_required_set()));
    }

    #[test]
    fn spec_models_team_verbs() {
        let cmd = super::command();
        let team = command_path(&cmd, &["team"]);
        let mut verbs: Vec<_> = team.get_subcommands().map(|c| c.get_name()).collect();
        verbs.sort_unstable();
        assert_eq!(
            verbs,
            ["disband", "get", "hook", "join", "leave", "list", "make", "purpose", "role"]
        );
        assert!(has_option(command_path(&cmd, &["team", "make"]), "purpose"));
        assert!(has_option(command_path(&cmd, &["team", "join"]), "role"));
        assert!(argument(command_path(&cmd, &["team", "leave"]), "pane").is_required_set());
    }

    #[test]
    fn spec_models_news_run_status_and_log() {
        let cmd = super::command();
        assert!(command_path(&cmd, &["news", "run"])
            .get_arguments()
            .next()
            .is_none());
        assert!(has_option(command_path(&cmd, &["news", "status"]), "json"));
        let log = command_path(&cmd, &["news", "log"]);
        assert!(has_option(log, "json"));
        assert!(log
            .get_arguments()
            .any(|arg| arg.get_id() == "count" && !arg.is_required_set()));
    }

    #[test]
    fn spec_models_news_open_history_enable_and_disable() {
        let cmd = super::command();
        assert!(has_option(command_path(&cmd, &["news", "open"]), "edition"));
        let history = command_path(&cmd, &["news", "history"]);
        assert!(has_option(history, "days"));
        assert!(has_option(history, "json"));
        for name in ["enable", "disable"] {
            assert!(command_path(&cmd, &["news", name])
                .get_arguments()
                .next()
                .is_none());
        }
    }

    #[test]
    fn spec_models_news_times() {
        let cmd = super::command();
        let times = command_path(&cmd, &["news", "times"]);
        assert!(has_option(times, "clear"));
        assert!(times
            .get_arguments()
            .any(|arg| arg.get_id() == "times" && !arg.is_required_set()));
    }

    #[test]
    fn spec_models_notes_verbs() {
        let cmd = super::command();
        assert!(has_option(command_path(&cmd, &["notes", "read"]), "json"));
        for verb in ["read", "path", "append", "write"] {
            let sub = command_path(&cmd, &["notes", verb]);
            for target in ["tab", "pane", "key"] {
                assert!(has_option(sub, target), "notes {verb} --{target}");
            }
        }
        let append = command_path(&cmd, &["notes", "append"]);
        assert!(has_option(append, "section"));
        assert!(has_option(append, "stamp"));
        assert!(append
            .get_arguments()
            .any(|arg| arg.get_id() == "text" && !arg.is_required_set()));
        let write = command_path(&cmd, &["notes", "write"]);
        assert!(has_option(write, "base"));
        assert!(has_option(write, "file"));
        let _ = command_path(&cmd, &["notes", "hook"]);
    }

    #[test]
    fn spec_models_checkpoint_verbs() {
        let cmd = super::command();
        let add = command_path(&cmd, &["checkpoint", "add"]);
        assert_eq!(
            add.get_arguments()
                .find(|arg| arg.get_id() == "kind")
                .unwrap()
                .get_possible_values()
                .iter()
                .map(|value| value.get_name().to_owned())
                .collect::<Vec<_>>(),
            ["decision", "milestone", "failure", "bookmark", "note"]
        );
        assert!(has_option(add, "detail"));
        assert!(has_option(add, "tag"));
        assert!(has_option(add, "as"));
        let list = command_path(&cmd, &["checkpoint", "list"]);
        assert!(has_option(list, "kind"));
        assert!(has_option(list, "limit"));
        assert!(has_option(list, "json"));
        assert!(has_option(
            command_path(&cmd, &["checkpoint", "show"]),
            "chars"
        ));
        for verb in ["show", "rm", "edit"] {
            assert!(command_path(&cmd, &["checkpoint", verb])
                .get_arguments()
                .any(|arg| arg.get_id() == "id" && arg.is_required_set()));
        }
        let edit = command_path(&cmd, &["checkpoint", "edit"]);
        for option in ["title", "detail", "kind", "tag"] {
            assert!(has_option(edit, option), "checkpoint edit --{option}");
        }
    }

    #[test]
    fn spec_models_browser_verbs_and_lifecycle() {
        let cmd = super::command();
        let open = command_path(&cmd, &["browser", "open"]);
        assert!(open
            .get_arguments()
            .any(|arg| arg.get_id() == "url" && arg.is_required_set()));
        assert!(has_option(open, "profile"));
        assert!(has_option(open, "json"));
        let read = command_path(&cmd, &["browser", "read"]);
        assert!(has_option(read, "offset"));
        assert!(has_option(read, "format"));
        assert!(read
            .get_arguments()
            .any(|arg| arg.get_id() == "tab" && !arg.is_required_set()));
        assert!(has_option(command_path(&cmd, &["browser", "wait"]), "text"));
        assert!(has_option(command_path(&cmd, &["browser", "log"]), "pane"));
        assert!(has_option(
            command_path(&cmd, &["browser", "setup"]),
            "node"
        ));
        assert!(has_option(command_path(&cmd, &["browser", "click"]), "ref"));
        assert!(command_path(&cmd, &["browser", "type"])
            .get_arguments()
            .any(|arg| arg.get_id() == "text" && arg.is_required_set()));
        assert!(has_option(
            command_path(&cmd, &["browser", "batch"]),
            "file"
        ));
        command_path(&cmd, &["browser", "profile", "delete"]);
        command_path(&cmd, &["browser", "mcp"]);
        command_path(&cmd, &["browser", "doctor"]);
    }

    #[test]
    fn spec_models_tab_remind_values() {
        let cmd = super::command();
        let values = |path: &[&str], id: &str| -> Vec<String> {
            command_path(&cmd, path)
                .get_arguments()
                .find(|arg| arg.get_id() == id)
                .expect("argument")
                .get_value_parser()
                .possible_values()
                .into_iter()
                .flatten()
                .map(|value| value.get_name().to_string())
                .collect()
        };
        let mut expected = vec!["off".to_string()];
        expected.extend(
            crate::api::schema::TabRemindInterval::ALL
                .iter()
                .map(|interval| interval.name().to_string()),
        );
        assert_eq!(values(&["tab", "remind"], "every"), expected);
        assert_eq!(values(&["tab", "important"], "state"), ["on", "off"]);
    }

    #[test]
    fn spec_models_agent_transcripts_as_a_local_listing() {
        let cmd = super::command();
        let transcripts = command_path(&cmd, &["agent", "transcripts"]);
        assert!(
            !transcripts
                .get_arguments()
                .any(|arg| arg.get_id() == "target"),
            "agent transcripts takes no target"
        );
        assert!(has_option(transcripts, "json"));
        assert!(!has_option(transcripts, "pane"));
        assert!(long_help(&["agent", "transcripts"]).contains("agent-transcripts/"));
    }

    fn long_help(path: &[&str]) -> String {
        let mut args = vec!["herdr".to_string()];
        args.extend(path.iter().map(|segment| segment.to_string()));
        args.push("--help".to_string());
        let mut output = Vec::new();
        assert!(
            super::write_requested_help(&args, &mut output, || {}).unwrap(),
            "help was not handled for herdr {}",
            path.join(" ")
        );
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn agent_resources_appear_on_command_groups_but_not_leaf_commands() {
        for group in ["agent", "pane", "workspace", "terminal"] {
            let help = long_help(&[group]);
            assert!(
                help.contains(super::super::AGENT_HELP_FOOTER),
                "herdr {group} is missing agent resources: {help}"
            );
        }

        let leaf = long_help(&["agent", "wait"]);
        assert!(
            !leaf.contains(super::super::AGENT_HELP_FOOTER),
            "leaf help should stay focused: {leaf}"
        );
    }

    #[test]
    fn next_step_hints_render_without_replacing_existing_after_help() {
        let agent_start = long_help(&["agent", "start"]);
        assert!(
            agent_start.contains("The pane must be at its interactive shell prompt."),
            "agent start dropped its existing after_help: {agent_start}"
        );
        assert!(
            agent_start.contains("next: herdr agent prompt <TARGET> <TEXT> --wait"),
            "agent start is missing its next-step hint: {agent_start}"
        );

        let pane_send_text = long_help(&["pane", "send-text"]);
        assert!(
            pane_send_text.contains(
                "next: herdr pane run <PANE_ID> <COMMAND> sends text and Enter in one call"
            ),
            "pane send-text is missing its next-step hint: {pane_send_text}"
        );
    }

    #[test]
    fn completion_generation_succeeds_for_every_supported_shell() {
        for shell in [
            clap_complete::Shell::Bash,
            clap_complete::Shell::Elvish,
            clap_complete::Shell::Fish,
            clap_complete::Shell::PowerShell,
            clap_complete::Shell::Zsh,
        ] {
            let mut cmd = super::command();
            let mut output = Vec::new();
            clap_complete::generate(shell, &mut cmd, "herdr", &mut output);
            assert!(!output.is_empty(), "empty {shell:?} completion output");
        }
    }
}
