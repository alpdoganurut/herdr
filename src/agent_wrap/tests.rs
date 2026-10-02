use super::test_support::TempHome;
use super::*;
use crate::cli::BROWSER_STEERING;
use crate::coordinator::launch::{ClaudeSession, CLAUDE_ALLOW_AGENT};

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

const CLAUDE_ALLOW: &str =
    "--allowedTools=mcp__herdr_agents__agents_whoami,mcp__herdr_agents__agents_notify";

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
                assert_eq!(args.iter().any(|a| a == CLAUDE_ALLOW), tools, "{label}");
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
        instructions::DEFAULT_NOTIFY_PARAGRAPH
    );
    assert_eq!(
        wrap_args("claude", &plan, &s(&["-p", "--", "prompt text"])),
        [
            "-p".to_string(),
            claude_flag(),
            CLAUDE_ALLOW.to_string(),
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
                assert_eq!(approvals, if tools { 2 } else { 0 }, "{label}");
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
            instructions::default_paragraph(false)
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
    let allow = "mcp__herdr_agents__agents_whoami,mcp__herdr_agents__agents_notify";
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
    assert_eq!(CLAUDE_ALLOW_AGENT, "mcp__herdr_agents");
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
        Some(instructions::default_paragraph(false).as_str())
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
