//! The agent wrap (fork): what `herdr agent wrap <claude|codex>` adds to a
//! plain `claude` / `codex` launch in a herdr+ pane, per `[agents]`.
//!
//! The pieces are pure: [`plan`] reads the config and the process facts
//! ([`WrapEnv`]) once, [`wrap_args`] builds the argv. The shell hook (the
//! managed zsh file, `crate::browser::setup`) routes the plain commands
//! here; managed launches (the coordinator's, which already carry the
//! herdr_agents server) also pass through and get only the browser
//! contributions. The settings section's facts ([`settings_snapshot`]) and
//! its one confirmed file edit ([`fix_shell_hook`]) live here too.
//!
//! A launch in a team group gets the team bits ([`team`]) whatever the
//! master switch says (`[agents] team_roster`): the herdr_agents server
//! with the team tools, the roster in the system prompt and, for Claude,
//! the per-turn hook settings. Nothing else of the wrap comes with them.

pub mod instructions;
pub mod team;

use std::path::{Path, PathBuf};

use crate::browser::setup::{self, HookState, SetupEnv};
use crate::config::{Config, WrapSource};
use crate::coordinator::launch::{self, LaunchCtx, MCP_KEY};

/// The per-launch opt-out flag, stripped before it reaches the agent.
pub const OPT_OUT_FLAG: &str = "--no-herdr";
/// `HERDR_NO_WRAP=1` turns the wrap off for one launch.
pub const NO_WRAP_ENV: &str = "HERDR_NO_WRAP";
/// Set by the wrap on the agent it execs (to the pane, `$HERDR_PANE_ID`):
/// a wrap that finds it equal to its own pane runs inside that agent (a
/// `claude -p` or `codex exec` from its Bash tool), not as the pane's agent.
pub const WRAPPED_PANE_ENV: &str = "HERDR_WRAPPED_PANE";
/// The herdr_agents tools a wrapped launch pre-approves (agents v2: every
/// agent has the tools, and the herdr server checks what it may do): every
/// tool but close and reopen, which ask the user first as a courtesy.
pub fn wrap_tools() -> Vec<&'static str> {
    crate::coordinator::mcp::preapproved_tools().collect()
}
/// Codex's per-tool approval (`mcp_servers.<key>.tools.<tool>.approval_mode`):
/// the key parses with codex-cli 0.160.0 (`codex mcp get`) and unknown keys
/// are tolerated, so emitting it cannot break a launch. Whether it is
/// honoured (no prompt on the first `agents_notify`) is checked in the demo.
const CODEX_PER_TOOL_APPROVAL: bool = true;

/// Arguments that pass straight through (no wrap contributions): the agents'
/// management subcommands and version/help flags, as the first argument.
const CLAUDE_PASSTHROUGH: [&str; 11] = [
    "mcp",
    "config",
    "doctor",
    "update",
    "install",
    "setup-token",
    "plugin",
    "-v",
    "--version",
    "-h",
    "--help",
];
const CODEX_PASSTHROUGH: [&str; 12] = [
    "login",
    "logout",
    "mcp",
    "mcp-server",
    "completion",
    "help",
    "apply",
    "features",
    "-V",
    "--version",
    "-h",
    "--help",
];

/// Herdr-owned launches (restore, restart, activate, reopen, `agent.start`)
/// type the wrap verb only where the verb execs the agent in place, so the
/// pane's foreground process is the agent itself (on Windows the verb waits
/// on a child and would stay the foreground process).
pub const RELAUNCH_THROUGH_WRAP: bool = cfg!(unix);

