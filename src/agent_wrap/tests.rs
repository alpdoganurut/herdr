use super::test_support::TempHome;
use super::*;
use crate::cli::BROWSER_STEERING;
use crate::coordinator::launch::{claude_allow_agent, ClaudeSession};

fn s(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn config(text: &str) -> Config {
    let mut config: Config = toml::from_str(text).unwrap();
    config.resolve_derived();
    config
}

const DIR: &str = "/home/u/.config/herdr-dev/coordinator";

fn env() -> WrapEnv {
    WrapEnv {
        herdr_no_wrap: false,
        herdr_bin: PathBuf::from("/opt/herdr/herdr"),
        coordinator_dir: PathBuf::from(DIR),
        dashboard_port: crate::coordinator::DEFAULT_PORT,
        codex_own_instructions: None,
        home: None,
        pane_id: Some("w3:p1".into()),
        team: None,
        nested: false,
    }
}

/// `[agents]` with the three switches; `disable_native_browser` keeps its
/// default (on).
fn plan_for(tools: bool, instructions: bool, steer: bool) -> WrapPlan {
    plan(
        &config(&format!(
            "[agents]\nwrap = true\ntools = {tools}\ninstructions = {instructions}\n[browser]\nsteer_agents = {steer}\n"
        )),
        &env(),
    )
}

fn claude_flag() -> String {
    format!("--mcp-config={DIR}/mcp/claude.json")
}

/// The notes recall settings of a wrapped launch outside teams.
fn wrap_settings_flag() -> String {
    format!("--settings={DIR}/wrap/claude-settings.json")
}

/// Every wrapped launch's allowlist: all herdr_agents tools but close and reopen.
fn claude_allow() -> String {
    format!("--allowedTools={}", claude_allow_list_value())
}

fn developer_instructions(args: &[String]) -> Option<String> {
    let value = args
        .iter()
        .find_map(|a| a.strip_prefix("developer_instructions="))?;
    let parsed: toml::Value = toml::from_str(&format!("x = {value}")).unwrap();
    parsed["x"].as_str().map(str::to_string)
}

fn count_herdr_agents(args: &[String]) -> usize {
    args.iter().filter(|a| a.contains("herdr_agents")).count()
}

#[test]
fn master_off_runs_the_agents_as_typed_except_codex_no_daemon() {
    for text in [
        "",
        "[agents]\nwrap = false\ntools = true\ninstructions = true\n",
        "[browser]\nwrap_agents = false\n",
    ] {
        let plan = plan(&config(text), &env());
        assert!(
            !plan.master && !plan.tools && plan.instructions.is_none(),
            "{text}"
        );
        assert!(!plan.steer && !plan.no_native);
        assert_eq!(
            wrap_args("codex", &plan, &s(&["resume", "x"])),
            ["--no-daemon", "resume", "x"]
        );
        // deduplicated: the old wrap repeated it
        assert_eq!(
            wrap_args("codex", &plan, &s(&["--no-daemon", "x"])),
            ["--no-daemon", "x"]
        );
        assert_eq!(
            wrap_args("claude", &plan, &s(&["uuid", "-p", "hi"])),
            ["uuid", "-p", "hi"]
        );
    }
    // the legacy key on: the wrap is on
    let legacy = plan(&config("[browser]\nwrap_agents = true\n"), &env());
    assert!(legacy.master && legacy.steer && legacy.no_native);
    // HERDR_NO_WRAP: off for this launch
    let off = plan(
        &config("[agents]\nwrap = true\ntools = true\n"),
        &WrapEnv {
            herdr_no_wrap: true,
            ..env()
        },
    );
    assert!(!off.master && !off.tools);
    assert_eq!(wrap_args("codex", &off, &[]), ["--no-daemon"]);
}

#[test]
fn every_switch_combination_builds_the_expected_claude_argv() {
    for tools in [false, true] {
        for instructions in [false, true] {
            for steer in [false, true] {
                let plan = plan_for(tools, instructions, steer);
                let args = wrap_args("claude", &plan, &s(&["0000-uuid"]));
                let label =
                    format!("tools={tools} instructions={instructions} steer={steer}: {args:?}");
                assert_eq!(args[0], "0000-uuid", "claude-z's $1 stays first: {label}");
                assert_eq!(args.contains(&claude_flag()), tools, "{label}");
                assert_eq!(args.iter().any(|a| *a == claude_allow()), tools, "{label}");
                let prompt = args
                    .iter()
                    .position(|a| a == "--append-system-prompt")
                    .map(|i| args[i + 1].clone());
                assert_eq!(prompt.is_some(), instructions || steer, "{label}");
                if let Some(prompt) = prompt {
                    assert_eq!(
                        prompt.contains(instructions::INTRO),
                        instructions,
                        "{label}"
                    );
                    assert_eq!(
                        prompt.contains("agents_notify"),
                        instructions && tools,
                        "the notify sentence needs the tools: {label}"
                    );
                    assert_eq!(prompt.contains(BROWSER_STEERING), steer, "{label}");
                    if instructions && steer {
                        assert!(
                            prompt.ends_with(&format!("\n\n{BROWSER_STEERING}")),
                            "{label}"
                        );
                    }
                }
                assert_eq!(
                    args.last().map(String::as_str),
                    Some("--no-chrome"),
                    "{label}"
                );
                // variadic flags only in their = form
                assert!(!args
                    .iter()
                    .any(|a| a == "--mcp-config" || a == "--allowedTools"));
            }
        }
    }
    // all on, exactly
    let plan = plan_for(true, true, true);
    let prompt = format!(
        "{}\n\n{BROWSER_STEERING}",
        instructions::default_paragraph(true, false)
    );
    assert_eq!(
        wrap_args("claude", &plan, &s(&["-p", "--", "prompt text"])),
        [
            "-p".to_string(),
            claude_flag(),
            claude_allow(),
            "--append-system-prompt".to_string(),
            prompt,
            "--no-chrome".to_string(),
            "--".to_string(),
            "prompt text".to_string(),
        ],
        "ours go before a user `--`"
    );
}

#[test]
fn every_switch_combination_builds_the_expected_codex_argv() {
    for tools in [false, true] {
        for instructions in [false, true] {
            for steer in [false, true] {
                let plan = plan_for(tools, instructions, steer);
                let args = wrap_args("codex", &plan, &s(&["resume", "--last"]));
                let label =
                    format!("tools={tools} instructions={instructions} steer={steer}: {args:?}");
                assert_eq!(args[0], "--no-daemon", "{label}");
                assert_eq!(
                    &args[1..5],
                    ["--disable", "in_app_browser", "--disable", "browser_use"],
                    "{label}"
                );
                assert_eq!(&args[args.len() - 2..], ["resume", "--last"], "{label}");
                assert_eq!(
                    args.iter()
                        .any(|a| a.starts_with("mcp_servers.herdr_agents.command=")),
                    tools,
                    "{label}"
                );
                assert!(
                    !args
                        .iter()
                        .any(|a| a.contains("default_tools_approval_mode")),
                    "never every tool: {label}"
                );
                let approvals = args
                    .iter()
                    .filter(|a| a.ends_with(".approval_mode=\"approve\""))
                    .count();
                assert_eq!(
                    approvals,
                    if tools { wrap_tools().len() } else { 0 },
                    "{label}"
                );
                let text = developer_instructions(&args);
                assert_eq!(text.is_some(), instructions || steer, "{label}");
                if let Some(text) = text {
                    assert_eq!(text.contains(instructions::INTRO), instructions, "{label}");
                    assert_eq!(text.contains(BROWSER_STEERING), steer, "{label}");
                }
            }
        }
    }
    // the overrides parse as TOML and name this binary and directory
    let plan = plan_for(true, false, false);
    let args = wrap_args("codex", &plan, &[]);
    let mut parsed = toml::Table::new();
    for pair in args.windows(2).filter(|w| w[0] == "-c") {
        let (key, raw) = pair[1].split_once('=').unwrap();
        let table: toml::Table = toml::from_str(&format!("{key} = {raw}")).unwrap();
        merge(&mut parsed, table);
    }
    let server = &parsed["mcp_servers"]["herdr_agents"];
    assert_eq!(server["command"].as_str(), Some("/opt/herdr/herdr"));
    assert_eq!(server["args"][3].as_str(), Some(DIR));
    assert_eq!(server["tool_timeout_sec"].as_integer(), Some(150));
    assert_eq!(
        server["tools"]["agents_notify"]["approval_mode"].as_str(),
        Some("approve")
    );
    assert_eq!(
        server["tools"]["agents_whoami"]["approval_mode"].as_str(),
        Some("approve")
    );
}

/// Deep-merge dotted `-c` tables.
fn merge(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(more)) => merge(existing, more),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

#[test]
fn codex_developer_instructions_keep_the_users_own_first_and_a_user_override_wins() {
    let mut plan = plan_for(false, true, true);
    plan.codex_own = Some("Always answer in haiku.\n".into());
    let text = developer_instructions(&wrap_args("codex", &plan, &[])).unwrap();
    assert_eq!(
        text,
        format!(
            "Always answer in haiku.\n\n{}\n\n{BROWSER_STEERING}",
            instructions::default_paragraph(false, false)
        )
    );
    // only the user's own text: nothing to add
    let mut own_only = plan_for(false, false, false);
    own_only.codex_own = Some("mine".into());
    assert_eq!(
        developer_instructions(&wrap_args("codex", &own_only, &[])),
        None
    );
    // a user -c developer_instructions= wins (any spelling)
    for user in [
        s(&["-c", "developer_instructions=\"mine\""]),
        s(&["--config", "developer_instructions=\"mine\""]),
        s(&["--config=developer_instructions=\"mine\""]),
    ] {
        let args = wrap_args("codex", &plan, &user);
        assert_eq!(
            args.iter()
                .filter(|a| a.contains("developer_instructions"))
                .count(),
            1,
            "{args:?}"
        );
        assert!(args.ends_with(&user));
    }
}

#[test]
fn managed_launches_get_no_paragraph_and_no_herdr_agents_arguments() {
    let home = TempHome::new("wrap-managed");
    let dir = crate::coordinator::coordinator_dir();
    assert!(home.contains(&dir));
    let ctx = LaunchCtx {
        herdr_bin: PathBuf::from("/opt/herdr/herdr"),
        dir: dir.clone(),
        port: crate::coordinator::DEFAULT_PORT,
    };
    let mut plan = plan_for(true, true, true);
    plan.ctx = ctx.clone();
    // Claude: the coordinator's argv, through the hook
    let managed =
        launch::claude_args(&ctx, &ClaudeSession::New("u1".into()), false, Some("go")).unwrap();
    assert!(is_managed(&managed, &dir));
    let args = wrap_args("claude", &plan, &managed);
    assert_eq!(
        count_herdr_agents(&args),
        count_herdr_agents(&managed),
        "{args:?}"
    );
    let prompt_at = args
        .iter()
        .position(|a| a == "--append-system-prompt")
        .unwrap();
    assert_eq!(
        args[prompt_at + 1],
        BROWSER_STEERING,
        "steering only, no paragraph"
    );
    assert!(args.contains(&"--no-chrome".to_string()));
    assert_eq!(
        &args[args.len() - 2..],
        ["--", "go"],
        "-- and the kickoff stay last"
    );
    assert!(!uses_claude_mcp_config(&plan, &managed));
    // Codex: the managed overrides, no approval -c of ours, no paragraph
    let managed = launch::codex_args(&ctx, Some("You are rev"));
    assert!(is_managed(&managed, &dir));
    let args = wrap_args("codex", &plan, &managed);
    assert_eq!(
        count_herdr_agents(&args),
        count_herdr_agents(&managed),
        "{args:?}"
    );
    assert!(!args
        .iter()
        .any(|a| a.ends_with(".approval_mode=\"approve\"")));
    assert_eq!(
        developer_instructions(&args).as_deref(),
        Some(BROWSER_STEERING)
    );
    assert_eq!(args.last().map(String::as_str), Some("You are rev"));
    drop(home);
}

#[test]
fn any_one_managed_sign_is_enough_whatever_the_directory() {
    let other = Path::new("/elsewhere/coordinator");
    assert!(is_managed(&s(&["--allowedTools=mcp__herdr_agents"]), other));
    assert!(is_managed(
        &s(&["--allowed-tools", "Bash,mcp__herdr_agents"]),
        other
    ));
    assert!(is_managed(
        &s(&["-c", "mcp_servers.herdr_agents.command=\"/x\""]),
        other
    ));
    assert!(is_managed(
        &s(&["--config=mcp_servers.herdr_agents.args=[]"]),
        other
    ));
    assert!(is_managed(
        &s(&["--mcp-config", "/elsewhere/coordinator/mcp/claude.json"]),
        other
    ));
    assert!(is_managed(
        &s(&["--mcp-config=/elsewhere/coordinator/mcp/claude.json"]),
        other
    ));
    assert!(!is_managed(&s(&["--mcp-config=/mine/mcp.json"]), other));
    assert!(!is_managed(&s(&["--allowedTools=Bash"]), other));
    assert!(!is_managed(&s(&["-c", "model=\"o3\""]), other));
}

#[test]
fn user_flags_win_and_the_allowlist_is_merged_into_one_value() {
    let plan = plan_for(true, true, true);
    for user in [
        s(&["uuid", "--append-system-prompt", "mine"]),
        s(&["uuid", "--append-system-prompt=mine"]),
        s(&["uuid", "--append-system-prompt-file", "/p.md"]),
        s(&["uuid", "--append-system-prompt-file=/p.md"]),
    ] {
        let args = wrap_args("claude", &plan, &user);
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--append-system-prompt"))
                .count(),
            1,
            "{args:?}"
        );
        assert!(args.starts_with(&user));
    }
    // --no-chrome is not repeated
    let args = wrap_args("claude", &plan, &s(&["--no-chrome"]));
    assert_eq!(args.iter().filter(|a| *a == "--no-chrome").count(), 1);
    // the user's allowlist gets ours merged, never a second flag
    let allow = claude_allow_list_value();
    let allow = allow.as_str();
    let args = wrap_args(
        "claude",
        &plan,
        &s(&["--allowedTools=Bash(git:*)", "--", "x"]),
    );
    assert_eq!(args[0], format!("--allowedTools=Bash(git:*),{allow}"));
    assert_eq!(
        args.iter()
            .filter(|a| a.starts_with("--allowedTools"))
            .count(),
        1
    );
    let args = wrap_args("claude", &plan, &s(&["--allowed-tools", "Read", "-p"]));
    assert_eq!(
        &args[..3],
        ["--allowed-tools", &format!("Read,{allow}"), "-p"]
    );
    assert!(!args.iter().any(|a| a.starts_with("--allowedTools")));
    // a dangling flag gets ours as its value
    let args = wrap_args("claude", &plan, &s(&["--allowedTools"]));
    assert_eq!(&args[..2], ["--allowedTools", allow]);
    // resume keeps its id first
    let args = wrap_args("claude", &plan, &s(&["--resume", "abc"]));
    assert_eq!(&args[..2], ["--resume", "abc"]);
    assert_eq!(claude_allow_agent(), claude_allow_list_value());
}

