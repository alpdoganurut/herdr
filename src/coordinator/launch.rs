//! Launch argv for managed agents: the flags that give a Claude Code or Codex
//! session the `herdr coordinator mcp` server for this launch only (nothing is
//! written to the user's global agent configs), its permission allowlist and
//! its kickoff prompt. Pure builders apart from the Claude MCP config file.
//!
//! The pane's shell hook may wrap `claude`/`codex` (`herdr agent wrap`): it
//! adds `--no-daemon` to Codex and inserts its own Claude flags before a user
//! `--`, so the argv here never repeats those and always ends Claude's flags
//! with `--`. The wrap recognises these launches as managed (by the MCP
//! config path, the allowlist or the `-c mcp_servers.herdr_agents.*`
//! overrides) and adds none of its own herdr_agents arguments to them; it
//! shares [`mcp_config_flag`], [`write_claude_mcp_config`] and
//! [`codex_mcp_overrides`] for unmanaged ones.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{instructions_path, mcp_dir, write_atomically, DEFAULT_PORT};
use crate::agent_wrap::team::TeamLaunch;

/// The MCP server key everywhere (Codex `-c` takes a dotted path: no dash).
pub const MCP_KEY: &str = "herdr_agents";
/// Claude allowlist for a managed agent: every herdr_agents tool.
pub const CLAUDE_ALLOW_AGENT: &str = "mcp__herdr_agents";
/// Claude allowlist for the coordinator: the herdr_agents and browser tools and
/// the read-only `herdr coordinator` verbs. The CLI's write verbs (manage, unmanage,
/// wake, start, clear-turn) are not pre-approved: they would get
/// around the MCP tools' non-user-turn guard without a prompt.
pub const CLAUDE_ALLOW_COORDINATOR: &str = "mcp__herdr_agents,mcp__herdr-browser,Bash(herdr coordinator status:*),Bash(herdr coordinator messages:*)";
/// `1` adds `--append-system-prompt-file=<dir>/coordinator.md` to the coordinator.
pub const SYSPROMPT_FILE_ENV: &str = "HERDR_COORDINATOR_SYSPROMPT_FILE";
/// `1` adds `--no-daemon` to Codex (when no shell hook adds it).
pub const CODEX_NO_DAEMON_ENV: &str = "HERDR_COORDINATOR_CODEX_NO_DAEMON";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCtx {
    /// The herdr binary the MCP server runs from (`current_exe`).
    pub herdr_bin: PathBuf,
    /// The absolute coordinator directory, passed to every MCP server as `--dir`.
    pub dir: PathBuf,
    /// The herdr server's `[coordinator] dashboard_port` (`0` = not served);
    /// passed to the MCP server as `--port` when not the default, so
    /// `agents_whoami` prints the URL the server actually uses and agents the
    /// MCP server starts inherit it.
    pub port: u16,
}

impl LaunchCtx {
    /// The running herdr binary with this directory and the configured
    /// dashboard port.
    pub fn current(dir: PathBuf, port: u16) -> io::Result<Self> {
        Ok(Self {
            herdr_bin: std::env::current_exe()?,
            dir,
            port,
        })
    }