/// The argv a herdr-owned launch of `agent` types instead of the native
/// `native` (`claude --resume <id>`, `codex resume <id>`, an `agent.start`
/// argv): `<herdr> agent wrap <kind> -- <native args…>`. The verb composes
/// the launch exactly as for a typed `claude` / `codex` (team lookup,
/// `HERDR_NO_WRAP`, nested guard, claude-z, codex `--no-daemon`) and picks
/// the binary itself, so the native executable name is dropped. `None`: not
/// a wrap launch (another agent, or a managed argv that already carries the
/// herdr_agents server and stays exactly as built).
pub fn relaunch_argv(
    herdr_bin: &Path,
    agent: &str,
    native: &[String],
    coordinator_dir: &Path,
) -> Option<Vec<String>> {
    let kind = match agent {
        "claude" => "claude",
        "codex" => "codex",
        _ => return None,
    };
    let args = native.get(1..)?;
    let managed = if kind == "claude" {
        is_managed(split_at_dashes(args).0, coordinator_dir)
    } else {
        is_managed(args, coordinator_dir)
    };
    if managed {
        return None;
    }
    let mut argv = vec![
        herdr_bin.display().to_string(),
        "agent".to_string(),
        "wrap".to_string(),
        kind.to_string(),
        "--".to_string(),
    ];
    argv.extend(args.iter().cloned());
    Some(argv)
}

/// The process facts a plan needs, read once per launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapEnv {
    /// `HERDR_NO_WRAP` is set (non-empty, not `0`).
    pub herdr_no_wrap: bool,
    /// The herdr binary the MCP server runs from: `$HERDR_BIN_PATH` when
    /// executable, else this process.
    pub herdr_bin: PathBuf,
    /// The coordinator directory (`--dir` of the MCP server; managed launches
    /// point their Claude MCP config into it).
    pub coordinator_dir: PathBuf,
    /// `[coordinator] dashboard_port`.
    pub dashboard_port: u16,
    /// The user's own top-level `developer_instructions` from Codex's
    /// `config.toml` (kept ahead of herdr's text; profiles are not covered).
    pub codex_own_instructions: Option<String>,
    /// `$HOME`, for `~` in `instructions_file`.
    pub home: Option<PathBuf>,
    /// `$HERDR_PANE_ID`: the pane the team lookup asks about.
    pub pane_id: Option<String>,
    /// The launching pane's team (the CLI's `team.context` lookup, done
    /// after the argument checks); `None` outside team groups.
    pub team: Option<team::TeamLaunch>,
    /// Started inside an agent this wrap already launched in the same pane
    /// ([`WRAPPED_PANE_ENV`]): it shares the pane's id but is not the pane's
    /// agent, so it gets no team identity and no herdr_agents server (any
    /// call would act, and ack team updates, as the pane's agent).
    pub nested: bool,
}

impl WrapEnv {
    pub fn from_process(config: &Config) -> Self {
        let herdr_bin = std::env::var_os("HERDR_BIN_PATH")
            .map(PathBuf::from)
            .filter(|path| setup::is_executable(path))
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_else(|| PathBuf::from("herdr"));
        let pane_id = std::env::var("HERDR_PANE_ID")
            .ok()
            .filter(|v| !v.trim().is_empty());
        Self {
            herdr_no_wrap: std::env::var(NO_WRAP_ENV)
                .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0"),
            herdr_bin,
            coordinator_dir: crate::coordinator::coordinator_dir(),
            dashboard_port: config.coordinator.dashboard_port,
            codex_own_instructions: codex_developer_instructions(),
            home: std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            pane_id: pane_id.clone(),
            team: None,
            nested: is_nested(
                pane_id.as_deref(),
                std::env::var(WRAPPED_PANE_ENV).ok().as_deref(),
            ),
        }
    }
}

/// Whether a wrap in `pane` runs inside an agent the wrap already launched
/// there (`wrapped`: [`WRAPPED_PANE_ENV`] as inherited).
pub fn is_nested(pane: Option<&str>, wrapped: Option<&str>) -> bool {
    match (pane.map(str::trim), wrapped.map(str::trim)) {
        (Some(pane), Some(wrapped)) => !pane.is_empty() && pane == wrapped,
        _ => false,
    }
}

/// `$CODEX_HOME/config.toml`, else `$HOME/.codex/config.toml`.
fn codex_config_path() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".codex"))
        })
        .map(|dir| dir.join("config.toml"))
}

