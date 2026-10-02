//! Setup, checks and fixes of the browser feature (fork), shared by the CLI
//! (`herdr browser setup` / `doctor`) and the server methods behind the
//! settings overlay's browser section (`browser.settings`,
//! `browser.settings.set`, `browser.fix`), so the two cannot drift.
//!
//! Everything here works on an explicit [`SetupEnv`] (paths and binaries),
//! never on the process environment directly: the server resolves it once
//! per call with [`SetupEnv::from_process`], tests build one over temporary
//! directories. A check answers a [`BrowserCheckInfo`]; a fix the matching
//! [`BrowserFixResult`]. Fixes that edit the user's files (`~/.claude.json`,
//! Codex's `config.toml`) are `edits_files` and only ever run on an explicit
//! request; `safe` fixes (the sidecar's assets and `npm ci`, companion files)
//! also run from [`auto_repair`] when the herdr binary changed since the last
//! setup.
//!
//! The shell hook (the guarded `~/.zshrc` line and its managed file) also
//! lives here but is not a browser check: the Agents settings section shows
//! [`shell_hook_check`] and edits `.zshrc` only after an explicit
//! confirmation (`agent_wrap::fix_shell_hook`); the CLI's `herdr browser
//! setup --shell [--remove]` is the other explicit path. `browser.fix` and
//! plain `herdr browser setup` never reach it.

use std::path::{Path, PathBuf};

use crate::api::schema::{
    BrowserCheckInfo, BrowserFixKind, BrowserFixResult, BrowserGetInfo, BrowserRuntimeInfo,
};
use crate::config::BrowserConfig;
use crate::integration::browser_assets;

/// The MCP server name registered with Claude Code and Codex.
pub const MCP_SERVER_NAME: &str = "herdr-browser";
/// The marker at the end of the guarded `.zshrc` line.
pub const ZSHRC_MARKER: &str = "# herdr+";
/// The version line of the managed shell file this herdr writes; a sourced
/// file without it is an older hook (`outdated`).
pub const SHELL_FILE_MARKER: &str = "# herdr+ shell v2";
/// The check ids, in display order.
pub const CHECK_IDS: [&str; 6] = [
    "executable",
    "helper",
    "extension",
    "mcp_claude",
    "mcp_codex",
    "launch_context",
];
/// The record of the last setup in the browser home (`auto_repair` compares it).
pub const SETUP_FILE: &str = "setup.json";

/// The pane variables Codex must forward to the server: Codex starts MCP
/// servers with a minimal environment (HOME, PATH, …), so without this list
/// the server cannot find the pane's herdr socket or attribute the caller.
pub const CODEX_FORWARDED_ENV: [&str; 7] = [
    "HERDR_PANE_ID",
    "HERDR_BIN_PATH",
    "HERDR_SOCKET_PATH",
    "HERDR_SESSION",
    "HERDR_ENV",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
];

/// Where the checks look and the fixes write.
#[derive(Debug, Clone)]
pub struct SetupEnv {
    /// The browser home (`<state dir>/browser`): `host/` under it.
    pub browser_home: PathBuf,
    /// This herdr's binary: the fallback in the MCP commands, compared with
    /// what a registration points at.
    pub binary: PathBuf,
    /// This instance's managed shell file (`<config dir>/shell/herdr-plus.zsh`).
    pub shell_file: PathBuf,
    /// `$ZDOTDIR/.zshrc`, else `$HOME/.zshrc`.
    pub zshrc: Option<PathBuf>,
    /// Claude Code's user-scope store, `$HOME/.claude.json`.
    pub claude_json: Option<PathBuf>,
    /// `claude` on PATH (the registration goes through its `mcp` verbs).
    pub claude_bin: Option<PathBuf>,
    /// `claude-z` on PATH (the agent wrap runs it instead of `claude`).
    pub claude_z_bin: Option<PathBuf>,
    /// `$CODEX_HOME/config.toml`, else `$HOME/.codex/config.toml`.
    pub codex_config: Option<PathBuf>,
    /// `codex` on PATH (only decides wording).
    pub codex_bin: Option<PathBuf>,
    /// `$HOME`, for `[browser] executable = "auto"`.
    pub home: Option<PathBuf>,
    /// `--node PATH` for one run.
    pub node_override: Option<String>,
}

impl SetupEnv {
    /// The running process's environment (what the CLI and the server use).
    pub fn from_process() -> Self {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let zshrc = std::env::var_os("ZDOTDIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.clone())
            .map(|dir| dir.join(".zshrc"));
        let codex_config = std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".codex")))
            .map(|dir| dir.join("config.toml"));
        Self {
            browser_home: super::browser_home(),
            binary: crate::platform::launch_executable().unwrap_or_else(|_| PathBuf::from("herdr")),
            shell_file: shell_file_path(),
            zshrc,
            claude_json: home.as_ref().map(|h| h.join(".claude.json")),
            claude_bin: on_path("claude"),
            claude_z_bin: on_path("claude-z"),
            codex_config,
            codex_bin: on_path("codex"),
            home,
            node_override: None,
        }
    }

    pub fn host_dir(&self) -> PathBuf {
        self.browser_home.join(browser_assets::HOST_DIR)
    }
}

/// The managed shell file: `<config dir>/shell/herdr-plus.zsh`.
pub fn shell_file_path() -> PathBuf {
    crate::config::config_dir()
        .join("shell")
        .join("herdr-plus.zsh")
}

/// The first `name` on PATH that is an executable file.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a file the current user may execute.
pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run `program args` and answer its trimmed stdout; a failure carries stderr.
pub fn run_capture(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|err| format!("{}: {err}", program.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("cannot read {} ({err})", path.display())),
    }
}

/// Write `contents` over `path` in place: through the symlink to the real
/// file, via a temp file in the same directory, keeping the file's mode.
pub fn write_in_place(path: &Path, contents: &str) -> std::io::Result<PathBuf> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = real
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let tmp = real.with_file_name(format!(".{name}.herdr-{}", std::process::id()));
    let mode = std::fs::metadata(&real).ok().map(|m| m.permissions());
    let result = std::fs::write(&tmp, contents).and_then(|()| {
        if let Some(mode) = mode {
            std::fs::set_permissions(&tmp, mode)?;
        }
        std::fs::rename(&tmp, &real)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| real)
}

/// A one-time copy of `path` as `<name>.herdr-backup` next to it (never overwritten).
pub fn backup_once(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !real.exists() {
        return Ok(None);
    }
    let name = real
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let backup = real.with_file_name(format!("{name}.herdr-backup"));
    if backup.exists() {
        return Ok(None);
    }
    std::fs::copy(&real, &backup)?;
    Ok(Some(backup))
}

// ---------------------------------------------------------------------------
// MCP entries

/// The shell line both registrations run: `HERDR_BIN_PATH` expands inside a
/// herdr pane (so the dev instance reaches its own binary); outside herdr the
/// absolute fallback keeps the server from showing as failed.
pub fn mcp_shell_command(fallback_binary: &Path) -> String {
    format!(
        "exec \"${{HERDR_BIN_PATH:-{}}}\" browser mcp",
        fallback_binary.display()
    )
}

/// The fallback binary inside an MCP command line, when it has our form.
pub fn mcp_command_fallback(command: &str) -> Option<PathBuf> {
    let start = command.find("${HERDR_BIN_PATH:-")? + "${HERDR_BIN_PATH:-".len();
    let end = command[start..].find('}')? + start;
    let path = &command[start..end];
    (!path.is_empty() && command[end..].contains("browser mcp")).then(|| PathBuf::from(path))
}

/// The MCP entry registered with Claude Code (user scope).
pub fn mcp_entry_json(fallback_binary: &Path) -> String {
    serde_json::json!({
        "type": "stdio",
        "command": "sh",
        "args": ["-c", mcp_shell_command(fallback_binary)],
    })
    .to_string()
}