#[test]
fn the_opt_out_flag_is_stripped_before_the_double_dash_only() {
    let (user, found) = strip_opt_out(&s(&["uuid", "--no-herdr", "-p", "--", "--no-herdr"]));
    assert!(found);
    assert_eq!(user, ["uuid", "-p", "--", "--no-herdr"]);
    let (user, found) = strip_opt_out(&s(&["--", "--no-herdr"]));
    assert!(!found);
    assert_eq!(user, ["--", "--no-herdr"]);
    let mut plan = plan_for(true, true, true);
    plan.disable();
    assert_eq!(wrap_args("claude", &plan, &user), user);
    assert_eq!(wrap_args("codex", &plan, &s(&["x"])), ["--no-daemon", "x"]);
}

#[test]
fn management_subcommands_pass_through() {
    let plan = plan_for(true, true, true);
    for first in [
        "mcp",
        "config",
        "doctor",
        "update",
        "install",
        "--version",
        "-v",
        "-h",
        "plugin",
        "setup-token",
    ] {
        let user = s(&[first, "list"]);
        assert!(passthrough("claude", &user), "{first}");
        assert_eq!(wrap_args("claude", &plan, &user), user, "{first}");
        assert!(!uses_claude_mcp_config(&plan, &user));
    }
    for first in [
        "login",
        "logout",
        "mcp",
        "mcp-server",
        "completion",
        "help",
        "apply",
        "features",
        "-V",
        "--help",
    ] {
        let user = s(&[first]);
        assert!(passthrough("codex", &user), "{first}");
        assert_eq!(
            wrap_args("codex", &plan, &user),
            ["--no-daemon", first],
            "{first}"
        );
    }
    assert!(!passthrough("claude", &s(&["0000-uuid", "mcp"])));
    assert!(!passthrough("codex", &s(&["resume"])));
    assert!(!passthrough("claude", &[]));
}