/// The user's own top-level `developer_instructions` from Codex's config.toml.
pub(crate) fn codex_developer_instructions() -> Option<String> {
    let text = std::fs::read_to_string(codex_config_path()?).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("developer_instructions")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// What one launch gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapPlan {
    /// The wrap is on for this launch (`[agents] wrap`, no opt-out).
    pub master: bool,
    /// Add the herdr_agents server (unmanaged launches only).
    pub tools: bool,
    /// The herdr+ paragraph (unmanaged launches only).
    pub instructions: Option<String>,
    /// The browser steering text (`[browser] steer_agents`).
    pub steer: bool,
    /// Turn the agents' own browsers off (`[browser] disable_native_browser`).
    pub no_native: bool,
    /// Where the MCP server runs from and with which directory.
    pub ctx: LaunchCtx,
    /// The user's own Codex `developer_instructions`.
    pub codex_own: Option<String>,
    /// The team bits (a launch in a team group, `[agents] team_roster`):
    /// forces the herdr_agents server with the team tools, adds the roster
    /// to the prompt; never the paragraph, steering or `no_native`.
    pub team: Option<team::TeamLaunch>,
    /// Claude gets the team's `--settings` hook file (off when it could not
    /// be written).
    pub team_hook: bool,
    /// Things the user should know (an unusable instructions file).
    pub warnings: Vec<String>,
}

impl WrapPlan {
    /// Off for this launch: the agent runs as typed (Codex keeps `--no-daemon`).
    pub fn disable(&mut self) {
        self.master = false;
        self.tools = false;
        self.instructions = None;
        self.steer = false;
        self.no_native = false;
        self.team = None;
        self.team_hook = false;
    }
}

/// The plan for a launch under `config`.
pub fn plan(config: &Config, env: &WrapEnv) -> WrapPlan {
    let master = config.agents_wrap().0 && !env.herdr_no_wrap;
    let team = env
        .team
        .clone()
        .filter(|_| config.agents.team_roster && !env.herdr_no_wrap && !env.nested);
    let tools = master && config.agents.tools && !env.nested;
    let mut warnings = Vec::new();
    let in_team = team.is_some();
    let instructions =
        (master && config.agents.instructions).then(|| match config.agents.instructions_file() {
            None => instructions::default_paragraph(tools, in_team),
            Some(file) => {
                let (text, warning) =
                    instructions::resolve(file, env.home.as_deref(), tools, in_team);
                warnings.extend(warning);
                text
            }
        });
    WrapPlan {
        master,
        instructions,
        steer: master && config.browser.steer_agents,
        no_native: master && config.browser.disable_native_browser,
        ctx: LaunchCtx {
            herdr_bin: env.herdr_bin.clone(),
            dir: env.coordinator_dir.clone(),
            port: env.dashboard_port,
        },
        codex_own: env.codex_own_instructions.clone(),
        team_hook: team.is_some(),
        // the team forces the server (with the team tools)
        tools: tools || team.is_some(),
        team,
        warnings,
    }
}

/// `user` without `--no-herdr` before its first `--` (after it, the flag is
/// the agent's text), and whether one was there.
pub fn strip_opt_out(user: &[String]) -> (Vec<String>, bool) {
    let split = user.iter().position(|a| a == "--").unwrap_or(user.len());
    let mut out: Vec<String> = Vec::with_capacity(user.len());
    let mut found = false;
    for arg in &user[..split] {
        if arg == OPT_OUT_FLAG {
            found = true;
        } else {
            out.push(arg.clone());
        }
    }
    out.extend(user[split..].iter().cloned());
    (out, found)
}

/// Whether the first argument is a management subcommand or a version/help
/// flag that runs without any wrap contribution.
pub fn passthrough(agent: &str, user: &[String]) -> bool {
    let Some(first) = user.first().map(String::as_str) else {
        return false;
    };
    match agent {
        "claude" => CLAUDE_PASSTHROUGH.contains(&first),
        "codex" => CODEX_PASSTHROUGH.contains(&first),
        _ => false,
    }
}

/// The value of `flag` at `args[i]`: `--flag=value` or `--flag value`.
fn flag_value<'a>(args: &'a [String], i: usize, names: &[&str]) -> Option<&'a str> {
    let arg = args[i].as_str();
    for name in names {
        if arg == *name {
            return args.get(i + 1).map(String::as_str);
        }
        if let Some(value) = arg
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(value);
        }
    }
    None
}

const ALLOWED_TOOLS: [&str; 2] = ["--allowedTools", "--allowed-tools"];
const CODEX_CONFIG: [&str; 2] = ["-c", "--config"];