    /// `herdr coordinator mcp` arguments.
    fn mcp_args(&self) -> Vec<String> {
        let mut args = vec![
            "coordinator".to_string(),
            "mcp".to_string(),
            "--dir".to_string(),
            self.dir.to_string_lossy().into_owned(),
        ];
        if self.port != DEFAULT_PORT {
            args.push("--port".into());
            args.push(self.port.to_string());
        }
        args
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeSession {
    New(String),
    Resume(String),
}

static UUID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A random (version 4) UUID, std only: two independently seeded hashers over
/// the time, the pid and a process-wide counter.
pub fn new_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = UUID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let half = |salt: u64| {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(count);
        hasher.write_u64(salt);
        hasher.finish()
    };
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&half(1).to_le_bytes());
    bytes[8..].copy_from_slice(&half(2).to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

pub fn claude_mcp_config_path(dir: &Path) -> PathBuf {
    mcp_dir(dir).join("claude.json")
}

/// Write `<dir>/mcp/claude.json` (idempotent) and return its path.
pub fn write_claude_mcp_config(ctx: &LaunchCtx) -> io::Result<PathBuf> {
    let config = json!({
        "mcpServers": {
            MCP_KEY: {
                "type": "stdio",
                "command": ctx.herdr_bin.to_string_lossy(),
                "args": ctx.mcp_args(),
            }
        }
    });
    let path = claude_mcp_config_path(&ctx.dir);
    let mut body = serde_json::to_vec_pretty(&config).map_err(io::Error::other)?;
    body.push(b'\n');
    write_atomically(&path, &body)?;
    Ok(path)
}

/// `--mcp-config=<path>`: the variadic flag in its `=` form, so it never
/// swallows what follows.
pub fn mcp_config_flag(path: &Path) -> String {
    format!("--mcp-config={}", path.display())
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value.trim() == "1")
}

/// Claude Code argv after the executable. Writes the MCP config it points at.
pub fn claude_args(
    ctx: &LaunchCtx,
    session: &ClaudeSession,
    coordinator: bool,
    kickoff: Option<&str>,
) -> io::Result<Vec<String>> {
    claude_args_with_team(ctx, session, coordinator, kickoff, None)
}

/// [`claude_args`] for a team member: also the team's hook settings
/// (`--settings=<dir>/team/claude-settings.json`, written when it differs)
/// and the roster as its system prompt addition. `None` is [`claude_args`].
pub fn claude_args_with_team(
    ctx: &LaunchCtx,
    session: &ClaudeSession,
    coordinator: bool,
    kickoff: Option<&str>,
    team: Option<&TeamLaunch>,
) -> io::Result<Vec<String>> {
    let config = write_claude_mcp_config(ctx)?;
    let mut args = claude_argv(
        ctx,
        &config,
        session,
        coordinator,
        kickoff,
        env_on(SYSPROMPT_FILE_ENV),
    );
    if let Some(team) = team {
        let settings = crate::agent_wrap::team::write_claude_settings(ctx)?;
        insert_before_dashes(&mut args, claude_team_flags(&settings, team));
    }
    Ok(args)
}

/// The team flags of a managed Claude launch (before its `--`).
fn claude_team_flags(settings: &Path, team: &TeamLaunch) -> Vec<String> {
    vec![
        crate::agent_wrap::team::settings_flag(settings),
        "--append-system-prompt".into(),
        team.text.clone(),
    ]
}

/// `flags` inserted before the first `--` of `args` (at the end without one).
fn insert_before_dashes(args: &mut Vec<String>, flags: Vec<String>) {
    let at = args.iter().position(|a| a == "--").unwrap_or(args.len());
    args.splice(at..at, flags);
}

fn claude_argv(
    ctx: &LaunchCtx,
    config: &Path,
    session: &ClaudeSession,
    coordinator: bool,
    kickoff: Option<&str>,
    sysprompt_file: bool,
) -> Vec<String> {
    let mut args = match session {
        ClaudeSession::New(id) => vec!["--session-id".to_string(), id.clone()],
        ClaudeSession::Resume(id) => vec!["--resume".to_string(), id.clone()],
    };
    // Variadic flags take the `=` form so they never swallow what follows.
    args.push(mcp_config_flag(config));
    if coordinator {
        args.push("--permission-mode".into());
        args.push("acceptEdits".into());
        args.push(format!("--allowedTools={CLAUDE_ALLOW_COORDINATOR}"));
        if sysprompt_file {
            args.push(format!(
                "--append-system-prompt-file={}",
                instructions_path(&ctx.dir).display()
            ));
        }
    } else {
        args.push(format!("--allowedTools={CLAUDE_ALLOW_AGENT}"));
    }
    args.push("--".into());
    if let Some(kickoff) = kickoff {
        args.push(kickoff.to_string());
    }
    args
}

/// A TOML basic string (`"..."`), for Codex `-c key=value` overrides.
pub(crate) fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn toml_array<S: AsRef<str>>(items: &[S]) -> String {
    let items: Vec<String> = items
        .iter()
        .map(|item| toml_string(item.as_ref()))
        .collect();
    format!("[{}]", items.join(","))
}

/// Codex argv after the executable: no approval prompts, a workspace-write
/// sandbox, and the herdr_agents MCP server for this launch only.
pub fn codex_args(ctx: &LaunchCtx, kickoff: Option<&str>) -> Vec<String> {
    codex_argv(ctx, kickoff, env_on(CODEX_NO_DAEMON_ENV))
}

/// [`codex_args`] for a team member: the roster in `developer_instructions`
/// after the user's own (Codex's `config.toml`). `None` is [`codex_args`].
pub fn codex_args_with_team(
    ctx: &LaunchCtx,
    kickoff: Option<&str>,
    team: Option<&TeamLaunch>,
) -> Vec<String> {
    let mut args = codex_args(ctx, kickoff);
    if let Some(team) = team {
        let own = crate::agent_wrap::codex_developer_instructions();
        codex_add_team(&mut args, own.as_deref(), team, kickoff.is_some());
    }
    args
}

/// `-c developer_instructions=<own ⧺ team text>` before the kickoff prompt.
fn codex_add_team(args: &mut Vec<String>, own: Option<&str>, team: &TeamLaunch, kickoff: bool) {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(own) = own.map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(own);
    }
    parts.push(team.text.trim());
    let at = if kickoff {
        args.len().saturating_sub(1)
    } else {
        args.len()
    };
    args.splice(
        at..at,
        [
            "-c".to_string(),
            format!(
                "developer_instructions={}",
                toml_string(&parts.join("\n\n"))
            ),
        ],
    );
}