fn plan_with_file(file: &str, wrap_env: &WrapEnv) -> WrapPlan {
    plan(
        &config(&format!(
            "[agents]\nwrap = true\ninstructions = true\ninstructions_file = \"{file}\"\n[browser]\nsteer_agents = false\n"
        )),
        wrap_env,
    )
}

#[test]
fn an_instructions_file_replaces_the_paragraph_and_a_bad_one_warns() {
    let home = TempHome::new("wrap-file");
    std::fs::write(home.home.join("agents.md"), "Use -c sparingly.\u{7}\n").unwrap();
    let wrap_env = WrapEnv {
        home: Some(home.home.clone()),
        ..env()
    };
    let plan = plan_with_file("~/agents.md", &wrap_env);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    assert_eq!(
        plan.instructions.as_deref(),
        Some("Use \u{2011}c sparingly.")
    );
    let args = wrap_args("claude", &plan, &[]);
    assert_eq!(
        args[..2],
        ["--append-system-prompt", "Use \u{2011}c sparingly."]
    );
    let missing = plan_with_file("~/nope.md", &wrap_env);
    assert_eq!(
        missing.instructions.as_deref(),
        Some(instructions::default_paragraph(false, false).as_str())
    );
    assert_eq!(missing.warnings.len(), 1);
    assert!(
        missing.warnings[0].contains("~/nope.md: missing"),
        "{:?}",
        missing.warnings
    );
    drop(home);
}