/// A managed launch (the coordinator's own argv, `launch::claude_args` /
/// `codex_args`): it already has the herdr_agents server, so the wrap adds
/// none of its herdr_agents arguments nor the paragraph. Any one of the
/// signs is enough (the coordinator directory need not match this process's).
pub fn is_managed(user: &[String], coordinator_dir: &Path) -> bool {
    let config_path = launch::claude_mcp_config_path(coordinator_dir);
    let agents_tools = format!("mcp__{MCP_KEY}");
    let codex_key = format!("mcp_servers.{MCP_KEY}.");
    (0..user.len()).any(|i| {
        flag_value(user, i, &["--mcp-config"]).is_some_and(|v| Path::new(v) == config_path)
            || flag_value(user, i, &ALLOWED_TOOLS).is_some_and(|v| v.contains(&agents_tools))
            || flag_value(user, i, &CODEX_CONFIG)
                .is_some_and(|v| v.trim_start().starts_with(&codex_key))
    })
}

/// The arguments before the first `--` (Claude's flags) and the rest.
fn split_at_dashes(user: &[String]) -> (&[String], &[String]) {
    let split = user.iter().position(|a| a == "--").unwrap_or(user.len());
    user.split_at(split)
}

/// Whether `wrap_args` will point Claude at `<dir>/mcp/claude.json` (the
/// verb writes the file first; the team settings file goes through the
/// same gate).
pub fn uses_claude_mcp_config(plan: &WrapPlan, user: &[String]) -> bool {
    ((plan.master && plan.tools) || plan.team.is_some())
        && !passthrough("claude", user)
        && !is_managed(split_at_dashes(user).0, &plan.ctx.dir)
}

/// Whether `pre` (Claude's flags) already has `--settings[=…]`.
fn has_settings_flag(pre: &[String]) -> bool {
    pre.iter()
        .any(|a| a == "--settings" || a.starts_with("--settings="))
}

/// Whether `pre` already has a system prompt flag of its own.
fn has_append_prompt(pre: &[String]) -> bool {
    pre.iter().any(|a| {
        ["--append-system-prompt", "--append-system-prompt-file"]
            .iter()
            .any(|flag| a == flag || a.strip_prefix(flag).is_some_and(|r| r.starts_with('=')))
    })
}

/// Whether the user's Codex arguments set `developer_instructions`.
fn codex_sets_instructions(user: &[String]) -> bool {
    (0..user.len()).any(|i| {
        flag_value(user, i, &CODEX_CONFIG)
            .is_some_and(|v| v.trim_start().starts_with("developer_instructions"))
    })
}

/// Whether the team settings file is used by this launch (the CLI writes it
/// first, on the exec path only).
pub fn uses_team_settings(plan: &WrapPlan, user: &[String]) -> bool {
    plan.team.is_some()
        && plan.team_hook
        && uses_claude_mcp_config(plan, user)
        && !has_settings_flag(split_at_dashes(user).0)
}

/// What the user's own flags take away from the team bits, as warnings for
/// stderr (`wrap_args` stays pure and silent).
pub fn team_conflicts(agent: &str, plan: &WrapPlan, user: &[String]) -> Vec<String> {
    if plan.team.is_none() || passthrough(agent, user) {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    match agent {
        "claude" => {
            let pre = split_at_dashes(user).0;
            if is_managed(pre, &plan.ctx.dir) {
                return warnings;
            }
            if has_settings_flag(pre) {
                warnings.push(
                    "team updates per turn are off (your --settings wins); they still arrive with the agents_* tool results"
                        .to_string(),
                );
            }
            if has_append_prompt(pre) {
                warnings.push(
                    "the team roster is not in the system prompt (your --append-system-prompt wins); agents_whoami and the first turn's hook carry it"
                        .to_string(),
                );
            }
        }
        "codex" if !is_managed(user, &plan.ctx.dir) && codex_sets_instructions(user) => {
            warnings.push(
                "the team roster is not in developer_instructions (your -c developer_instructions wins); agents_whoami and the tool results carry it"
                    .to_string(),
            );
        }
        _ => {}
    }
    warnings
}

/// The team text, the paragraph and the steering text (unmanaged: all;
/// managed: steering only — a managed team launch carries its roster).
fn prompt_parts(plan: &WrapPlan, managed: bool) -> Vec<&str> {
    let mut parts = Vec::new();
    if !managed {
        if let Some(text) = plan
            .team
            .as_ref()
            .map(|team| team.text.trim())
            .filter(|t| !t.is_empty())
        {
            parts.push(text);
        }
        if let Some(text) = plan
            .instructions
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            parts.push(text);
        }
    }
    if plan.steer {
        parts.push(crate::cli::BROWSER_STEERING);
    }
    parts
}