/// The `[mcp_servers.herdr-browser]` table written to Codex's config.
pub fn codex_mcp_block(fallback_binary: &Path) -> String {
    let mut table = toml::value::Table::new();
    table.insert("command".into(), toml::Value::String("sh".into()));
    table.insert(
        "args".into(),
        toml::Value::Array(vec![
            toml::Value::String("-c".into()),
            toml::Value::String(mcp_shell_command(fallback_binary)),
        ]),
    );
    table.insert(
        "env_vars".into(),
        toml::Value::Array(
            CODEX_FORWARDED_ENV
                .iter()
                .map(|v| toml::Value::String((*v).into()))
                .collect(),
        ),
    );
    let body = toml::to_string(&toml::Value::Table(table)).unwrap_or_default();
    format!("[mcp_servers.{MCP_SERVER_NAME}]\n{body}")
}

/// Put `block` (a `[mcp_servers.herdr-browser]` table) into a Codex
/// `config.toml`, edited as a document: an existing entry in any shape
/// (a table, a dotted key, an inline `herdr-browser = {…}`) is replaced,
/// everything else — other tables, comments, spacing — stays. A file that
/// does not parse is refused. `None` removes the entry.
pub fn upsert_codex_block(config: &str, block: Option<&str>) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, Table};
    let mut doc: DocumentMut = config
        .parse()
        .map_err(|err| format!("config.toml does not parse, left untouched: {err}"))?;
    let entry: Option<Table> = match block {
        Some(block) => {
            let fresh: DocumentMut = block
                .parse()
                .map_err(|err| format!("herdr's block does not parse: {err}"))?;
            Some(
                fresh["mcp_servers"][MCP_SERVER_NAME]
                    .as_table()
                    .cloned()
                    .ok_or_else(|| "herdr's block has no table".to_string())?,
            )
        }
        None => None,
    };
    if doc.get("mcp_servers").is_none() {
        if entry.is_none() {
            return Ok(doc.to_string());
        }
        let mut servers = Table::new();
        servers.set_implicit(true);
        doc["mcp_servers"] = Item::Table(servers);
    }
    let inline = doc["mcp_servers"].is_inline_table();
    let servers = doc
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| "`mcp_servers` is not a table; left untouched".to_string())?;
    servers.remove(MCP_SERVER_NAME);
    if let Some(entry) = entry {
        if inline {
            servers.insert(MCP_SERVER_NAME, toml_edit::value(entry.into_inline_table()));
        } else {
            servers.insert(MCP_SERVER_NAME, Item::Table(entry));
        }
    }
    Ok(doc.to_string())
}

/// What a store says about the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRegistration {
    /// No entry.
    Absent,
    /// Our entry, pointing at this fallback binary (`None`: not our command form).
    Registered {
        fallback: Option<PathBuf>,
        /// Codex only: the pane variables are forwarded.
        env_forwarded: bool,
    },
    /// The store cannot be read or parsed.
    Unreadable(String),
}

/// Claude Code's user-scope entry (`~/.claude.json`, `mcpServers`).
pub fn claude_registration(claude_json: &Path) -> McpRegistration {
    let text = match read_text(claude_json) {
        Ok(text) => text,
        Err(err) => return McpRegistration::Unreadable(err),
    };
    if text.trim().is_empty() {
        return McpRegistration::Absent;
    }
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            return McpRegistration::Unreadable(format!(
                "{} does not parse ({err})",
                claude_json.display()
            ))
        }
    };
    let Some(entry) = value.get("mcpServers").and_then(|s| s.get(MCP_SERVER_NAME)) else {
        return McpRegistration::Absent;
    };
    let command = entry
        .get("args")
        .and_then(|a| a.as_array())
        .and_then(|a| a.get(1))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    McpRegistration::Registered {
        fallback: mcp_command_fallback(command),
        env_forwarded: true,
    }
}

/// Codex's entry (`[mcp_servers.herdr-browser]` in config.toml).
pub fn codex_registration(codex_config: &Path) -> McpRegistration {
    let text = match read_text(codex_config) {
        Ok(text) => text,
        Err(err) => return McpRegistration::Unreadable(err),
    };
    if text.trim().is_empty() {
        return McpRegistration::Absent;
    }
    let value: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            return McpRegistration::Unreadable(format!(
                "{} does not parse ({err})",
                codex_config.display()
            ))
        }
    };
    let Some(entry) = value
        .get("mcp_servers")
        .and_then(|s| s.get(MCP_SERVER_NAME))
    else {
        return McpRegistration::Absent;
    };
    let command = entry
        .get("args")
        .and_then(toml::Value::as_array)
        .and_then(|a| a.get(1))
        .and_then(toml::Value::as_str)
        .unwrap_or("");
    let env_forwarded = entry
        .get("env_vars")
        .and_then(toml::Value::as_array)
        .is_some_and(|vars| {
            CODEX_FORWARDED_ENV
                .iter()
                .all(|v| vars.iter().any(|x| x.as_str() == Some(v)))
        });
    McpRegistration::Registered {
        fallback: mcp_command_fallback(command),
        env_forwarded,
    }
}

/// Write (or remove, `wanted = false`) the Codex entry. Answers what happened.
pub fn register_codex(env: &SetupEnv, wanted: bool) -> Result<String, String> {
    let Some(path) = env.codex_config.as_deref() else {
        return Err("codex: no home directory".into());
    };
    let current = read_text(path)?;
    let block = codex_mcp_block(&env.binary);
    let next = upsert_codex_block(&current, wanted.then_some(block.as_str()))
        .map_err(|err| format!("codex: {} {err}", path.display()))?;
    if next == current {
        return Ok(if wanted {
            format!(
                "codex: {} already registers {MCP_SERVER_NAME}",
                path.display()
            )
        } else {
            format!("codex: {} has no {MCP_SERVER_NAME} entry", path.display())
        });
    }
    let backup = backup_once(path)
        .map_err(|err| format!("codex: cannot back up {} ({err})", path.display()))?;
    let real = write_in_place(path, &next)
        .map_err(|err| format!("codex: cannot write {} ({err})", path.display()))?;
    let backup = backup
        .map(|b| format!(" (backup {})", b.display()))
        .unwrap_or_default();
    Ok(if wanted {
        format!(
            "codex: wrote [mcp_servers.{MCP_SERVER_NAME}] to {}{backup}",
            real.display()
        )
    } else {
        format!(
            "codex: removed [mcp_servers.{MCP_SERVER_NAME}] from {}{backup}",
            real.display()
        )
    })
}

/// Register (or remove) the Claude Code user-scope entry through `claude mcp`.
pub fn register_claude(env: &SetupEnv, wanted: bool) -> Result<String, String> {
    let Some(claude) = env.claude_bin.as_deref() else {
        return Err("claude: not on PATH; when it is: claude mcp add-json --scope user …".into());
    };
    let store = env.claude_json.as_deref();
    let registered = |what: McpRegistration| !matches!(what, McpRegistration::Absent);
    let before = store
        .map(claude_registration)
        .unwrap_or(McpRegistration::Absent);
    let mut removed = false;
    if registered(before) {
        // `remove` of a present entry; its own failure is not trusted (an
        // absent entry fails the same way), the store is re-read instead.
        let _ = std::process::Command::new(claude)
            .args(["mcp", "remove", "-s", "user", MCP_SERVER_NAME])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let after = store
            .map(claude_registration)
            .unwrap_or(McpRegistration::Absent);
        if registered(after) {
            return Err(format!(
                "claude: `claude mcp remove -s user {MCP_SERVER_NAME}` left the entry in place"
            ));
        }
        removed = true;
    }
    if !wanted {
        return Ok(if removed {
            format!("claude: {MCP_SERVER_NAME} removed (user scope)")
        } else {
            format!("claude: no {MCP_SERVER_NAME} entry (user scope)")
        });
    }
    let entry = mcp_entry_json(&env.binary);
    run_capture(
        claude,
        &[
            "mcp",
            "add-json",
            "--scope",
            "user",
            MCP_SERVER_NAME,
            &entry,
        ],
        None,
    )
    .map(|_| format!("claude: registered {MCP_SERVER_NAME} (user scope) → {entry}"))
    .map_err(|err| {
        if removed {
            format!("claude: the old entry was removed, re-adding failed ({err}); run: claude mcp add-json --scope user {MCP_SERVER_NAME} '{entry}'")
        } else {
            format!("claude: registration failed ({err})")
        }
    })
}