#[test]
fn the_temp_home_points_every_derived_path_into_itself() {
    let home = TempHome::new("guard");
    for path in [
        crate::config::config_dir(),
        crate::config::state_dir(),
        crate::config::config_path(),
        crate::coordinator::coordinator_dir(),
        crate::browser::setup::shell_file_path(),
        instructions::default_file_path(),
        home.env.browser_home.clone(),
        home.env.shell_file.clone(),
        home.env.binary.clone(),
    ] {
        assert!(
            home.contains(&path),
            "{} escapes {}",
            path.display(),
            home.root.display()
        );
    }
    for path in [
        home.env.zshrc.clone(),
        home.env.claude_json.clone(),
        home.env.codex_config.clone(),
        home.env.home.clone(),
        codex_config_path(),
    ] {
        let path = path.unwrap();
        assert!(home.contains(&path), "{}", path.display());
    }
    assert!(home.env.claude_bin.is_none() && home.env.codex_bin.is_none());
    let root = home.root.clone();
    drop(home);
    assert!(!root.exists(), "removed on drop");
}

#[test]
fn the_snapshot_reports_the_hook_and_offers_the_fix_by_state() {
    let home = TempHome::new("snapshot");
    // wrap off, no hook: missing, not offered
    let off = settings_snapshot(&config("[browser]\nwrap_agents = false\n"), &home.env);
    assert!(!off.wrap);
    assert_eq!(off.wrap_source, WrapSource::LegacyBrowser);
    let hook = &off.checks[0];
    assert_eq!((hook.id, hook.state), ("shell_hook", "missing"));
    assert!(!hook.fixable && hook.edits_files);
    assert_eq!(off.hook_preview, None);
    assert_eq!(off.instructions_detail, "built-in");
    assert!(off.notices && off.steer_browser);
    assert_eq!(
        off.checks
            .iter()
            .map(|c| (c.id, c.state))
            .collect::<Vec<_>>(),
        [
            ("shell_hook", "missing"),
            ("claude", "absent"),
            ("codex", "absent")
        ]
    );
    // wrap on: offered, with the exact line
    let on = settings_snapshot(&config("[agents]\nwrap = true\n"), &home.env);
    assert!(on.checks[0].fixable);
    let preview = on.hook_preview.unwrap();
    assert!(preview.starts_with("~/.zshrc: "), "{preview}");
    assert!(preview.ends_with(&setup::zshrc_hook_line(&home.env.shell_file)));
    // an old (v1) hook is offered even with the wrap off
    let old = home.root.join("old/herdr-plus.zsh");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, "_herdr_plus_wrap() { :; }\n").unwrap();
    let zshrc = home.env.zshrc.clone().unwrap();
    let line = format!("{}\n", setup::zshrc_hook_line(&old));
    std::fs::write(&zshrc, &line).unwrap();
    let outdated = settings_snapshot(&config(""), &home.env);
    assert_eq!(outdated.checks[0].state, "outdated");
    assert!(outdated.checks[0].fixable);
    // the snapshot never wrote anything
    assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), line);
    assert!(!home.env.shell_file.exists());
    // the confirmed fix: our line, our v2 file, backup once; then ok
    let done = fix_shell_hook(&home.env).unwrap();
    assert!(done.contains("added"), "{done}");
    assert_eq!(
        setup::hook_lines(&std::fs::read_to_string(&zshrc).unwrap()),
        std::slice::from_ref(&home.env.shell_file)
    );
    assert!(home.contains(&home.env.shell_file));
    let fixed = settings_snapshot(&config(""), &home.env);
    assert_eq!(fixed.checks[0].state, "ok");
    assert!(!fixed.checks[0].fixable);
    // PATH facts, claude-z preferred
    let mut with_bins = home.env.clone();
    with_bins.claude_bin = Some(PathBuf::from("/bin/claude"));
    with_bins.claude_z_bin = Some(PathBuf::from("/bin/claude-z"));
    with_bins.codex_bin = Some(PathBuf::from("/bin/codex"));
    let bins = settings_snapshot(&config(""), &with_bins);
    assert_eq!((bins.checks[1].state, bins.checks[2].state), ("ok", "ok"));
    assert!(bins.checks[1].detail.contains("claude-z"));
    assert!(bins.checks[1].detail.contains("server's PATH"));
    drop(home);
}