/// A team member's kickoff: the roster block first, then the usual kickoff.
pub fn team_kickoff(team_text: &str, kickoff: &str) -> String {
    format!("{}\n\n{kickoff}", team_text.trim())
}

/// The `-c mcp_servers.herdr_agents.*` overrides that give a Codex launch
/// the herdr_agents server: command, args, the forwarded pane variables and
/// the tool timeout; `approve_all` also pre-approves every tool of the
/// server (managed launches run under `-a never`).
pub fn codex_mcp_overrides(ctx: &LaunchCtx, approve_all: bool) -> Vec<String> {
    let key = format!("mcp_servers.{MCP_KEY}");
    let mut args: Vec<String> = vec![
        "-c".into(),
        format!(
            "{key}.command={}",
            toml_string(&ctx.herdr_bin.to_string_lossy())
        ),
        "-c".into(),
        format!("{key}.args={}", toml_array(&ctx.mcp_args())),
        "-c".into(),
        format!(
            "{key}.env_vars={}",
            toml_array(&crate::browser::setup::CODEX_FORWARDED_ENV)
        ),
        "-c".into(),
        format!("{key}.tool_timeout_sec=150"),
    ];
    if approve_all {
        // Under `-a never` Codex rejects an MCP call that needs approval
        // ("requires approval, but approval policy is never"); pre-approve
        // this one server, like Claude's `--allowedTools=mcp__herdr_agents`.
        args.push("-c".into());
        args.push(format!("{key}.default_tools_approval_mode=\"approve\""));
    }
    args
}

fn codex_argv(ctx: &LaunchCtx, kickoff: Option<&str>, no_daemon: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-a".into(),
        "never".into(),
        "-s".into(),
        "workspace-write".into(),
    ];
    args.extend(codex_mcp_overrides(ctx, true));
    if no_daemon {
        args.push("--no-daemon".into());
    }
    if let Some(kickoff) = kickoff {
        args.push(kickoff.to_string());
    }
    args
}

pub fn coordinator_kickoff(dir: &Path) -> String {
    format!(
        "You are the herdr+ coordinator agent. Read {} now and follow it exactly (file tools only, no shell commands). Then do the \"On start\" steps.",
        instructions_path(dir).display()
    )
}