// ---------------------------------------------------------------------------
// The shell hook

/// What the hook writes to the managed file (overwritten every time). Each
/// function carries its own guard: an agent's shell snapshot keeps the
/// functions but not a helper they call, so a helper would break `codex` /
/// `claude` inside the agents' own shells. `HERDR_NO_WRAP` and
/// `[agents] wrap` are decided by `herdr agent wrap`, per launch.
pub fn shell_file_contents() -> String {
    format!(
        "# managed by herdr — rewritten by Settings → Agents [fix] or `herdr browser setup --shell`; do not edit\n\
{SHELL_FILE_MARKER}\n\
# Inside a herdr+ pane, codex and claude run through `herdr agent wrap` ([agents] wrap decides what it adds);\n\
# elsewhere the real commands run. `command claude` / `command codex` bypass it.\n\
function codex {{ if [ -n \"$HERDR_PANE_ID\" ] && [ -x \"$HERDR_BIN_PATH\" ]; then \"$HERDR_BIN_PATH\" agent wrap codex -- \"$@\"; else command codex \"$@\"; fi }}\n\
function claude-z {{ if [ -n \"$HERDR_PANE_ID\" ] && [ -x \"$HERDR_BIN_PATH\" ]; then \"$HERDR_BIN_PATH\" agent wrap claude -- \"$@\"; else command claude-z \"$@\"; fi }}\n\
# `claude` itself only when it is not an alias (an alias claude='claude-z' reaches the function above);\n\
# the `function` form keeps zsh from expanding such an alias while parsing this file.\n\
if ! alias claude >/dev/null 2>&1; then\n\
  function claude {{ if [ -n \"$HERDR_PANE_ID\" ] && [ -x \"$HERDR_BIN_PATH\" ]; then \"$HERDR_BIN_PATH\" agent wrap claude -- \"$@\"; else command claude \"$@\"; fi }}\n\
fi\n"
    )
}

/// A single-quoted shell word (`'` inside becomes `'\''`).
pub fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The guarded line: sources `file` inside herdr panes only.
pub fn zshrc_hook_line(file: &Path) -> String {
    let quoted = shell_single_quote(&file.display().to_string());
    format!("[ -n \"$HERDR_PANE_ID\" ] && [ -f {quoted} ] && source {quoted}  {ZSHRC_MARKER}")
}

/// The managed file a live (uncommented) herdr+ line sources, if the line is one.
pub fn hook_line_path(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || !trimmed.ends_with(ZSHRC_MARKER) {
        return None;
    }
    let after = trimmed.find("source ")? + "source ".len();
    let rest = &trimmed[after..];
    let path = if let Some(rest) = rest.strip_prefix('\'') {
        // single-quoted, `'\''` escapes
        let mut out = String::new();
        let mut rest = rest;
        loop {
            let end = rest.find('\'')?;
            out.push_str(&rest[..end]);
            rest = &rest[end + 1..];
            if let Some(more) = rest.strip_prefix("\\''") {
                out.push('\'');
                rest = more;
            } else {
                break;
            }
        }
        out
    } else if let Some(rest) = rest.strip_prefix('"') {
        rest[..rest.find('"')?].to_string()
    } else {
        rest.split_whitespace().next()?.to_string()
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The live herdr+ lines of a `.zshrc` and the files they source.
pub fn hook_lines(text: &str) -> Vec<PathBuf> {
    text.lines().filter_map(hook_line_path).collect()
}

/// `text` with every live herdr+ line removed (`only` restricts it to lines
/// for that file) and, with `add`, ours appended. Answers the text and
/// whether it changed.
pub fn rewrite_hook_lines(text: &str, only: Option<&Path>, add: Option<&str>) -> (String, bool) {
    let mut out = String::new();
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        match hook_line_path(line) {
            Some(path) if only.is_none_or(|o| o == path) => {
                // an identical line we are about to add again stays put
                if add.is_some_and(|add| line.trim_end_matches('\n') == add) {
                    out.push_str(line);
                } else {
                    changed = true;
                }
            }
            _ => out.push_str(line),
        }
    }
    if let Some(add) = add {
        if !out.lines().any(|line| line == add) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(add);
            out.push('\n');
            changed = true;
        }
    }
    (out, changed)
}

/// Install (`wanted`) or remove this instance's hook: the managed file and
/// the guarded line in `.zshrc` (a line of another instance is replaced when
/// installing, left alone when removing). Answers what happened.
pub fn install_shell_hook(env: &SetupEnv, wanted: bool) -> Result<String, String> {
    let Some(zshrc) = env.zshrc.as_deref() else {
        return Err("shell: no HOME (or ZDOTDIR); cannot find .zshrc".into());
    };
    let file = &env.shell_file;
    let mut notes = Vec::new();
    if wanted {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|err| format!("shell: {err}"))?;
        }
        std::fs::write(file, shell_file_contents())
            .map_err(|err| format!("shell: cannot write {} ({err})", file.display()))?;
        notes.push(format!("wrote {}", file.display()));
    }
    let text = read_text(zshrc)?;
    let line = zshrc_hook_line(file);
    let (next, changed) = if wanted {
        rewrite_hook_lines(&text, None, Some(&line))
    } else {
        rewrite_hook_lines(&text, Some(file), None)
    };
    if changed {
        if let Some(backup) = backup_once(zshrc)
            .map_err(|err| format!("shell: cannot back up {} ({err})", zshrc.display()))?
        {
            notes.push(format!("backed up .zshrc to {}", backup.display()));
        }
        let real = write_in_place(zshrc, &next)
            .map_err(|err| format!("shell: cannot write {} ({err})", zshrc.display()))?;
        notes.push(if wanted {
            format!("added the herdr+ line to {}", real.display())
        } else {
            format!("removed the herdr+ line from {}", real.display())
        });
    } else {
        notes.push(if wanted {
            format!("{} already has the herdr+ line", zshrc.display())
        } else {
            format!("{} has no herdr+ line for this instance", zshrc.display())
        });
    }
    Ok(format!("shell: {}", notes.join(" · ")))
}

/// The shell hook's state: `ok`, `outdated` or `missing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookState {
    /// The last live herdr+ line in `.zshrc` sources a file with
    /// [`SHELL_FILE_MARKER`] (any instance's file: the content does not
    /// depend on the instance).
    Ok,
    /// A herdr+ line is there, but the file it sources is missing or an
    /// older version (v1 breaks `codex` / `claude` inside agents' shells).
    Outdated,
    /// No live herdr+ line (or no `.zshrc` / home at all).
    Missing,
}

impl HookState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Outdated => "outdated",
            Self::Missing => "missing",
        }
    }
}

/// What [`shell_hook_check`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub state: HookState,
    pub detail: String,
    /// The fix ([`install_shell_hook`] with `wanted`) can repair it.
    pub fixable: bool,
    /// The fix edits the user's files (`.zshrc`, the managed file).
    pub edits_files: bool,
}

/// Whether the managed file at `path` is this version's.
fn shell_file_is_current(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|text| text.lines().any(|line| line.trim() == SHELL_FILE_MARKER))
}