#[test]
fn seeding_the_instructions_file_never_overwrites() {
    let home = TempHome::new("seed");
    let path = instructions::default_file_path();
    assert!(home.contains(&path));
    seed_instructions_file(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{}\n", instructions::DEFAULT_NOTIFY_PARAGRAPH)
    );
    std::fs::write(&path, "mine\n").unwrap();
    seed_instructions_file(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    let snapshot = settings_snapshot(
        &config(&format!(
            "[agents]\ninstructions_file = {:?}\n",
            path.display().to_string()
        )),
        &home.env,
    );
    assert!(
        snapshot.instructions_detail.ends_with("agents.md (5 B)"),
        "{}",
        snapshot.instructions_detail
    );
    drop(home);
}

#[test]
fn an_unmanaged_claude_launch_with_tools_uses_the_coordinator_mcp_config() {
    let home = TempHome::new("wrap-mcp");
    let config = config("[agents]\nwrap = true\ntools = true\n");
    let wrap_env = WrapEnv::from_process(&config);
    assert!(home.contains(&wrap_env.coordinator_dir));
    let plan = plan(&config, &wrap_env);
    assert!(uses_claude_mcp_config(&plan, &s(&["uuid"])));
    let path = launch::write_claude_mcp_config(&plan.ctx).unwrap();
    assert!(home.contains(&path));
    let args = wrap_args("claude", &plan, &s(&["uuid"]));
    assert!(args.contains(&launch::mcp_config_flag(&path)), "{args:?}");
    drop(home);
}

// ---------------------------------------------------------------------------
// The team-only launch wrap

const TEAM_TEXT: &str =
    "You are a new member (this pane, w3:p1, no role yet) in a herdr+ team (group demo).";

fn team_env() -> WrapEnv {
    WrapEnv {
        team: Some(team::TeamLaunch {
            text: TEAM_TEXT.into(),
        }),
        ..env()
    }
}

fn team_plan(text: &str) -> WrapPlan {
    plan(&config(text), &team_env())
}

fn settings_arg() -> String {
    format!("--settings={DIR}/team/claude-settings.json")
}

fn team_allow() -> String {
    claude_allow()
}

#[test]
fn a_team_launch_with_the_master_off_gets_only_the_team_bits() {
    let plan = team_plan("");
    assert!(!plan.master && plan.tools && plan.team.is_some() && plan.team_hook);
    assert!(plan.instructions.is_none() && !plan.steer && !plan.no_native);
    let user = s(&["uuid-1"]);
    assert!(uses_claude_mcp_config(&plan, &user), "the file gate is on");
    assert!(uses_team_settings(&plan, &user));
    let args = wrap_args("claude", &plan, &user);
    assert_eq!(
        args,
        [
            "uuid-1".to_string(),
            claude_flag(),
            team_allow(),
            settings_arg(),
            "--append-system-prompt".into(),
            TEAM_TEXT.into(),
        ]
    );
    assert!(!args.iter().any(|a| a == "--no-chrome"));
    assert_eq!(
        args.iter().filter(|a| a.starts_with("--settings")).count(),
        1
    );
    // every tool is pre-approved but close and reopen (agents v2)
    for tool in wrap_tools() {
        assert!(team_allow().contains(&format!("mcp__herdr_agents__{tool}")));
    }
    assert!(team_allow().contains("agents_open_tab"));
    for tool in ["agents_suspend", "agents_activate", "agents_restart"] {
        assert!(
            team_allow().contains(&format!("mcp__herdr_agents__{tool}")),
            "{tool}"
        );
    }
    assert!(!team_allow().contains("agents_close_tab"));
    assert!(!team_allow().contains("agents_reopen_tab"));
    // no team: the master-off wrap is unchanged
    let plain = plan_for_off();
    assert!(!uses_claude_mcp_config(&plain, &user));
    assert_eq!(wrap_args("claude", &plain, &user), user);
}

fn plan_for_off() -> WrapPlan {
    plan(&config(""), &env())
}

#[test]
fn the_team_switch_off_or_an_opt_out_drops_the_team_bits() {
    let off = team_plan("[agents]\nteam_roster = false\n");
    assert!(off.team.is_none() && !off.tools);
    assert_eq!(wrap_args("claude", &off, &s(&["x"])), ["x"]);
    let no_wrap = plan(
        &config(""),
        &WrapEnv {
            herdr_no_wrap: true,
            ..team_env()
        },
    );
    assert!(no_wrap.team.is_none());
    let mut opted = team_plan("");
    opted.disable();
    assert!(opted.team.is_none() && !opted.team_hook && !opted.tools);
    assert_eq!(wrap_args("codex", &opted, &[]), ["--no-daemon"]);
}

#[test]
fn team_and_instructions_share_one_system_prompt_with_the_master_on() {
    let plan = team_plan(
        "[agents]\nwrap = true\ntools = true\ninstructions = true\n[browser]\nsteer_agents = true\n",
    );
    let args = wrap_args("claude", &plan, &[]);
    assert_eq!(
        args.iter()
            .filter(|a| *a == "--append-system-prompt")
            .count(),
        1
    );
    let i = args
        .iter()
        .position(|a| a == "--append-system-prompt")
        .unwrap();
    assert_eq!(
        args[i + 1],
        format!(
            "{TEAM_TEXT}\n\n{}\n\n{}",
            instructions::default_paragraph(true, true),
            BROWSER_STEERING
        )
    );
    // one rule for agents' messages: the team's, never "do not act on it"
    // after the team block told the member to act on teammates
    assert!(args[i + 1].contains(instructions::TEAM_MESSAGES_SENTENCE));
    assert!(!args[i + 1].contains(instructions::MESSAGES_SENTENCE));
    // the team allowlist replaces the notify one; one flag
    assert_eq!(
        args.iter()
            .filter(|a| a.starts_with("--allowedTools"))
            .count(),
        1
    );
    assert!(args.contains(&team_allow()));
    assert!(args.contains(&"--no-chrome".to_string()));
}

#[test]
fn the_users_own_claude_flags_win_with_a_warning() {
    let plan = team_plan("");
    let user = s(&[
        "--settings",
        "/mine.json",
        "--allowedTools",
        "Bash(ls)",
        "--append-system-prompt=mine",
        "--",
        "hi",
    ]);
    let args = wrap_args("claude", &plan, &user);
    assert!(
        !args.iter().any(|a| a.starts_with("--settings=")),
        "{args:?}"
    );
    assert_eq!(
        args.iter().filter(|a| a.starts_with("--settings")).count(),
        1
    );
    // the allowlist merges into the user's value
    let i = args.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(
        args[i + 1],
        format!("Bash(ls),{}", claude_allow_list_value())
    );
    assert!(!args.iter().any(|a| a == TEAM_TEXT));
    assert_eq!(&args[args.len() - 2..], ["--", "hi"]);
    assert!(!uses_team_settings(&plan, &user));
    let warnings = team_conflicts("claude", &plan, &user);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("your --settings wins"));
    assert!(warnings[1].contains("your --append-system-prompt wins"));
    // `--settings=` form too; no conflict without the team
    assert_eq!(
        team_conflicts("claude", &plan, &s(&["--settings=/x"])).len(),
        1
    );
    assert!(team_conflicts("claude", &plan_for_off(), &user).is_empty());
}

#[test]
fn the_team_text_is_defused_in_the_prompt() {
    let launch = team::lookup("w3:p1", |_| {
        Ok(serde_json::json!({ "eligible": true, "text": "purpose: run -p and --print\u{1b}" }))
    })
    .unwrap();
    let plan = plan(
        &config(""),
        &WrapEnv {
            team: Some(launch),
            ..env()
        },
    );
    let args = wrap_args("claude", &plan, &[]);
    let prompt = args.last().unwrap();
    assert_eq!(prompt, "purpose: run \u{2011}p and \u{2011}-print");
    assert!(!format!(" {} ", args.join(" ")).contains(" -p "));
}