/// The comma list for Claude's allowlist: the same for every launch (a
/// team member's included).
fn claude_allow_list(_plan: &WrapPlan) -> String {
    claude_allow_list_value()
}

/// `mcp__herdr_agents__<tool>,…` for [`wrap_tools`].
pub(crate) fn claude_allow_list_value() -> String {
    wrap_tools()
        .iter()
        .map(|tool| format!("mcp__{MCP_KEY}__{tool}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The tools a Codex launch pre-approves.
fn codex_approved_tools(_plan: &WrapPlan) -> Vec<&'static str> {
    wrap_tools()
}

/// Merge `allow` into the first user `--allowedTools` value of `pre`
/// (never a second flag); `false` when there is none.
fn merge_allowed_tools(pre: &mut Vec<String>, allow: &str) -> bool {
    for i in 0..pre.len() {
        let arg = pre[i].clone();
        for name in ALLOWED_TOOLS {
            if arg == name {
                match pre.get_mut(i + 1) {
                    Some(value) if !value.starts_with('-') => {
                        value.push(',');
                        value.push_str(allow);
                    }
                    _ => pre.insert(i + 1, allow.to_string()),
                }
                return true;
            }
            if let Some(value) = arg.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                pre[i] = if value.is_empty() {
                    format!("{name}={allow}")
                } else {
                    format!("{name}={value},{allow}")
                };
                return true;
            }
        }
    }
    false
}

/// The argv handed to the agent (after its executable). Codex: herdr's flags
/// first, then the user's (subcommands like `resume` stay after them).
/// Claude: the user's flags first (claude-z reads its session id from `$1`),
/// herdr's after them and before a user `--`, and never a flag the user
/// already passed. Variadic flags take their `=` form.
pub fn wrap_args(agent: &str, plan: &WrapPlan, user: &[String]) -> Vec<String> {
    let off = (!plan.master && plan.team.is_none()) || passthrough(agent, user);
    match agent {
        "codex" => {
            // The daemon would spawn MCP servers with another pane's
            // environment: attribution needs this even when wrapping is off.
            let mut args: Vec<String> = Vec::new();
            if !user.iter().any(|a| a == "--no-daemon") {
                args.push("--no-daemon".into());
            }
            if !off {
                let managed = is_managed(user, &plan.ctx.dir);
                if plan.no_native {
                    args.extend(
                        ["--disable", "in_app_browser", "--disable", "browser_use"]
                            .map(String::from),
                    );
                }
                if plan.tools && !managed {
                    args.extend(launch::codex_mcp_overrides(&plan.ctx, false));
                    if CODEX_PER_TOOL_APPROVAL {
                        for tool in codex_approved_tools(plan) {
                            args.push("-c".into());
                            args.push(format!(
                                "mcp_servers.{MCP_KEY}.tools.{tool}.approval_mode=\"approve\""
                            ));
                        }
                    }
                }
                let parts = prompt_parts(plan, managed);
                if !parts.is_empty() && !codex_sets_instructions(user) {
                    let mut all: Vec<&str> = Vec::new();
                    if let Some(own) = plan
                        .codex_own
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        all.push(own);
                    }
                    all.extend(parts);
                    args.push("-c".into());
                    args.push(format!(
                        "developer_instructions={}",
                        launch::toml_string(&all.join("\n\n"))
                    ));
                }
            }
            args.extend(user.iter().cloned());
            args
        }
        "claude" => {
            if off {
                return user.to_vec();
            }
            let (pre, post) = split_at_dashes(user);
            let managed = is_managed(pre, &plan.ctx.dir);
            let mut pre = pre.to_vec();
            let mut ours: Vec<String> = Vec::new();
            if plan.tools && !managed {
                ours.push(launch::mcp_config_flag(&launch::claude_mcp_config_path(
                    &plan.ctx.dir,
                )));
                let allow = claude_allow_list(plan);
                if !merge_allowed_tools(&mut pre, &allow) {
                    ours.push(format!("--allowedTools={allow}"));
                }
            }
            if plan.team.is_some() && plan.team_hook && !managed && !has_settings_flag(&pre) {
                ours.push(team::settings_flag(&team::claude_settings_path(
                    &plan.ctx.dir,
                )));
            }
            let prompt = prompt_parts(plan, managed).join("\n\n");
            if !prompt.is_empty() && !has_append_prompt(&pre) {
                ours.push("--append-system-prompt".into());
                ours.push(prompt);
            }
            if plan.no_native && !pre.iter().any(|a| a == "--no-chrome") {
                ours.push("--no-chrome".into());
            }
            pre.extend(ours);
            pre.extend(post.iter().cloned());
            pre
        }
        _ => user.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// The settings section's facts and its confirmed fix

/// One status entry of the Agents section (`shell_hook`, `claude`, `codex`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapCheck {
    pub id: &'static str,
    /// `ok`, `outdated`, `missing` (the hook) or `absent` (not on PATH).
    pub state: &'static str,
    pub detail: String,
    /// A fix is offered (only the hook has one).
    pub fixable: bool,
    /// The fix edits the user's files: it needs an explicit confirmation.
    pub edits_files: bool,
}

/// The Agents section's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapSnapshot {
    pub wrap: bool,
    pub wrap_source: WrapSource,
    pub tools: bool,
    pub instructions: bool,
    /// `[agents] instructions_file` as written (empty: built-in).
    pub instructions_file: String,
    /// `built-in`, `~/.config/herdr/agents.md (412 B)`, `… missing → built-in`.
    pub instructions_detail: String,
    /// `[browser] steer_agents` (applies while wrapped; read-only here).
    pub steer_browser: bool,
    pub notices: bool,
    /// `[agents] team_roster` (the team-only launch wrap).
    pub team_roster: bool,
    pub checks: Vec<WrapCheck>,
    /// The exact `.zshrc` line the hook fix adds and where, when it is offered.
    pub hook_preview: Option<String>,
}

