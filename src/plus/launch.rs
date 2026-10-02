//! Launch argv for herdr+ agents: the flags that give a Claude Code or Codex
//! session the `herdr plus mcp` server for this launch only (nothing is
//! written to the user's global agent configs), its permission allowlist and
//! its kickoff prompt. Pure builders apart from the Claude MCP config file.
//!
//! The pane's shell hook may wrap `claude`/`codex` (`herdr browser wrap`): it
//! adds `--no-daemon` to Codex and inserts its own Claude flags before a user
//! `--`, so the argv here never repeats those and always ends Claude's flags
//! with `--`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use super::{instructions_path, mcp_dir, write_atomically, DEFAULT_PORT};

/// The MCP server key everywhere (Codex `-c` takes a dotted path: no dash).
pub const MCP_KEY: &str = "herdr_plus";
/// Claude allowlist for a managed agent: every herdr_plus tool.
pub const CLAUDE_ALLOW_AGENT: &str = "mcp__herdr_plus";
/// Claude allowlist for the coordinator.
pub const CLAUDE_ALLOW_COORDINATOR: &str = "mcp__herdr_plus,mcp__herdr-browser,Bash(herdr plus:*)";
/// `1` adds `--append-system-prompt-file=<dir>/coordinator.md` to the coordinator.
pub const SYSPROMPT_FILE_ENV: &str = "HERDR_PLUS_SYSPROMPT_FILE";
/// `1` adds `--no-daemon` to Codex (when no shell hook adds it).
pub const CODEX_NO_DAEMON_ENV: &str = "HERDR_PLUS_CODEX_NO_DAEMON";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCtx {
    /// The herdr binary the MCP server runs from (`current_exe`).
    pub herdr_bin: PathBuf,
    /// The absolute herdr+ directory, passed to every MCP server as `--dir`.
    pub dir: PathBuf,
    /// The dashboard port; passed to the MCP server as `--port` when not the default.
    pub port: u16,
}

impl LaunchCtx {
    /// The running herdr binary with this directory and port.
    pub fn current(dir: PathBuf, port: u16) -> io::Result<Self> {
        Ok(Self {
            herdr_bin: std::env::current_exe()?,
            dir,
            port,
        })
    }

    /// `herdr plus mcp` arguments.
    fn mcp_args(&self) -> Vec<String> {
        let mut args = vec![
            "plus".to_string(),
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
    let config = write_claude_mcp_config(ctx)?;
    Ok(claude_argv(
        ctx,
        &config,
        session,
        coordinator,
        kickoff,
        env_on(SYSPROMPT_FILE_ENV),
    ))
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
    args.push(format!("--mcp-config={}", config.display()));
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
fn toml_string(value: &str) -> String {
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
/// sandbox, and the herdr_plus MCP server for this launch only.
pub fn codex_args(ctx: &LaunchCtx, kickoff: Option<&str>) -> Vec<String> {
    codex_argv(ctx, kickoff, env_on(CODEX_NO_DAEMON_ENV))
}

fn codex_argv(ctx: &LaunchCtx, kickoff: Option<&str>, no_daemon: bool) -> Vec<String> {
    let key = format!("mcp_servers.{MCP_KEY}");
    let mut args: Vec<String> = vec![
        "-a".into(),
        "never".into(),
        "-s".into(),
        "workspace-write".into(),
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
        // Under `-a never` Codex rejects an MCP call that needs approval
        // ("requires approval, but approval policy is never"); pre-approve
        // this one server, like Claude's `--allowedTools=mcp__herdr_plus`.
        "-c".into(),
        format!("{key}.default_tools_approval_mode=\"approve\""),
    ];
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
        "You are {name}, a herdr+ managed agent{tags}. Call plus_whoami once to see the herdr+ tools and etiquette. {task}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ctx() -> LaunchCtx {
        LaunchCtx {
            herdr_bin: PathBuf::from("/opt/herdr \"dev\"/herdr"),
            dir: PathBuf::from("/home/u/.config/herdr-dev/plus"),
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
                "--mcp-config=/home/u/.config/herdr-dev/plus/mcp/claude.json",
                "--allowedTools=mcp__herdr_plus",
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
                "--mcp-config=/home/u/.config/herdr-dev/plus/mcp/claude.json",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools=mcp__herdr_plus,mcp__herdr-browser,Bash(herdr plus:*)",
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
                "--append-system-prompt-file=/home/u/.config/herdr-dev/plus/coordinator.md",
                "--",
                "go"
            ]
        );
        // Every allowlist entry starts with the MCP key the configs register.
        assert!(CLAUDE_ALLOW_AGENT == format!("mcp__{MCP_KEY}"));
        assert!(CLAUDE_ALLOW_COORDINATOR.starts_with(CLAUDE_ALLOW_AGENT));
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
                "plus",
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
            let key = key.strip_prefix("mcp_servers.herdr_plus.").unwrap();
            let table: toml::Table = toml::from_str(&format!("{key} = {raw}")).unwrap();
            parsed.extend(table);
        }
        assert_eq!(parsed["command"].as_str(), Some("/opt/herdr \"dev\"/herdr"));
        assert_eq!(
            parsed["args"],
            toml::Value::Array(
                ["plus", "mcp", "--dir", "/home/u/.config/herdr-dev/plus"]
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
            "You are rev, a herdr+ managed agent (role reviewer, project demo). Call plus_whoami once to see the herdr+ tools and etiquette. Review lead's branch."
        );
        assert_eq!(
            agent_kickoff(dir, "lead", None, None, None),
            "You are lead, a herdr+ managed agent. Call plus_whoami once to see the herdr+ tools and etiquette. Wait for the user's instructions."
        );
    }
}