#[test]
fn codex_in_a_team_gets_the_server_the_team_approvals_and_the_roster() {
    let plan = plan(
        &config(""),
        &WrapEnv {
            codex_own_instructions: Some("Be terse.".into()),
            ..team_env()
        },
    );
    let args = wrap_args("codex", &plan, &s(&["resume"]));
    assert_eq!(args[0], "--no-daemon");
    assert_eq!(args.last().unwrap(), "resume");
    assert!(!args.iter().any(|a| a == "--disable"));
    for tool in wrap_tools() {
        let approval = format!("mcp_servers.herdr_agents.tools.{tool}.approval_mode=\"approve\"");
        assert!(args.contains(&approval), "{tool}");
    }
    for tool in crate::coordinator::mcp::UNAPPROVED_TOOLS {
        assert!(
            !args.iter().any(|a| a.contains(&format!("tools.{tool}."))),
            "{tool}"
        );
    }
    assert_eq!(
        developer_instructions(&args).as_deref(),
        Some(format!("Be terse.\n\n{TEAM_TEXT}").as_str())
    );
    // the user's own developer_instructions win, with a warning
    let user = s(&["-c", "developer_instructions=\"mine\""]);
    let args = wrap_args("codex", &plan, &user);
    assert_eq!(developer_instructions(&args).as_deref(), Some("mine"));
    assert_eq!(team_conflicts("codex", &plan, &user).len(), 1);
}

#[test]
fn a_managed_team_launch_gets_nothing_added() {
    let plan = team_plan("");
    let dir = std::path::Path::new(DIR);
    let managed = s(&[
        "--session-id",
        "u1",
        &crate::coordinator::launch::mcp_config_flag(
            &crate::coordinator::launch::claude_mcp_config_path(dir),
        ),
        &format!("--allowedTools={}", claude_allow_agent()),
        &settings_arg(),
        "--append-system-prompt",
        TEAM_TEXT,
        "--",
        "kickoff",
    ]);
    assert_eq!(wrap_args("claude", &plan, &managed), managed);
    assert!(!uses_claude_mcp_config(&plan, &managed));
    assert!(team_conflicts("claude", &plan, &managed).is_empty());
    let codex = crate::coordinator::launch::codex_args(
        &crate::coordinator::launch::LaunchCtx {
            herdr_bin: PathBuf::from("/opt/herdr/herdr"),
            dir: PathBuf::from(DIR),
            port: crate::coordinator::DEFAULT_PORT,
        },
        Some("go"),
    );
    let args = wrap_args("codex", &plan, &codex);
    assert_eq!(&args[1..], &codex[..]);
    assert!(developer_instructions(&args).is_none());
}

#[test]
fn the_lookup_runs_only_for_team_eligible_launches() {
    let cfg = config("");
    let user = s(&["x"]);
    assert!(team::should_lookup(&cfg, &env(), false, "claude", &user));
    assert!(
        !team::should_lookup(&cfg, &env(), true, "claude", &user),
        "--no-herdr"
    );
    let no_wrap = WrapEnv {
        herdr_no_wrap: true,
        ..env()
    };
    assert!(
        !team::should_lookup(&cfg, &no_wrap, false, "claude", &user),
        "HERDR_NO_WRAP"
    );
    let no_pane = WrapEnv {
        pane_id: None,
        ..env()
    };
    assert!(!team::should_lookup(&cfg, &no_pane, false, "codex", &user));
    assert!(!team::should_lookup(
        &cfg,
        &env(),
        false,
        "claude",
        &s(&["mcp", "list"])
    ));
    assert!(!team::should_lookup(
        &cfg,
        &env(),
        false,
        "codex",
        &s(&["login"])
    ));
    assert!(!team::should_lookup(
        &config("[agents]\nteam_roster = false\n"),
        &env(),
        false,
        "claude",
        &user
    ));
    let managed = s(&[&format!("--allowedTools={}", claude_allow_agent())]);
    assert!(!team::should_lookup(
        &cfg,
        &env(),
        false,
        "claude",
        &managed
    ));
}

#[test]
fn an_agent_started_inside_the_panes_own_agent_is_not_the_member() {
    // The wrap marks the agent it execs; a `claude -p` from that agent's
    // Bash tool inherits HERDR_PANE_ID and the mark.
    assert!(is_nested(Some("w3:p1"), Some("w3:p1")));
    assert!(!is_nested(Some("w3:p1"), None));
    assert!(
        !is_nested(Some("w3:p2"), Some("w3:p1")),
        "another pane's mark"
    );
    assert!(!is_nested(None, Some("w3:p1")));
    assert!(!is_nested(Some(""), Some("")));
    let nested = WrapEnv {
        nested: true,
        team: Some(team::TeamLaunch {
            text: TEAM_TEXT.into(),
        }),
        ..env()
    };
    let user = s(&["-p", "summarize the diff"]);
    let cfg = config("[agents]\nwrap = true\ntools = true\ninstructions = true\n");
    assert!(!team::should_lookup(&cfg, &nested, false, "claude", &user));
    // no team identity and no herdr_agents server: any call would act, and
    // ack team updates, as the pane's agent
    let plan = super::plan(&cfg, &nested);
    assert!(
        plan.team.is_none() && !plan.team_hook && !plan.tools,
        "{plan:?}"
    );
    assert!(!uses_claude_mcp_config(&plan, &user));
    assert!(!uses_team_settings(&plan, &user));
    let args = wrap_args("claude", &plan, &user);
    assert!(!args
        .iter()
        .any(|a| a.contains("herdr_agents") || a.starts_with("--settings")));
    // the same launch as the pane's own agent keeps both
    let own = super::plan(&cfg, &team_env());
    assert!(own.team.is_some() && own.tools);
}

#[test]
fn a_failed_lookup_is_a_plain_wrap() {
    let launch = team::lookup("w3:p1", |_| Err("server_unavailable: no socket".into()));
    let plan = plan(
        &config("[agents]\nwrap = true\ntools = true\n[browser]\nsteer_agents = false\n"),
        &WrapEnv {
            team: launch,
            ..env()
        },
    );
    assert!(plan.team.is_none());
    assert_eq!(
        wrap_args("claude", &plan, &[]),
        [
            claude_flag(),
            claude_allow(),
            wrap_settings_flag(),
            "--no-chrome".to_string()
        ]
    );
}