/// The section's facts under `config`, with the hook and PATH facts from
/// `env` (the server's environment). Never writes.
pub fn settings_snapshot(config: &Config, env: &SetupEnv) -> WrapSnapshot {
    let (wrap, wrap_source) = config.agents_wrap();
    let hook = setup::shell_hook_check(env);
    // With the wrap off a missing hook is fine; an outdated one is broken in
    // agents' shells whatever the setting, so it stays fixable.
    let hook_fixable = hook.fixable
        && match hook.state {
            HookState::Ok => false,
            HookState::Outdated => true,
            HookState::Missing => wrap,
        };
    let hook_preview = match (&env.zshrc, hook_fixable) {
        (Some(zshrc), true) => Some(format!(
            "{}: {}",
            setup::shorten_home(zshrc, env.home.as_deref()),
            setup::zshrc_hook_line(&env.shell_file)
        )),
        _ => None,
    };
    let on_path = |id: &'static str, found: bool, detail: String| WrapCheck {
        id,
        state: if found { "ok" } else { "absent" },
        detail,
        fixable: false,
        edits_files: false,
    };
    let claude = match (&env.claude_z_bin, &env.claude_bin) {
        (Some(_), _) => on_path(
            "claude",
            true,
            "claude-z on the server's PATH (the wrap runs it)".into(),
        ),
        (None, Some(_)) => on_path("claude", true, "on the server's PATH".into()),
        (None, None) => on_path(
            "claude",
            false,
            "not on the server's PATH (nor claude-z)".into(),
        ),
    };
    let codex = on_path(
        "codex",
        env.codex_bin.is_some(),
        if env.codex_bin.is_some() {
            "on the server's PATH".into()
        } else {
            "not on the server's PATH".into()
        },
    );
    WrapSnapshot {
        wrap,
        wrap_source,
        tools: config.agents.tools,
        instructions: config.agents.instructions,
        instructions_file: config.agents.instructions_file.clone(),
        instructions_detail: instructions::file_detail(
            &config.agents.instructions_file,
            env.home.as_deref(),
        ),
        steer_browser: config.browser.steer_agents,
        notices: config.agents.notices,
        team_roster: config.agents.team_roster,
        checks: vec![
            WrapCheck {
                id: "shell_hook",
                state: hook.state.as_str(),
                detail: hook.detail,
                fixable: hook_fixable,
                edits_files: hook.edits_files,
            },
            claude,
            codex,
        ],
        hook_preview,
    }
}