/// The shell hook as zsh will see it, instance-agnostic: zsh sources the
/// lines in order and the last definition wins, so the last live herdr+
/// line decides. Never writes.
pub fn shell_hook_check(env: &SetupEnv) -> CheckOutcome {
    let outcome = |state: HookState, detail: String, fixable: bool| CheckOutcome {
        state,
        detail,
        fixable,
        edits_files: fixable,
    };
    let Some(zshrc) = env.zshrc.as_deref() else {
        return outcome(
            HookState::Missing,
            "no HOME (or ZDOTDIR); no .zshrc".into(),
            false,
        );
    };
    let text = match read_text(zshrc) {
        Ok(text) => text,
        Err(err) => return outcome(HookState::Missing, err, false),
    };
    let short = |p: &Path| shorten_home(p, env.home.as_deref());
    let lines = hook_lines(&text);
    let Some(last) = lines.last() else {
        return outcome(
            HookState::Missing,
            format!("{} has no herdr+ line", short(zshrc)),
            true,
        );
    };
    let others = lines.len() - 1;
    let also = if others == 0 {
        String::new()
    } else {
        format!(
            " (+{others} earlier herdr+ line{})",
            if others == 1 { "" } else { "s" }
        )
    };
    if !last.is_file() {
        return outcome(
            HookState::Outdated,
            format!(
                "{} sources a missing file {}{also}",
                short(zshrc),
                short(last)
            ),
            true,
        );
    }
    if !shell_file_is_current(last) {
        return outcome(
            HookState::Outdated,
            format!("{} is an older hook (not v2){also}", short(last)),
            true,
        );
    }
    outcome(
        HookState::Ok,
        format!("{} → {}{also}", short(zshrc), short(last)),
        true,
    )
}

// ---------------------------------------------------------------------------
// The helper (sidecar): assets, node, npm ci, runtime.json

/// Install the sidecar: assets, `npm ci` when the lock or node_modules need
/// it (`force_npm` always), runtime.json. Answers a one-line summary.
/// One helper install at a time, across threads (a `static` mutex) and
/// processes (`flock` on `<browser home>/setup.lock`, next to the setup
/// record): the server's auto-repair, a fix and the CLI must not run
/// `npm ci` into the same node_modules together.
pub struct HelperLock {
    _thread: std::sync::MutexGuard<'static, ()>,
    _file: Option<std::fs::File>,
}