#[test]
fn the_coordinators_real_team_argv_is_seen_as_managed_through_the_hook() {
    // The argv `agents_open_tab` really builds for a team member, through
    // the shell hook with every switch on (the dev config) and a team plan:
    // managed, so the wrap adds no second settings file, allowlist or
    // MCP config, and warns about nothing of ours.
    let home = TempHome::new("wrap-managed-team");
    let dir = crate::coordinator::coordinator_dir();
    let ctx = LaunchCtx {
        herdr_bin: PathBuf::from("/opt/herdr/herdr"),
        dir: dir.clone(),
        port: crate::coordinator::DEFAULT_PORT,
    };
    let team = team::TeamLaunch {
        text: TEAM_TEXT.into(),
    };
    let managed = launch::claude_args_with_team(
        &ctx,
        &ClaudeSession::New("u1".into()),
        false,
        Some("go"),
        Some(&team),
    )
    .unwrap();
    assert!(is_managed(&managed, &dir));
    for mut plan in [plan_for(true, true, true), team_plan("")] {
        plan.ctx = ctx.clone();
        if plan.team.is_none() {
            plan.team = Some(team.clone());
        }
        let args = wrap_args("claude", &plan, &managed);
        assert_eq!(
            count_herdr_agents(&args),
            count_herdr_agents(&managed),
            "{args:?}"
        );
        assert_eq!(
            args.iter().filter(|a| a.starts_with("--settings")).count(),
            1,
            "{args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("--append-system-prompt"))
                .count(),
            1,
            "one system prompt flag: {args:?}"
        );
        let prompt_at = args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert!(args[prompt_at + 1].contains(TEAM_TEXT), "{args:?}");
        assert!(!uses_claude_mcp_config(&plan, &managed));
        assert!(team_conflicts("claude", &plan, &managed).is_empty());
        assert_eq!(&args[args.len() - 2..], ["--", "go"]);
    }
    // Codex: the same for its overrides and developer_instructions.
    let managed = launch::codex_args_with_team(&ctx, Some("go"), Some(&team));
    assert!(is_managed(&managed, &dir));
    let mut plan = plan_for(true, true, true);
    plan.ctx = ctx.clone();
    plan.team = Some(team.clone());
    let args = wrap_args("codex", &plan, &managed);
    assert_eq!(count_herdr_agents(&args), count_herdr_agents(&managed));
    assert!(developer_instructions(&args)
        .unwrap_or_default()
        .contains(TEAM_TEXT));
    assert!(team_conflicts("codex", &plan, &managed).is_empty());
    drop(home);
}

#[test]
fn herdr_relaunches_type_the_wrap_verb_with_the_native_resume_arguments() {
    let herdr = Path::new("/opt/herdr/herdr");
    let dir = Path::new(DIR);
    assert_eq!(
        relaunch_argv(herdr, "claude", &s(&["claude", "--resume", "c-1"]), dir).unwrap(),
        s(&[
            "/opt/herdr/herdr",
            "agent",
            "wrap",
            "claude",
            "--",
            "--resume",
            "c-1"
        ])
    );
    assert_eq!(
        relaunch_argv(herdr, "codex", &s(&["codex", "resume", "x-1"]), dir).unwrap(),
        s(&[
            "/opt/herdr/herdr",
            "agent",
            "wrap",
            "codex",
            "--",
            "resume",
            "x-1"
        ])
    );
    // a bare start: the verb alone
    assert_eq!(
        relaunch_argv(herdr, "claude", &s(&["claude"]), dir).unwrap(),
        s(&["/opt/herdr/herdr", "agent", "wrap", "claude", "--"])
    );
    // other agents and empty argv stay native
    assert_eq!(
        relaunch_argv(herdr, "pi", &s(&["pi", "--session", "p"]), dir),
        None
    );
    assert_eq!(relaunch_argv(herdr, "claude", &[], dir), None);
}

#[test]
fn managed_launch_argv_never_goes_through_the_relaunch_wrap() {
    let home = TempHome::new("wrap-relaunch-managed");
    let dir = crate::coordinator::coordinator_dir();
    let ctx = LaunchCtx {
        herdr_bin: PathBuf::from("/opt/herdr/herdr"),
        dir: dir.clone(),
        port: crate::coordinator::DEFAULT_PORT,
    };
    let herdr = Path::new("/opt/herdr/herdr");
    // the coordinator's own (re)launch argv
    let mut claude = vec!["claude".to_string()];
    claude.extend(
        launch::claude_args(&ctx, &ClaudeSession::Resume("u1".into()), true, Some("go")).unwrap(),
    );
    assert_eq!(relaunch_argv(herdr, "claude", &claude, &dir), None);
    let mut codex = vec!["codex".to_string()];
    codex.extend(launch::codex_args_with_team(&ctx, Some("go"), None));
    assert_eq!(relaunch_argv(herdr, "codex", &codex, &dir), None);
    drop(home);
}

#[test]
fn a_relaunch_through_the_wrap_still_honours_herdr_no_wrap() {
    // What the verb does with a relaunch's arguments when the pane's shell
    // has HERDR_NO_WRAP=1: the resume runs as typed.
    let user = s(&["--resume", "c-1"]);
    let off = plan(
        &config("[agents]\nwrap = true\ntools = true\ninstructions = true\n"),
        &WrapEnv {
            herdr_no_wrap: true,
            ..env()
        },
    );
    assert_eq!(wrap_args("claude", &off, &user), user);
    assert_eq!(
        wrap_args("codex", &off, &s(&["resume", "x-1"])),
        s(&["--no-daemon", "resume", "x-1"])
    );
    // and wrapped otherwise, the session id kept first for claude-z
    let on = plan_for(true, true, false);
    let args = wrap_args("claude", &on, &user);
    assert_eq!(&args[..2], user.as_slice());
    assert!(args.contains(&claude_flag()), "{args:?}");
}