/// The confirmed hook fix: write the managed file and make the guarded line
/// in `.zshrc` this instance's (backing `.zshrc` up once). Only ever called
/// on an explicit, confirmed request.
pub fn fix_shell_hook(env: &SetupEnv) -> Result<String, String> {
    setup::install_shell_hook(env, true)
}

/// Create the instructions file at `path` with the built-in paragraph when
/// it does not exist; an existing file is never overwritten.
pub fn seed_instructions_file(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(instructions::DEFAULT_NOTIFY_PARAGRAPH.as_bytes())?;
            file.write_all(b"\n")
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A temporary home for tests that write config, `.zshrc`, `agents.md`
    //! or `mcp/claude.json`: every variable those paths derive from points
    //! into one temp directory while the guard lives (restored on drop), and
    //! the process-wide config env lock is held so tests that touch these
    //! variables never interleave.

    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use crate::browser::setup::SetupEnv;

    /// The variables set (or removed, `None`) while a [`TempHome`] lives.
    const VARS: [&str; 9] = [
        "HOME",
        "ZDOTDIR",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        crate::config::CONFIG_PATH_ENV_VAR,
        crate::coordinator::COORDINATOR_DIR_ENV,
        super::NO_WRAP_ENV,
        // never ask a live herdr about this process's pane (the team lookup)
        "HERDR_PANE_ID",
    ];

    pub(crate) struct TempHome {
        pub root: PathBuf,
        pub home: PathBuf,
        pub config_path: PathBuf,
        pub env: SetupEnv,
        saved: Vec<(&'static str, Option<OsString>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TempHome {
        pub(crate) fn new(name: &str) -> Self {
            let lock = crate::config::test_config_env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let root = std::env::temp_dir().join(format!(
                "herdr-temphome-{name}-{}-{}",
                std::process::id(),
                crate::coordinator::launch::new_uuid()
            ));
            let home = root.join("home");
            std::fs::create_dir_all(home.join(".codex")).expect("temp home");
            let saved = VARS
                .iter()
                .map(|var| (*var, std::env::var_os(var)))
                .collect();
            std::env::set_var("HOME", &home);
            std::env::set_var("ZDOTDIR", &home);
            std::env::set_var("CODEX_HOME", home.join(".codex"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("xdg-config"));
            std::env::set_var("XDG_STATE_HOME", root.join("xdg-state"));
            std::env::remove_var(crate::coordinator::COORDINATOR_DIR_ENV);
            std::env::remove_var(super::NO_WRAP_ENV);
            std::env::remove_var("HERDR_PANE_ID");
            let config_path = crate::config::config_dir().join("config.toml");
            std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &config_path);
            let env = SetupEnv {
                browser_home: root.join("browser"),
                binary: root.join("bin/herdr"),
                shell_file: crate::browser::setup::shell_file_path(),
                zshrc: Some(home.join(".zshrc")),
                claude_json: Some(home.join(".claude.json")),
                // never a real CLI: a fix would run it
                claude_bin: None,
                claude_z_bin: None,
                codex_config: Some(home.join(".codex/config.toml")),
                codex_bin: None,
                home: Some(home.clone()),
                node_override: None,
            };
            Self {
                root,
                home,
                config_path,
                env,
                saved,
                _lock: lock,
            }
        }

        pub(crate) fn write_config(&self, text: &str) {
            if let Some(parent) = self.config_path.parent() {
                std::fs::create_dir_all(parent).expect("config dir");
            }
            std::fs::write(&self.config_path, text).expect("config");
        }

        pub(crate) fn contains(&self, path: &Path) -> bool {
            path.starts_with(&self.root)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            for (var, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(var, value),
                    None => std::env::remove_var(var),
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests;