pub fn helper_lock(browser_home: &Path) -> Result<HelperLock, String> {
    static THREAD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let thread = THREAD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::fs::create_dir_all(browser_home)
        .map_err(|err| format!("helper: cannot create {} ({err})", browser_home.display()))?;
    let path = browser_home.join("setup.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|err| format!("helper: cannot open {} ({err})", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: flock on a file descriptor this process owns; it blocks
        // until the other holder lets go.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(format!(
                "helper: cannot lock {} ({})",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(HelperLock {
        _thread: thread,
        _file: Some(file),
    })
}

pub fn install_helper(
    env: &SetupEnv,
    config: &BrowserConfig,
    force_npm: bool,
) -> Result<String, String> {
    let _lock = helper_lock(&env.browser_home)?;
    let host_dir = env.host_dir();
    let lock_before = std::fs::read_to_string(host_dir.join("package-lock.json")).ok();
    let written = browser_assets::install(&host_dir)
        .map_err(|err| format!("assets: cannot write {} ({err})", host_dir.display()))?;
    let lock_changed = lock_before.as_deref()
        != std::fs::read_to_string(host_dir.join("package-lock.json"))
            .ok()
            .as_deref();
    let recorded = browser_assets::read_runtime(&host_dir);
    let node = super::node::discover_default(
        env.node_override.as_deref().or(config.node()),
        recorded.as_ref().map(|r| Path::new(&r.node)),
    )
    .ok_or_else(|| {
        "node: not found. Install Node.js 20+ (nvm, Homebrew) or set [browser] node".to_string()
    })?;
    let node_version = run_capture(&node.path, &["--version"], None)
        .map_err(|err| format!("node: {} does not run ({err})", node.path.display()))?;
    if !super::node::version_ok(&node_version) {
        return Err(format!(
            "node: {} is {node_version}; 20 or newer is required",
            node.path.display()
        ));
    }
    let installed = browser_assets::playwright_installed(&host_dir);
    let need_npm = force_npm
        || lock_changed
        || installed.as_deref() != Some(browser_assets::PLAYWRIGHT_CORE_VERSION);
    let npm = super::node::npm_beside(&node.path).or_else(|| on_path("npm"));
    let mut summary = vec![format!("{written} asset file(s) written")];
    if need_npm {
        let Some(npm) = npm.as_deref() else {
            return Err(format!(
                "npm: not found beside {} or on PATH",
                node.path.display()
            ));
        };
        let mut command = std::process::Command::new(npm);
        command
            .args([
                "ci",
                "--omit=dev",
                "--no-audit",
                "--no-fund",
                "--ignore-scripts",
            ])
            .current_dir(&host_dir)
            .env("PATH", {
                let mut paths = vec![node
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()];
                if let Some(path) = std::env::var_os("PATH") {
                    paths.extend(std::env::split_paths(&path));
                }
                std::env::join_paths(paths).unwrap_or_default()
            })
            .stdin(std::process::Stdio::null());
        let output = command
            .output()
            .map_err(|err| format!("npm: cannot run {} ({err})", npm.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail: Vec<&str> = stderr
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(format!(
                "npm ci failed ({}) in {}: {}",
                output.status,
                host_dir.display(),
                tail.join(" | ")
            ));
        }
        summary.push("npm ci".into());
    }
    let playwright = browser_assets::playwright_installed(&host_dir)
        .ok_or_else(|| "npm ci finished but node_modules/playwright-core is missing".to_string())?;
    let runtime = BrowserRuntimeInfo {
        version: browser_assets::RUNTIME_VERSION,
        node: node.path.display().to_string(),
        node_version: Some(node_version.clone()),
        npm: npm.map(|p| p.display().to_string()),
        playwright_core: Some(playwright.clone()),
        assets_sha256: browser_assets::assets_sha256(),
        installed_at: super::unix_now(),
    };
    browser_assets::write_runtime(&host_dir, &runtime)
        .map_err(|err| format!("runtime.json: cannot write ({err})"))?;
    summary.push(format!(
        "playwright-core {playwright} · node {node_version}"
    ));
    Ok(format!("helper: {}", summary.join(" · ")))
}

// ---------------------------------------------------------------------------
// Checks

fn check(id: &str, ok: bool, detail: impl Into<String>, fix: BrowserFixKind) -> BrowserCheckInfo {
    BrowserCheckInfo {
        id: id.to_string(),
        ok,
        detail: detail.into(),
        fixable: fix != BrowserFixKind::None,
        fix_kind: fix,
    }
}

/// The doctor's checks, in `CHECK_IDS` order. `live` is the server's current
/// picture (the companion version of a running profile); the CLI passes
/// what `browser.get` answered, the server its own.
pub fn checks(
    config: &BrowserConfig,
    env: &SetupEnv,
    live: Option<&BrowserGetInfo>,
) -> Vec<BrowserCheckInfo> {
    let mut out = Vec::with_capacity(CHECK_IDS.len());

    // executable
    out.push(
        match super::launch::resolve_executable(&config.executable, env.home.as_deref()) {
            Ok(exe) => check(
                "executable",
                true,
                format!("{} ({})", exe.display(), exe.source),
                BrowserFixKind::None,
            ),
            Err(err) => check("executable", false, err.message, BrowserFixKind::None),
        },
    );

    // helper
    let host_dir = env.host_dir();
    let runtime = browser_assets::read_runtime(&host_dir);
    let playwright = browser_assets::playwright_installed(&host_dir);
    let node = super::node::discover_default(
        env.node_override.as_deref().or(config.node()),
        runtime.as_ref().map(|r| Path::new(&r.node)),
    );
    let node_version = node
        .as_ref()
        .map(|n| run_capture(&n.path, &["--version"], None).unwrap_or_else(|err| err));
    let helper = match (&runtime, &playwright, &node, &node_version) {
        (_, _, None, _) => check(
            "helper",
            false,
            "node not found: install Node.js 20+ (nvm, Homebrew) or set [browser] node",
            BrowserFixKind::None,
        ),
        (_, _, Some(node), Some(version)) if !super::node::version_ok(version) => check(
            "helper",
            false,
            format!(
                "node {} is {version}; 20 or newer is required",
                node.path.display()
            ),
            BrowserFixKind::None,
        ),
        (None, _, _, _) => check(
            "helper",
            false,
            "not installed (no runtime.json)",
            BrowserFixKind::Safe,
        ),
        (_, None, _, _) => check(
            "helper",
            false,
            "playwright-core not installed (npm ci pending)",
            BrowserFixKind::Safe,
        ),
        (_, Some(installed), _, _) if installed != browser_assets::PLAYWRIGHT_CORE_VERSION => {
            check(
                "helper",
                false,
                format!(
                    "playwright-core {installed} installed, this herdr expects {}",
                    browser_assets::PLAYWRIGHT_CORE_VERSION
                ),
                BrowserFixKind::Safe,
            )
        }
        (Some(runtime), _, _, _) if runtime.assets_sha256 != browser_assets::assets_sha256() => {
            check(
                "helper",
                false,
                "sidecar assets outdated",
                BrowserFixKind::Safe,
            )
        }
        (_, Some(installed), _, Some(version)) => check(
            "helper",
            true,
            format!("playwright-core {installed} · node {version}"),
            BrowserFixKind::Safe,
        ),
        _ => check("helper", false, "unknown state", BrowserFixKind::Safe),
    };
    out.push(helper);

    // extension (companion): the running worker's version, else the bundle
    let running = live.and_then(|info| {
        info.profiles
            .iter()
            .find(|p| p.name == config.default_profile() && p.state == "running")
    });
    out.push(match running {
        Some(profile) => match profile.companion.as_deref() {
            Some(state) if state.contains("expects") => {
                let idle = profile.agents == 0;
                check(
                    "extension",
                    false,
                    if idle {
                        state.to_string()
                    } else {
                        format!("{state} — stop and open when the agents are done")
                    },
                    if idle {
                        BrowserFixKind::Safe
                    } else {
                        BrowserFixKind::None
                    },
                )
            }
            Some(state) if state.starts_with("ready") => check(
                "extension",
                true,
                format!("v{} · {state}", browser_assets::COMPANION_VERSION),
                BrowserFixKind::None,
            ),
            Some(state) => check(
                "extension",
                !state.starts_with("missing"),
                format!("v{} · {state}", browser_assets::COMPANION_VERSION),
                BrowserFixKind::None,
            ),
            None => check(
                "extension",
                true,
                format!("v{} (bundled)", browser_assets::COMPANION_VERSION),
                BrowserFixKind::None,
            ),
        },
        None => check(
            "extension",
            true,
            format!(
                "v{} (bundled; checked when the browser runs)",
                browser_assets::COMPANION_VERSION
            ),
            BrowserFixKind::None,
        ),
    });

    // MCP per agent
    let wants = |agent: &str| config.mcp_agents.iter().any(|a| a == agent);
    out.push(mcp_check(
        "mcp_claude",
        "claude",
        wants("claude"),
        env.claude_json.as_deref().map(claude_registration),
        env.claude_bin.is_some(),
        &env.binary,
        "claude not on PATH",
    ));
    let mut codex = mcp_check(
        "mcp_codex",
        "codex",
        wants("codex"),
        env.codex_config.as_deref().map(codex_registration),
        env.codex_config.is_some(),
        &env.binary,
        "no home directory",
    );
    if !codex.ok && env.codex_bin.is_none() && codex.detail.ends_with("not registered") {
        codex
            .detail
            .push_str(" (no `codex` on PATH; fix all registers it anyway)");
    }
    out.push(codex);

    // launch context
    out.push(launch_context_check());
    out
}

#[allow(clippy::too_many_arguments)]
fn mcp_check(
    id: &str,
    agent: &str,
    wanted: bool,
    registration: Option<McpRegistration>,
    can_fix: bool,
    binary: &Path,
    cannot_fix_reason: &str,
) -> BrowserCheckInfo {
    let fix = if can_fix {
        BrowserFixKind::EditsFiles
    } else {
        BrowserFixKind::None
    };
    let registration = registration.unwrap_or(McpRegistration::Absent);
    match (wanted, registration) {
        (_, McpRegistration::Unreadable(err)) => check(id, false, err, BrowserFixKind::None),
        (true, McpRegistration::Absent) => check(
            id,
            false,
            if can_fix {
                format!("{agent}: not registered")
            } else {
                format!("{agent}: not registered ({cannot_fix_reason})")
            },
            fix,
        ),
        (
            true,
            McpRegistration::Registered {
                fallback,
                env_forwarded,
            },
        ) => {
            if !env_forwarded {
                check(
                    id,
                    false,
                    format!("{agent}: registered without the forwarded pane variables (env_vars)"),
                    fix,
                )
            } else {
                match fallback {
                    Some(path) if path == binary => {
                        check(id, true, format!("{agent}: registered"), fix)
                    }
                    Some(path) => check(
                        id,
                        false,
                        format!(
                            "{agent}: registered for another herdr binary: {}",
                            path.display()
                        ),
                        fix,
                    ),
                    None => check(
                        id,
                        false,
                        format!("{agent}: registered with a command herdr did not write"),
                        fix,
                    ),
                }
            }
        }
        (false, McpRegistration::Absent) => check(id, true, format!("{agent}: off"), fix),
        (false, McpRegistration::Registered { .. }) => check(
            id,
            false,
            format!("{agent}: still registered (off in [browser] mcp_agents)"),
            fix,
        ),
    }
}

fn launch_context_check() -> BrowserCheckInfo {
    #[cfg(target_os = "macos")]
    {
        let manager = run_capture(Path::new("/bin/launchctl"), &["managername"], None)
            .unwrap_or_else(|_| "unknown".into());
        let detail = if manager == "Aqua" {
            "Aqua".to_string()
        } else {
            format!("{manager} (Chromium still gets a window through LaunchServices)")
        };
        check("launch_context", true, detail, BrowserFixKind::None)
    }
    #[cfg(not(target_os = "macos"))]
    {
        check("launch_context", true, "n/a", BrowserFixKind::None)
    }
}

/// `~/…` for a path under `home`.
pub fn shorten_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Whether a check that only an explicit request may fix is failing.
pub fn setup_needed(checks: &[BrowserCheckInfo]) -> bool {
    checks
        .iter()
        .any(|c| !c.ok && c.fix_kind == BrowserFixKind::EditsFiles)
}

// ---------------------------------------------------------------------------
// Fixes

/// Run the fix for `id` (everything but `extension`, which only the hub can
/// do: it restarts the browser). `None` for an id without a fix.
pub fn fix(id: &str, config: &BrowserConfig, env: &SetupEnv) -> Option<BrowserFixResult> {
    let wants = |agent: &str| config.mcp_agents.iter().any(|a| a == agent);
    let outcome = match id {
        "helper" => install_helper(env, config, false),
        "mcp_claude" => register_claude(env, wants("claude")),
        "mcp_codex" => register_codex(env, wants("codex")),
        _ => return None,
    };
    Some(match outcome {
        Ok(detail) => BrowserFixResult {
            id: id.into(),
            ok: true,
            detail,
        },
        Err(detail) => BrowserFixResult {
            id: id.into(),
            ok: false,
            detail,
        },
    })
}

/// The fixes for `ids`, or for every failing fixable check when empty
/// (`extension` left to the caller).
pub fn fix_all(
    ids: &[String],
    checks: &[BrowserCheckInfo],
    config: &BrowserConfig,
    env: &SetupEnv,
) -> Vec<BrowserFixResult> {
    let wanted: Vec<String> = if ids.is_empty() {
        checks
            .iter()
            .filter(|c| !c.ok && c.fixable && c.id != "extension")
            .map(|c| c.id.clone())
            .collect()
    } else {
        ids.to_vec()
    };
    wanted
        .iter()
        .filter_map(|id| fix(id, config, env))
        .collect()
}

// ---------------------------------------------------------------------------
// Auto-repair

/// What the last setup ran for: the binary (version, size, mtime) and the
/// assets. A difference means the herdr binary changed since.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct SetupRecord {
    pub herdr_version: String,
    pub binary_len: u64,
    pub binary_mtime: u64,
    pub assets_sha256: String,
    #[serde(default)]
    pub at: u64,
}

impl SetupRecord {
    pub fn current(env: &SetupEnv) -> Self {
        let meta = std::fs::metadata(&env.binary).ok();
        Self {
            herdr_version: crate::build_info::version(),
            binary_len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            binary_mtime: meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            assets_sha256: browser_assets::assets_sha256(),
            at: 0,
        }
    }

    fn same_build(&self, other: &Self) -> bool {
        self.herdr_version == other.herdr_version
            && self.binary_len == other.binary_len
            && self.binary_mtime == other.binary_mtime
            && self.assets_sha256 == other.assets_sha256
    }
}

pub fn read_setup_record(home: &Path) -> Option<SetupRecord> {
    let text = std::fs::read_to_string(home.join(SETUP_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_setup_record(home: &Path, record: &SetupRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let path = home.join(SETUP_FILE);
    let tmp = home.join(format!(".{SETUP_FILE}.{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(record).unwrap_or_default(),
    )?;
    std::fs::rename(&tmp, &path)
}

/// On the first browser use after the herdr binary changed: the safe fixes
/// (assets, `npm ci` when the lock changed or playwright-core is off its
/// pin, runtime.json). Never a user file. Answers what it did, `None` when
/// the record matched and nothing ran.
pub fn auto_repair(config: &BrowserConfig, env: &SetupEnv) -> Option<Result<String, String>> {
    let mut current = SetupRecord::current(env);
    let recorded = read_setup_record(&env.browser_home);
    if recorded.as_ref().is_some_and(|r| r.same_build(&current)) {
        return None;
    }
    // A home that was never set up has nothing to repair: `setup` installs it.
    browser_assets::read_runtime(&env.host_dir())?;
    let outcome = install_helper(env, config, false);
    if outcome.is_ok() {
        current.at = super::unix_now();
        let _ = write_setup_record(&env.browser_home, &current);
    }
    Some(outcome.map(|detail| {
        format!(
            "{detail} (herdr changed{})",
            recorded
                .map(|r| format!(" since {}", r.herdr_version))
                .unwrap_or_default()
        )
    }))
}

/// Record the current build as set up (after `setup` or a successful helper fix).
pub fn mark_set_up(env: &SetupEnv) {
    let mut record = SetupRecord::current(env);
    record.at = super::unix_now();
    let _ = write_setup_record(&env.browser_home, &record);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("herdr-setup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn env_in(dir: &Path) -> SetupEnv {
        SetupEnv {
            browser_home: dir.join("browser"),
            binary: dir.join("bin/herdr"),
            shell_file: dir.join("config/shell/herdr-plus.zsh"),
            zshrc: Some(dir.join("home/.zshrc")),
            claude_json: Some(dir.join("home/.claude.json")),
            claude_bin: None,
            claude_z_bin: None,
            codex_config: Some(dir.join("home/.codex/config.toml")),
            codex_bin: None,
            home: Some(dir.join("home")),
            node_override: None,
        }
    }

    fn config(mcp: &[&str], legacy_hook: bool) -> BrowserConfig {
        BrowserConfig {
            mcp_agents: mcp.iter().map(|s| s.to_string()).collect(),
            shell_hook: legacy_hook,
            executable: "/nonexistent/Chromium.app".into(),
            ..BrowserConfig::default()
        }
    }

    fn by_id<'a>(checks: &'a [BrowserCheckInfo], id: &str) -> &'a BrowserCheckInfo {
        checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no check {id}"))
    }

    #[test]
    fn the_browser_checks_and_fixes_never_reach_the_shell_hook() {
        let dir = temp("no-hook");
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let env = env_in(&dir);
        std::fs::write(dir.join("home/.zshrc"), "alias x=y\n").unwrap();
        assert!(!CHECK_IDS.contains(&"shell_hook"));
        // even the legacy `shell_hook = true` changes nothing
        let found = checks(&config(&[], true), &env, None);
        assert!(found.iter().all(|c| c.id != "shell_hook"), "{found:?}");
        assert_eq!(
            found.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            CHECK_IDS
        );
        assert!(fix("shell_hook", &config(&[], true), &env).is_none());
        let named = fix_all(&["shell_hook".into()], &found, &config(&[], true), &env);
        assert!(named.is_empty(), "{named:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("home/.zshrc")).unwrap(),
            "alias x=y\n",
            "byte-identical"
        );
        assert!(!env.shell_file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shell_file_inlines_its_guard_and_calls_agent_wrap() {
        let text = shell_file_contents();
        assert!(text.lines().any(|l| l == SHELL_FILE_MARKER), "{text}");
        assert!(!text.contains("_herdr_plus_wrap"), "no helper function");
        assert!(!text.contains("browser wrap"), "{text}");
        for (function, agent, real) in [
            ("codex", "codex", "codex"),
            ("claude-z", "claude", "claude-z"),
            ("claude", "claude", "claude"),
        ] {
            let line = text
                .lines()
                .find(|l| {
                    l.trim_start()
                        .starts_with(&format!("function {function} {{"))
                })
                .unwrap_or_else(|| panic!("no {function} in {text}"));
            assert!(
                line.contains("[ -n \"$HERDR_PANE_ID\" ] && [ -x \"$HERDR_BIN_PATH\" ]"),
                "{line}"
            );
            assert!(
                line.contains(&format!("\"$HERDR_BIN_PATH\" agent wrap {agent} -- \"$@\"")),
                "{line}"
            );
            assert!(line.contains(&format!("command {real} \"$@\"")), "{line}");
        }
        // zsh parses it (skipped without zsh)
        let dir = temp("zsh-n");
        let file = dir.join("herdr-plus.zsh");
        std::fs::write(&file, &text).unwrap();
        if let Some(zsh) = on_path("zsh") {
            let status = std::process::Command::new(zsh)
                .arg("-n")
                .arg(&file)
                .status()
                .unwrap();
            assert!(status.success(), "zsh -n failed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hook_check_is_ok_outdated_or_missing_and_instance_agnostic() {
        let dir = temp("hook");
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let env = env_in(&dir);
        let zshrc = dir.join("home/.zshrc");
        // no .zshrc / no line: missing, fixable
        let missing = shell_hook_check(&env);
        assert_eq!(missing.state, HookState::Missing);
        assert!(missing.fixable && missing.edits_files);
        std::fs::write(&zshrc, "alias x=y\n").unwrap();
        assert_eq!(shell_hook_check(&env).state, HookState::Missing);
        // another instance's line whose file is v1: outdated
        let other = dir.join("other/shell/herdr-plus.zsh");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(
            &other,
            "_herdr_plus_wrap() { [ -n \"$HERDR_PANE_ID\" ]; }\nfunction codex { herdr browser wrap codex; }\n",
        )
        .unwrap();
        std::fs::write(&zshrc, format!("alias x=y\n{}\n", zshrc_hook_line(&other))).unwrap();
        let old = shell_hook_check(&env);
        assert_eq!(old.state, HookState::Outdated, "{}", old.detail);
        assert!(old.detail.contains("older hook"), "{}", old.detail);
        assert!(old.fixable);
        // the same instance-foreign line, its file now v2: ok without a rewrite
        std::fs::write(&other, shell_file_contents()).unwrap();
        let ok = shell_hook_check(&env);
        assert_eq!(ok.state, HookState::Ok, "{}", ok.detail);
        // a line whose file is gone: outdated
        std::fs::remove_file(&other).unwrap();
        let gone = shell_hook_check(&env);
        assert_eq!(gone.state, HookState::Outdated);
        assert!(gone.detail.contains("missing file"), "{}", gone.detail);
        // the last line decides (zsh: the last definition wins)
        std::fs::write(&other, shell_file_contents()).unwrap();
        std::fs::create_dir_all(env.shell_file.parent().unwrap()).unwrap();
        std::fs::write(&env.shell_file, "function codex { :; }\n").unwrap();
        std::fs::write(
            &zshrc,
            format!(
                "{}\n{}\n",
                zshrc_hook_line(&other),
                zshrc_hook_line(&env.shell_file)
            ),
        )
        .unwrap();
        let last = shell_hook_check(&env);
        assert_eq!(last.state, HookState::Outdated, "{}", last.detail);
        assert!(last.detail.contains("+1 earlier"), "{}", last.detail);
        // no HOME at all: missing, not fixable
        let mut homeless = env.clone();
        homeless.zshrc = None;
        let none = shell_hook_check(&homeless);
        assert_eq!(none.state, HookState::Missing);
        assert!(!none.fixable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_the_hook_replaces_other_lines_once_and_removing_takes_only_ours() {
        let dir = temp("hook-install");
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let env = env_in(&dir);
        let other = PathBuf::from("/Users/me/.herdr-dev/config/herdr/shell/herdr-plus.zsh");
        std::fs::write(
            dir.join("home/.zshrc"),
            format!("alias x=y\n{}\n", zshrc_hook_line(&other)),
        )
        .unwrap();
        assert_eq!(
            hook_lines(&std::fs::read_to_string(dir.join("home/.zshrc")).unwrap()),
            vec![other.clone()]
        );
        // install replaces it (backup first), and only once
        let result = install_shell_hook(&env, true).unwrap();
        assert!(result.contains("added"), "{result}");
        let text = std::fs::read_to_string(dir.join("home/.zshrc")).unwrap();
        assert_eq!(hook_lines(&text), vec![env.shell_file.clone()]);
        assert!(text.starts_with("alias x=y\n"));
        assert!(!text.contains(".herdr-dev"));
        assert!(dir.join("home/.zshrc.herdr-backup").is_file());
        assert!(shell_file_is_current(&env.shell_file));
        assert_eq!(shell_hook_check(&env).state, HookState::Ok);
        let again = install_shell_hook(&env, true).unwrap();
        assert!(again.contains("already has"), "{again}");
        assert_eq!(
            std::fs::read_to_string(dir.join("home/.zshrc")).unwrap(),
            text
        );
        // a commented-out copy is neither present nor removed; remove takes only ours
        std::fs::write(
            dir.join("home/.zshrc"),
            format!("#{}\n{text}", zshrc_hook_line(&env.shell_file)),
        )
        .unwrap();
        let off = install_shell_hook(&env, false).unwrap();
        assert!(off.contains("removed"), "{off}");
        let text = std::fs::read_to_string(dir.join("home/.zshrc")).unwrap();
        assert!(hook_lines(&text).is_empty());
        assert!(text.contains("#[ -n"), "the comment stays: {text}");
        assert_eq!(shell_hook_check(&env).state, HookState::Missing);
        // single quotes with an apostrophe in the path round-trip
        let odd = PathBuf::from("/Users/o'neil/shell/herdr-plus.zsh");
        assert_eq!(hook_line_path(&zshrc_hook_line(&odd)), Some(odd));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_checks_compare_the_registered_binary_and_the_wanted_agents() {
        let dir = temp("mcp");
        std::fs::create_dir_all(dir.join("home/.codex")).unwrap();
        let env = env_in(&dir);
        // nothing registered, both wanted: both fail; codex fixable without a CLI, claude needs one
        let checks0 = checks(&config(&["claude", "codex"], false), &env, None);
        assert!(!by_id(&checks0, "mcp_codex").ok);
        assert!(by_id(&checks0, "mcp_codex").fixable);
        assert!(!by_id(&checks0, "mcp_claude").ok);
        assert!(!by_id(&checks0, "mcp_claude").fixable, "no claude on PATH");
        assert!(by_id(&checks0, "mcp_claude").detail.contains("not on PATH"));
        // the codex fix writes the entry for THIS binary; the check passes
        let fixed = fix("mcp_codex", &config(&["codex"], false), &env).unwrap();
        assert!(fixed.ok, "{}", fixed.detail);
        assert!(by_id(&checks(&config(&["codex"], false), &env, None), "mcp_codex").ok);
        // another binary's registration is reported
        let text = std::fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
        std::fs::write(
            dir.join("home/.codex/config.toml"),
            text.replace(&env.binary.display().to_string(), "/opt/other/herdr"),
        )
        .unwrap();
        let other = checks(&config(&["codex"], false), &env, None);
        assert!(!by_id(&other, "mcp_codex").ok);
        assert!(
            by_id(&other, "mcp_codex")
                .detail
                .contains("/opt/other/herdr"),
            "{}",
            by_id(&other, "mcp_codex").detail
        );
        // off: a registration is a failing check, the fix removes it, other content stays
        std::fs::write(
            dir.join("home/.codex/config.toml"),
            format!("model = \"x\" # keep\n{text}"),
        )
        .unwrap();
        let off = checks(&config(&[], false), &env, None);
        assert!(!by_id(&off, "mcp_codex").ok);
        let removed = fix("mcp_codex", &config(&[], false), &env).unwrap();
        assert!(removed.ok, "{}", removed.detail);
        let after = std::fs::read_to_string(dir.join("home/.codex/config.toml")).unwrap();
        assert!(after.contains("model = \"x\" # keep"));
        assert!(!after.contains(MCP_SERVER_NAME));
        assert!(by_id(&checks(&config(&[], false), &env, None), "mcp_codex").ok);
        // claude: the user-scope store is read directly
        std::fs::write(
            dir.join("home/.claude.json"),
            serde_json::json!({ "mcpServers": { MCP_SERVER_NAME: { "type": "stdio", "command": "sh", "args": ["-c", mcp_shell_command(&env.binary)] } } }).to_string(),
        )
        .unwrap();
        let claude = checks(&config(&["claude"], false), &env, None);
        assert!(
            by_id(&claude, "mcp_claude").ok,
            "{}",
            by_id(&claude, "mcp_claude").detail
        );
        let not_wanted = checks(&config(&[], false), &env, None);
        assert!(!by_id(&not_wanted, "mcp_claude").ok);
        assert!(by_id(&not_wanted, "mcp_claude")
            .detail
            .contains("still registered"));
        assert_eq!(
            mcp_command_fallback("exec \"${HERDR_BIN_PATH:-/a/b herdr}\" browser mcp"),
            Some(PathBuf::from("/a/b herdr"))
        );
        assert_eq!(mcp_command_fallback("node server.js"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fake_claude_cli_registers_and_removes_through_mcp_verbs() {
        let dir = temp("claude-cli");
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let script = dir.join("claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> {}\n",
                dir.join("argv.log").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = env_in(&dir);
        env.claude_bin = Some(script);
        let on = fix("mcp_claude", &config(&["claude"], false), &env).unwrap();
        assert!(on.ok, "{}", on.detail);
        let off = fix("mcp_claude", &config(&[], false), &env).unwrap();
        assert!(off.ok, "{}", off.detail);
        // the fake never writes the store: the entry stays absent, so no
        // remove runs for the registration nor for the removal (A16)
        let log = std::fs::read_to_string(dir.join("argv.log")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "{log}");
        assert!(lines[0].starts_with("mcp add-json --scope user herdr-browser {"));
        assert!(lines[0].contains("browser mcp"));
        assert!(
            off.detail.contains("no herdr-browser entry"),
            "{}",
            off.detail
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_registration_skips_the_remove_when_absent_and_reads_the_store_back() {
        let dir = temp("claude-store");
        std::fs::create_dir_all(dir.join("home")).unwrap();
        // a fake claude that maintains the store: remove deletes the entry,
        // add-json writes it (unless FAIL_ADD is set)
        let script = dir.join("claude");
        std::fs::write(&script, format!(r#"#!/bin/sh
echo "$@" >> {log}
store={store}
case "$2" in
  remove) if [ -f "$store" ]; then python3 -c "import json,sys; p=sys.argv[1]; d=json.load(open(p)); d.get('mcpServers',{{}}).pop('herdr-browser', None); json.dump(d, open(p,'w'))" "$store"; else exit 1; fi ;;
  add-json) if [ -n "$FAIL_ADD" ]; then exit 1; fi; python3 -c "import json,sys,os; p=sys.argv[1]; d=json.load(open(p)) if os.path.exists(p) else {{}}; d.setdefault('mcpServers',{{}})['herdr-browser']=json.loads(sys.argv[2]); json.dump(d, open(p,'w'))" "$store" "$6" ;;
esac
"#, log = dir.join("argv.log").display(), store = dir.join("home/.claude.json").display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut env = env_in(&dir);
        env.claude_bin = Some(script);
        // absent + wanted: no remove, one add-json; the check then passes
        let on = register_claude(&env, true).unwrap();
        assert!(on.starts_with("claude: registered"), "{on}");
        let log = std::fs::read_to_string(dir.join("argv.log")).unwrap();
        assert_eq!(
            log.lines().count(),
            1,
            "no remove of an absent entry: {log}"
        );
        assert!(log.starts_with("mcp add-json"));
        assert!(matches!(
            claude_registration(&dir.join("home/.claude.json")),
            McpRegistration::Registered { .. }
        ));
        // present + wanted: remove, re-read, add-json
        let again = register_claude(&env, true).unwrap();
        assert!(again.starts_with("claude: registered"), "{again}");
        let log = std::fs::read_to_string(dir.join("argv.log")).unwrap();
        assert_eq!(log.lines().count(), 3, "{log}");
        // present + not wanted: remove only, verified by the store
        let off = register_claude(&env, false).unwrap();
        assert!(off.contains("removed"), "{off}");
        assert!(matches!(
            claude_registration(&dir.join("home/.claude.json")),
            McpRegistration::Absent
        ));
        // absent + not wanted: nothing runs
        let before = std::fs::read_to_string(dir.join("argv.log")).unwrap();
        let none = register_claude(&env, false).unwrap();
        assert!(none.contains("no herdr-browser entry"), "{none}");
        assert_eq!(
            std::fs::read_to_string(dir.join("argv.log")).unwrap(),
            before
        );
        // present + wanted, but add-json fails: the message says removed, re-add failed
        register_claude(&env, true).unwrap();
        std::env::set_var("FAIL_ADD", "1");
        let err = register_claude(&env, true).unwrap_err();
        std::env::remove_var("FAIL_ADD");
        assert!(err.contains("removed, re-adding failed"), "{err}");
        assert!(matches!(
            claude_registration(&dir.join("home/.claude.json")),
            McpRegistration::Absent
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_helper_lock_serialises_concurrent_installs() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let dir = temp("helper-lock");
        let home = dir.join("browser");
        let inside = Arc::new(AtomicUsize::new(0));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (home, inside, overlaps) = (home.clone(), inside.clone(), overlaps.clone());
                std::thread::spawn(move || {
                    let _lock = helper_lock(&home).unwrap();
                    if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                        overlaps.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    inside.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "the lock lets one install in at a time"
        );
        assert!(
            home.join("setup.lock").is_file(),
            "the lock file sits next to the setup record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_block_is_upserted_without_touching_the_rest() {
        let block = codex_mcp_block(Path::new("/opt/herdr"));
        let herdr = |text: &str| -> toml::Value {
            let value: toml::Value = toml::from_str(text).unwrap();
            value["mcp_servers"]["herdr-browser"].clone()
        };
        let expected = herdr(&block);
        let other = "# my codex config\nmodel = \"x\" # keep\n\n[ mcp_servers . node_repl ]\ncommand = \"node\"\n\n[mcp_servers.node_repl.env]\nA = \"1\"\n";
        let once = upsert_codex_block(other, Some(&block)).unwrap();
        assert!(
            once.starts_with("# my codex config\nmodel = \"x\" # keep\n"),
            "{once}"
        );
        assert!(once.contains("[ mcp_servers . node_repl ]"), "{once}");
        let value: toml::Value = toml::from_str(&once).unwrap();
        assert_eq!(herdr(&once), expected);
        assert_eq!(
            value["mcp_servers"]["node_repl"]["env"]["A"].as_str(),
            Some("1")
        );
        assert_eq!(upsert_codex_block(&once, Some(&block)).unwrap(), once);
        let stale = "a = 1\n[mcp_servers.herdr-browser]\ncommand = \"old\"\n[mcp_servers.herdr-browser.env]\nX = \"1\"\n[features]\nrmcp_client = true # trailing\n";
        let fixed = upsert_codex_block(stale, Some(&block)).unwrap();
        let value: toml::Value = toml::from_str(&fixed).unwrap();
        assert_eq!(herdr(&fixed), expected);
        assert!(value["mcp_servers"]["herdr-browser"].get("env").is_none());
        assert_eq!(value["features"]["rmcp_client"].as_bool(), Some(true));
        assert!(fixed.contains("# trailing"));
        let inline =
            "[mcp_servers]\nherdr-browser = { command = \"old\" }\nother = { command = \"o\" }\n";
        let fixed = upsert_codex_block(inline, Some(&block)).unwrap();
        assert_eq!(herdr(&fixed), expected);
        assert!(fixed.contains("other = { command = \"o\" }"), "{fixed}");
        let dotted =
            "mcp_servers.herdr-browser.command = \"old\"\nmcp_servers.other.command = \"o\"\n";
        let fixed = upsert_codex_block(dotted, Some(&block)).unwrap();
        assert_eq!(herdr(&fixed), expected);
        let err = upsert_codex_block("model = \"x\n[broken", Some(&block)).unwrap_err();
        assert!(err.contains("does not parse"), "{err}");
        assert_eq!(
            herdr(&upsert_codex_block("", Some(&block)).unwrap()),
            expected
        );
        // removal keeps everything else and is a no-op without an entry
        let gone = upsert_codex_block(&once, None).unwrap();
        assert!(!gone.contains("herdr-browser"));
        assert!(gone.contains("[ mcp_servers . node_repl ]"));
        assert_eq!(
            upsert_codex_block("model = \"x\"\n", None).unwrap(),
            "model = \"x\"\n"
        );
    }

    #[test]
    fn the_setup_record_tells_a_changed_binary_and_auto_repair_skips_an_uninstalled_home() {
        let dir = temp("record");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/herdr"), "v1").unwrap();
        let env = env_in(&dir);
        assert!(read_setup_record(&env.browser_home).is_none());
        mark_set_up(&env);
        let record = read_setup_record(&env.browser_home).unwrap();
        assert!(record.same_build(&SetupRecord::current(&env)));
        assert!(record.at > 0);
        // same build: nothing to do (even without a runtime)
        assert!(auto_repair(&BrowserConfig::default(), &env).is_none());
        // a different binary, but no runtime.json: setup never ran here, nothing to repair
        std::fs::write(dir.join("bin/herdr"), "v2 longer").unwrap();
        assert!(!record.same_build(&SetupRecord::current(&env)));
        assert!(auto_repair(&BrowserConfig::default(), &env).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_needed_only_for_failing_file_edits() {
        let mut checks = vec![
            check("helper", false, "x", BrowserFixKind::Safe),
            check("mcp_codex", true, "x", BrowserFixKind::EditsFiles),
        ];
        assert!(!setup_needed(&checks));
        checks.push(check("mcp_claude", false, "x", BrowserFixKind::EditsFiles));
        assert!(setup_needed(&checks));
        let (text, changed) = rewrite_hook_lines("a\n", None, Some("LINE  # herdr+"));
        assert!(changed);
        assert_eq!(text, "a\nLINE  # herdr+\n");
        assert!(!rewrite_hook_lines(&text, None, Some("LINE  # herdr+")).1);
    }
}