#[test]
fn the_notes_tools_are_pre_approved_for_every_wrapped_and_team_launch() {
    let notes = crate::coordinator::mcp::NOTES_TOOLS;
    for tool in notes {
        assert!(wrap_tools().contains(&tool), "{tool}");
        assert!(
            crate::coordinator::mcp::preapproved_tools().any(|t| t == tool),
            "{tool}"
        );
    }
    for plan in [plan_for(true, false, false), team_plan("")] {
        let claude = wrap_args("claude", &plan, &s(&["uuid"]));
        let allow = claude
            .iter()
            .find_map(|a| a.strip_prefix("--allowedTools="))
            .unwrap();
        let codex = wrap_args("codex", &plan, &[]);
        for tool in notes {
            assert!(
                allow
                    .split(',')
                    .any(|e| e == format!("mcp__herdr_agents__{tool}")),
                "{tool} in {allow}"
            );
            let approval =
                format!("mcp_servers.herdr_agents.tools.{tool}.approval_mode=\"approve\"");
            assert!(codex.contains(&approval), "{approval} in {codex:?}");
        }
    }
    // managed launches: Codex approves the whole server, the coordinator's
    // Claude allowlist names the server
    let ctx = launch::LaunchCtx {
        herdr_bin: PathBuf::from("/opt/herdr/herdr"),
        dir: PathBuf::from(DIR),
        port: crate::coordinator::DEFAULT_PORT,
    };
    assert!(launch::codex_args(&ctx, None)
        .iter()
        .any(|a| a == "mcp_servers.herdr_agents.default_tools_approval_mode=\"approve\""));
    assert!(launch::claude_allow_coordinator()
        .split(',')
        .any(|e| e == "mcp__herdr_agents"));
}

#[test]
fn a_wrapped_claude_launch_gets_the_notes_recall_settings_unless_something_wins() {
    let on = plan_for(true, false, false);
    assert!(on.notes_hook);
    let user = s(&["uuid"]);
    let args = wrap_args("claude", &on, &user);
    assert_eq!(
        args.iter().filter(|a| a.starts_with("--settings")).count(),
        1
    );
    assert!(args.contains(&wrap_settings_flag()), "{args:?}");
    assert!(notes_conflicts("claude", &on, &user).is_empty());
    // without the tools no recall: it points at tools the agent lacks
    let toolless = plan_for(false, false, false);
    assert!(!toolless.notes_hook);
    assert!(!wrap_args("claude", &toolless, &user).contains(&wrap_settings_flag()));
    // [notes] enabled = false: no hook
    let off = plan(
        &config("[agents]\nwrap = true\ntools = true\n[notes]\nenabled = false\n"),
        &env(),
    );
    assert!(!off.notes_hook);
    assert!(!wrap_args("claude", &off, &user)
        .iter()
        .any(|a| a.starts_with("--settings")));
    // the user's --settings wins, with a warning
    let theirs = s(&["--settings", "/mine.json"]);
    let args = wrap_args("claude", &on, &theirs);
    assert_eq!(
        args.iter().filter(|a| a.starts_with("--settings")).count(),
        1
    );
    assert_eq!(notes_conflicts("claude", &on, &theirs).len(), 1);
    // a `-p` run, a pass-through and a managed launch get none
    for user in [s(&["-p", "hi"]), s(&["mcp", "list"])] {
        assert!(
            !wrap_args("claude", &on, &user)
                .iter()
                .any(|a| a.starts_with("--settings")),
            "{user:?}"
        );
    }
    let managed = s(&[&claude_flag(), "--", "go"]);
    assert_eq!(claude_settings_file(&on, &managed), None);
    // the wrap off, nested or opted out: none
    assert_eq!(claude_settings_file(&plan_for_off(), &user), None);
    let nested = plan(
        &config("[agents]\nwrap = true\ntools = true\n"),
        &WrapEnv {
            nested: true,
            ..env()
        },
    );
    assert!(!nested.notes_hook);
    let mut opted = plan_for(true, false, false);
    opted.disable();
    assert!(!opted.notes_hook && opted.recall.is_none());
    // a team launch: the team's file, which carries the recall (one flag)
    let team = team_plan("");
    assert!(team.notes_hook);
    let args = wrap_args("claude", &team, &user);
    assert!(args.contains(&settings_arg()));
    assert!(!args.contains(&wrap_settings_flag()));
    // a team launch whose hook file failed falls back to the recall file
    let mut failed = team_plan("");
    failed.team_hook = false;
    assert_eq!(
        claude_settings_file(&failed, &user),
        Some(PathBuf::from(DIR).join("wrap/claude-settings.json"))
    );
}

#[test]
fn a_codex_launch_carries_the_recall_in_its_developer_instructions() {
    let mut plan = plan_for(true, true, false);
    assert!(codex_wants_recall(&plan, &[]));
    plan.recall = Some("[herdr+ notes] remember the flag".into());
    let text = developer_instructions(&wrap_args("codex", &plan, &[])).unwrap();
    assert!(text.starts_with(instructions::INTRO), "{text}");
    assert!(text.ends_with("[herdr+ notes] remember the flag"), "{text}");
    // Claude gets it from the hook, never in its argv
    assert!(!wrap_args("claude", &plan, &[])
        .iter()
        .any(|a| a.contains("remember the flag")));
    // the user's own developer_instructions win; managed launches and a
    // launch without the tools do not ask for it
    let theirs = s(&["-c", "developer_instructions=\"mine\""]);
    assert!(!codex_wants_recall(&plan, &theirs));
    assert!(!wrap_args("codex", &plan, &theirs)
        .iter()
        .any(|a| a.contains("remember the flag")));
    let managed = s(&["-c", "mcp_servers.herdr_agents.command=\"x\""]);
    assert!(!codex_wants_recall(&plan, &managed));
    assert!(!wrap_args("codex", &plan, &managed)
        .iter()
        .any(|a| a.contains("remember the flag")));
    assert!(!codex_wants_recall(&plan_for(false, true, false), &[]));
    assert!(!codex_wants_recall(&plan, &s(&["exec", "summarize"])));
    assert!(!codex_wants_recall(
        &plan,
        &s(&["--yolo", "exec", "summarize"])
    ));
    assert!(codex_wants_recall(&plan, &s(&["resume", "--last"])));
    let notes_off = super::plan(
        &config("[agents]\nwrap = true\ntools = true\n[notes]\nenabled = false\n"),
        &env(),
    );
    assert!(!codex_wants_recall(&notes_off, &[]));
    assert!(codex_wants_recall(&team_plan(""), &[]));
}