pub fn agent_kickoff(
    _dir: &Path,
    name: &str,
    role: Option<&str>,
    project: Option<&str>,
    task: Option<&str>,
) -> String {
    let mut tags = Vec::new();
    if let Some(role) = role.filter(|r| !r.trim().is_empty()) {
        tags.push(format!("role {}", role.trim()));
    }
    if let Some(project) = project.filter(|p| !p.trim().is_empty()) {
        tags.push(format!("project {}", project.trim()));
    }
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!(" ({})", tags.join(", "))
    };
    let task = task
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Wait for the user's instructions.");
    format!(
        "You are {name}, a herdr+ managed agent{tags}. Call agents_whoami once to see the agent tools and etiquette. {task}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ctx() -> LaunchCtx {
        LaunchCtx {
            herdr_bin: PathBuf::from("/opt/herdr \"dev\"/herdr"),
            dir: PathBuf::from("/home/u/.config/herdr-dev/coordinator"),
            port: DEFAULT_PORT,
        }
    }

    fn is_uuid_v4(value: &str) -> bool {
        let parts: Vec<&str> = value.split('-').collect();
        let hex = |s: &str| s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        parts.len() == 5
            && [8, 4, 4, 4, 12]
                .iter()
                .zip(&parts)
                .all(|(len, part)| part.len() == *len && hex(part))
            && parts[2].starts_with('4')
            && parts[3].starts_with(['8', '9', 'a', 'b'])
    }

    #[test]
    fn uuids_are_v4_shaped_and_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = new_uuid();
            assert!(is_uuid_v4(&id), "{id}");
            assert!(seen.insert(id));
        }
        assert!(!is_uuid_v4("4f1e9a2c-0000-1000-8000-000000000000"));
    }

    #[test]
    fn claude_args_use_eq_forms_and_end_flags_with_a_double_dash() {
        let ctx = ctx();
        let config = claude_mcp_config_path(&ctx.dir);
        let agent = claude_argv(
            &ctx,
            &config,
            &ClaudeSession::New("u1".into()),
            false,
            Some("--hello"),
            true,
        );
        assert_eq!(
            agent,
            [
                "--session-id",
                "u1",
                "--mcp-config=/home/u/.config/herdr-dev/coordinator/mcp/claude.json",
                "--allowedTools=mcp__herdr_agents",
                "--",
                "--hello",
            ]
        );
        let coordinator = claude_argv(
            &ctx,
            &config,
            &ClaudeSession::Resume("u2".into()),
            true,
            None,
            false,
        );
        assert_eq!(
            coordinator,
            [
                "--resume",
                "u2",
                "--mcp-config=/home/u/.config/herdr-dev/coordinator/mcp/claude.json",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools=mcp__herdr_agents,mcp__herdr-browser,Bash(herdr coordinator status:*),Bash(herdr coordinator messages:*)",
                "--",
            ]
        );
        let with_file = claude_argv(
            &ctx,
            &config,
            &ClaudeSession::New("u3".into()),
            true,
            Some("go"),
            true,
        );
        assert_eq!(
            &with_file[with_file.len() - 3..],
            [
                "--append-system-prompt-file=/home/u/.config/herdr-dev/coordinator/coordinator.md",
                "--",
                "go"
            ]
        );
        // Every allowlist entry starts with the MCP key the configs register.
        assert!(CLAUDE_ALLOW_AGENT == format!("mcp__{MCP_KEY}"));
        assert!(CLAUDE_ALLOW_COORDINATOR.starts_with(CLAUDE_ALLOW_AGENT));
        // Only read-only CLI verbs are pre-approved for the coordinator.
        for entry in CLAUDE_ALLOW_COORDINATOR.split(',') {
            if let Some(command) = entry.strip_prefix("Bash(") {
                assert!(
                    ["herdr coordinator status:", "herdr coordinator messages:"]
                        .iter()
                        .any(|read| command.starts_with(read)),
                    "{entry}"
                );
            }
        }
    }

    #[test]
    fn claude_mcp_config_names_this_binary_and_directory() {
        let dir = super::super::test_dir("launch");
        let ctx = LaunchCtx {
            herdr_bin: PathBuf::from("/bin/herdr"),
            dir: dir.clone(),
            port: 7719,
        };
        let args = claude_args(&ctx, &ClaudeSession::New("u".into()), false, None).unwrap();
        let path = write_claude_mcp_config(&ctx).unwrap();
        assert!(args.contains(&format!("--mcp-config={}", path.display())));
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let server = &config["mcpServers"][MCP_KEY];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/bin/herdr");
        assert_eq!(
            server["args"],
            json!([
                "coordinator",
                "mcp",
                "--dir",
                dir.to_string_lossy(),
                "--port",
                "7719"
            ])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_args_carry_the_configured_dashboard_port() {
        let port_args = |port: u16| {
            let args = LaunchCtx { port, ..ctx() }.mcp_args();
            args.iter()
                .position(|a| a == "--port")
                .map(|at| args[at + 1].clone())
        };
        assert_eq!(port_args(DEFAULT_PORT), None, "the default stays implicit");
        assert_eq!(port_args(7728).as_deref(), Some("7728"));
        // Serving off: the MCP server must not print the default URL.
        assert_eq!(port_args(0).as_deref(), Some("0"));
    }

    #[test]
    fn codex_args_are_valid_toml_overrides_without_duplicate_daemon_flags() {
        let ctx = ctx();
        let args = codex_argv(&ctx, Some("You are rev"), false);
        assert_eq!(&args[..4], ["-a", "never", "-s", "workspace-write"]);
        assert!(!args
            .iter()
            .any(|a| a == "--full-auto" || a == "--no-daemon"));
        assert_eq!(args.last().map(String::as_str), Some("You are rev"));
        let overrides: Vec<&String> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(flag, _)| *flag == "-c")
            .map(|(_, value)| value)
            .collect();
        assert_eq!(overrides.len(), 5);
        let mut parsed = toml::Table::new();
        for value in overrides {
            let (key, raw) = value.split_once('=').unwrap();
            let key = key.strip_prefix("mcp_servers.herdr_agents.").unwrap();
            let table: toml::Table = toml::from_str(&format!("{key} = {raw}")).unwrap();
            parsed.extend(table);
        }
        assert_eq!(parsed["command"].as_str(), Some("/opt/herdr \"dev\"/herdr"));
        assert_eq!(
            parsed["args"],
            toml::Value::Array(
                [
                    "coordinator",
                    "mcp",
                    "--dir",
                    "/home/u/.config/herdr-dev/coordinator"
                ]
                .iter()
                .map(|s| toml::Value::String((*s).into()))
                .collect()
            )
        );
        let env_vars: Vec<&str> = parsed["env_vars"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(env_vars, crate::browser::setup::CODEX_FORWARDED_ENV);
        assert_eq!(parsed["tool_timeout_sec"].as_integer(), Some(150));
        assert_eq!(
            parsed["default_tools_approval_mode"].as_str(),
            Some("approve")
        );

        let with_daemon = codex_argv(&ctx, None, true);
        assert_eq!(
            with_daemon.iter().filter(|a| *a == "--no-daemon").count(),
            1
        );
        assert_eq!(with_daemon.last().map(String::as_str), Some("--no-daemon"));
        assert_eq!(toml_string("a\u{1}b\\"), "\"a\\u0001b\\\\\"");
    }

    #[test]
    fn kickoffs_name_the_directory_and_the_agent() {
        let dir = Path::new("/p");
        assert_eq!(
            coordinator_kickoff(dir),
            "You are the herdr+ coordinator agent. Read /p/coordinator.md now and follow it exactly (file tools only, no shell commands). Then do the \"On start\" steps."
        );
        assert_eq!(
            agent_kickoff(dir, "rev", Some("reviewer"), Some("demo"), Some("Review lead's branch.")),
            "You are rev, a herdr+ managed agent (role reviewer, project demo). Call agents_whoami once to see the agent tools and etiquette. Review lead's branch."
        );
        assert_eq!(
            agent_kickoff(dir, "lead", None, None, None),
            "You are lead, a herdr+ managed agent. Call agents_whoami once to see the agent tools and etiquette. Wait for the user's instructions."
        );
    }

    #[test]
    fn a_team_launch_adds_the_hook_settings_and_the_roster_before_the_dashes() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-launch-team-{}-{}",
            std::process::id(),
            new_uuid()
        ));
        let ctx = LaunchCtx {
            herdr_bin: PathBuf::from("/opt/herdr/herdr"),
            dir: dir.clone(),
            port: DEFAULT_PORT,
        };
        let team = TeamLaunch {
            text: "You are \"fixer\" (pane w3:p1) in a herdr+ team (group demo).".into(),
        };
        let session = ClaudeSession::New("u1".into());
        let plain = claude_args(&ctx, &session, false, Some("go")).unwrap();
        let args = claude_args_with_team(&ctx, &session, false, Some("go"), Some(&team)).unwrap();
        let settings = dir.join("team/claude-settings.json");
        assert!(settings.is_file());
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &args[dashes - 3..],
            [
                format!("--settings={}", settings.display()),
                "--append-system-prompt".to_string(),
                team.text.clone(),
                "--".to_string(),
                "go".to_string(),
            ]
        );
        // everything else is the plain launch
        let mut without = args.clone();
        without.drain(dashes - 3..dashes);
        assert_eq!(without, plain);
        assert_eq!(
            claude_args_with_team(&ctx, &session, false, Some("go"), None).unwrap(),
            plain
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_codex_team_launch_puts_the_roster_after_the_users_instructions() {
        let team = TeamLaunch {
            text: "You are \"reviewer\"".into(),
        };
        let mut args = codex_argv(&ctx(), Some("go"), false);
        let plain = args.clone();
        codex_add_team(&mut args, Some(" Be terse. "), &team, true);
        assert_eq!(args.last().map(String::as_str), Some("go"));
        assert_eq!(args[args.len() - 3], "-c");
        assert_eq!(
            args[args.len() - 2],
            format!(
                "developer_instructions={}",
                toml_string("Be terse.\n\nYou are \"reviewer\"")
            )
        );
        assert_eq!(args.len(), plain.len() + 2);
        let mut args = codex_argv(&ctx(), None, false);
        codex_add_team(&mut args, None, &team, false);
        assert_eq!(
            args.last().unwrap(),
            &format!(
                "developer_instructions={}",
                toml_string("You are \"reviewer\"")
            )
        );
        assert_eq!(
            codex_args_with_team(&ctx(), Some("go"), None),
            codex_args(&ctx(), Some("go"))
        );
        assert_eq!(
            team_kickoff(" ROSTER \n", "You are fixer."),
            "ROSTER\n\nYou are fixer."
        );
    }
}
