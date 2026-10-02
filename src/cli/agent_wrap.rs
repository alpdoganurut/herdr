//! `herdr agent wrap <claude|codex> [--print] [--] ARGS…` (fork): run the
//! agent with what `[agents]` adds (`crate::agent_wrap`). The shell hook's
//! functions call it for plain `claude` / `codex` in herdr+ panes;
//! `herdr browser wrap` is a permanent alias (older managed shell files call
//! it).

use std::path::{Path, PathBuf};

use crate::agent_wrap::{self, WrapEnv};
use crate::browser::setup::is_executable;

const USAGE: &str = "usage: herdr agent wrap <claude|codex> [--print] [--] ARGS…
Runs the agent with what [agents] adds: the herdr_agents tools (tools), the herdr+ paragraph (instructions) and,
from [browser], the browser steering and no built-in browser — only while [agents] wrap is on. Codex always gets
--no-daemon (attribution); claude runs through claude-z when it is on PATH.
Per launch: --no-herdr (before a `--`) or HERDR_NO_WRAP=1 runs the agent as typed; `command claude` skips herdr.
--print shows the binary, then one argument per line (newlines inside an argument as \\n), then warnings; nothing runs.";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Parsed {
    agent: &'static str,
    print: bool,
    user: Vec<String>,
}

/// `[--print] <agent> [--print] [--] ARGS…`; `Err(exit code)` after printing usage.
fn parse(args: &[String]) -> Result<Parsed, i32> {
    let mut print = false;
    let mut i = 0;
    while args.get(i).is_some_and(|a| a == "--print") {
        print = true;
        i += 1;
    }
    let agent = match args.get(i).map(String::as_str) {
        Some("claude") => "claude",
        Some("codex") => "codex",
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            return Err(0);
        }
        Some(other) => {
            eprintln!("unknown agent {other:?}\n{USAGE}");
            return Err(2);
        }
        None => {
            eprintln!("{USAGE}");
            return Err(2);
        }
    };
    i += 1;
    if args.get(i).is_some_and(|a| a == "--print") {
        print = true;
        i += 1;
    }
    if args.get(i).is_some_and(|a| a == "--") {
        i += 1;
    }
    Ok(Parsed {
        agent,
        print,
        user: args[i..].to_vec(),
    })
}

/// The binaries the wrap execs, first found on PATH wins.
fn binary_names(agent: &str) -> &'static [&'static str] {
    if agent == "claude" {
        &["claude-z", "claude"]
    } else {
        &["codex"]
    }
}

/// The first executable of `names` on PATH (a PATH lookup: the shell
/// functions of the hook are not seen here).
fn real_binary(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    names.iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    })
}

/// `--print`'s output.
fn render_print(
    binary: Option<&Path>,
    names: &[&str],
    argv: &[String],
    warnings: &[String],
) -> String {
    let mut out = match binary {
        Some(binary) => format!("{}\n", binary.display()),
        None => format!("(not on PATH: {})\n", names.join(", ")),
    };
    for arg in argv {
        out.push_str(&arg.replace('\n', "\\n"));
        out.push('\n');
    }
    for warning in warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    out
}

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    let agent = parsed.agent;
    let (user, opted_out) = agent_wrap::strip_opt_out(&parsed.user);
    let config = crate::config::Config::load().config;
    let env = WrapEnv::from_process(&config);
    let mut plan = agent_wrap::plan(&config, &env);
    if opted_out {
        plan.disable();
    }
    // the file the `--mcp-config=` flag points at (idempotent; --print writes nothing)
    if !parsed.print && agent == "claude" && agent_wrap::uses_claude_mcp_config(&plan, &user) {
        if let Err(err) = crate::coordinator::launch::write_claude_mcp_config(&plan.ctx) {
            tracing::warn!(event = "agent.wrap", %err, "cannot write the herdr_agents MCP config");
            plan.warnings.push(format!(
                "cannot write the herdr_agents MCP config ({err}); launching without the agent tools"
            ));
            plan.tools = false;
        }
    }
    let argv = agent_wrap::wrap_args(agent, &plan, &user);
    let names = binary_names(agent);
    let binary = real_binary(names);
    if parsed.print {
        print!(
            "{}",
            render_print(binary.as_deref(), names, &argv, &plan.warnings)
        );
        return Ok(0);
    }
    for warning in &plan.warnings {
        eprintln!("herdr agent wrap: {warning}");
    }
    let Some(binary) = binary else {
        eprintln!("herdr agent wrap: no {} on PATH", names.join(" or "));
        return Ok(127);
    };
    let mut command = std::process::Command::new(&binary);
    command.args(&argv);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = command.exec();
        eprintln!("herdr agent wrap: cannot run {}: {err}", binary.display());
        Ok(126)
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn the_verb_takes_print_before_the_double_dash_only() {
        assert_eq!(
            parse(&s(&["claude", "--", "--print", "x"])).unwrap(),
            Parsed {
                agent: "claude",
                print: false,
                user: s(&["--print", "x"])
            },
            "after `--` it is claude's own --print"
        );
        assert_eq!(
            parse(&s(&["--print", "codex", "--", "resume"])).unwrap(),
            Parsed {
                agent: "codex",
                print: true,
                user: s(&["resume"])
            }
        );
        assert_eq!(
            parse(&s(&["claude", "--print", "-p", "hi"])).unwrap(),
            Parsed {
                agent: "claude",
                print: true,
                user: s(&["-p", "hi"])
            }
        );
        assert_eq!(parse(&s(&["claude"])).unwrap().user, Vec::<String>::new());
        assert_eq!(parse(&s(&["pi"])), Err(2));
        assert_eq!(parse(&[]), Err(2));
        assert_eq!(parse(&s(&["--help"])), Err(0));
    }

    #[test]
    fn print_shows_the_binary_one_argument_per_line_and_the_warnings() {
        let out = render_print(
            Some(Path::new("/bin/claude-z")),
            &["claude-z", "claude"],
            &s(&["uuid", "--append-system-prompt", "a\n\nb"]),
            &["instructions file ~/x: missing; using the built-in text".to_string()],
        );
        assert_eq!(
            out,
            "/bin/claude-z\nuuid\n--append-system-prompt\na\\n\\nb\nwarning: instructions file ~/x: missing; using the built-in text\n"
        );
        assert_eq!(
            render_print(None, &["codex"], &s(&["--no-daemon"]), &[]),
            "(not on PATH: codex)\n--no-daemon\n"
        );
    }

    #[test]
    fn real_binaries_need_the_execute_bit() {
        let dir = std::env::temp_dir().join(format!("herdr-wrap-x-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        assert!(!is_executable(&plain), "a readable file is not enough");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(is_executable(&plain));
        }
        assert!(!is_executable(&dir));
        assert_eq!(binary_names("claude"), ["claude-z", "claude"]);
        assert_eq!(binary_names("codex"), ["codex"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
