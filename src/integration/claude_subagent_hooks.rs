//! Claude Code `SubagentStart` / `SubagentStop` / `Stop` hooks for the fork's
//! subagent count (`pane.report_subagent`), and `UserPromptSubmit` / `Stop`
//! for its main turns (`pane.report_turn`, message delivery).
//!
//! Applied after the upstream settings edit (`claude_settings`), so that
//! module stays as upstream wrote it: `install` adds one entry per event that
//! runs the Claude hook asset with that event's action (`subagent` for the
//! subagent events, `stop` for the main agent's Stop, which reports the
//! turn's end and whose `background_tasks` snapshot the running set, `turn`
//! for UserPromptSubmit, a turn's start, `browser` for PreToolUse, the
//! browser steering, with [`BROWSER_STEER_MATCHER`]), `uninstall` removes
//! the asset's commands for those actions again. User hooks and formatting outside
//! the touched arrays are kept; the result is re-parsed and must equal the
//! intended settings value, as the upstream edit does.

use std::io;
use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::{json, ParseOptions};
use serde_json::{json as serde_json_value, Map, Value};

use super::command::hook_command;
use super::config_edit::{hook_command_variants, is_matching_command_hook};

/// The tools the browser steering looks at (PreToolUse): WebFetch, Bash
/// (a command that opens a page) and browser automation MCP servers. The
/// asset decides; herdr-browser's own tools are never denied.
pub(crate) const BROWSER_STEER_MATCHER: &str =
    "^(WebFetch|Bash|mcp__.*(playwright|puppeteer|chrome|browsermcp|browserbase|browser-use|browser_use).*)$";

/// The hook events the fork adds, each with the asset action it runs and
/// its matcher.
pub(crate) const SUBAGENT_HOOKS: [(&str, &str, Option<&str>); 6] = [
    ("SubagentStart", "subagent", None),
    ("SubagentStop", "subagent", None),
    ("Stop", "stop", None),
    // A main turn's start, for message delivery (`pane.report_turn`; Stop
    // reports its end).
    ("UserPromptSubmit", "turn", None),
    // A compaction starts, for handoff, then compact (`pane.report_compact`;
    // the `session` action reports SessionStart(compact)).
    ("PreCompact", "precompact", None),
    // The herdr browser steering: a WebFetch, a command opening a page in
    // another browser or another browser MCP is denied once with what to
    // use instead (Claude integration v17).
    ("PreToolUse", "browser", Some(BROWSER_STEER_MATCHER)),
];
const SUBAGENT_HOOK_TIMEOUT: u64 = 10;

fn canonical_value(hook_path: &Path, action: &str, matcher: Option<&str>) -> Value {
    let mut value = serde_json_value!({
        "hooks": [{
            "type": "command",
            "command": hook_command(hook_path, Some(action)),
            "timeout": SUBAGENT_HOOK_TIMEOUT,
        }],
    });
    if let (Some(matcher), Some(object)) = (matcher, value.as_object_mut()) {
        object.insert("matcher".into(), Value::String(matcher.into()));
    }
    value
}

fn canonical_input(hook_path: &Path, action: &str, matcher: Option<&str>) -> CstInputValue {
    let command = hook_command(hook_path, Some(action));
    match matcher {
        Some(matcher) => json!({
            matcher: matcher,
            hooks: [{
                "type": "command",
                command: command,
                timeout: SUBAGENT_HOOK_TIMEOUT,
            }],
        }),
        None => json!({
            hooks: [{
                "type": "command",
                command: command,
                timeout: SUBAGENT_HOOK_TIMEOUT,
            }],
        }),
    }
}

/// Whether `entry` (one matcher group) runs the asset's subagent action.
fn entry_runs_subagent_hook(entry: &Value, commands: &[String]) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                commands
                    .iter()
                    .any(|command| is_matching_command_hook(hook, command))
            })
        })
}

fn event_has_subagent_hook(hooks: &Map<String, Value>, event: &str, commands: &[String]) -> bool {
    hooks
        .get(event)
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry_runs_subagent_hook(entry, commands))
        })
}

fn parse_options() -> ParseOptions {
    ParseOptions {
        allow_comments: false,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

fn parse_value(content: &str, settings_path: &Path) -> io::Result<Value> {
    serde_json::from_str(content).map_err(|err| {
        io::Error::other(format!(
            "failed to parse {}: {err}",
            settings_path.display()
        ))
    })
}

fn parse_root(content: &str, settings_path: &Path) -> io::Result<CstRootNode> {
    CstRootNode::parse(content, &parse_options()).map_err(|err| {
        io::Error::other(format!(
            "failed to parse {}: {err}",
            settings_path.display()
        ))
    })
}

fn not_an_object(settings_path: &Path, what: &str) -> io::Error {
    io::Error::other(format!(
        "claude {what} at {} must be a JSON object",
        settings_path.display()
    ))
}

fn verify(updated: String, settings_path: &Path, desired: &Value) -> io::Result<String> {
    if &parse_value(&updated, settings_path)? != desired {
        return Err(io::Error::other(format!(
            "failed to safely update claude settings at {}",
            settings_path.display()
        )));
    }
    Ok(updated)
}

/// Add an entry per `SUBAGENT_HOOKS` event running the asset with its
/// action, unless the event already runs it.
pub(crate) fn install(content: &str, settings_path: &Path, hook_path: &Path) -> io::Result<String> {
    let original = parse_value(content, settings_path)?;
    let mut desired = original.clone();
    let root = desired
        .as_object_mut()
        .ok_or_else(|| not_an_object(settings_path, "settings"))?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| not_an_object(settings_path, "settings hooks"))?;
    let missing: Vec<(&str, &str, Option<&str>)> = SUBAGENT_HOOKS
        .into_iter()
        .filter(|(event, action, _)| {
            !event_has_subagent_hook(
                hooks,
                event,
                &hook_command_variants(hook_path, Some(action)),
            )
        })
        .collect();
    for (event, action, matcher) in &missing {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                io::Error::other(format!("hook entries for {event} must be an array"))
            })?;
        entries.push(canonical_value(hook_path, action, *matcher));
    }
    if desired == original {
        return Ok(content.to_string());
    }

    let cst = parse_root(content, settings_path)?;
    let root_object = cst
        .value()
        .and_then(|value| value.as_object())
        .ok_or_else(|| not_an_object(settings_path, "settings"))?;
    let hooks_object = match root_object.get("hooks") {
        Some(property) => property
            .object_value()
            .ok_or_else(|| not_an_object(settings_path, "settings hooks"))?,
        None => root_object
            .append("hooks", CstInputValue::Object(Vec::new()))
            .object_value()
            .ok_or_else(|| io::Error::other("failed to create claude settings hooks object"))?,
    };
    for (event, action, matcher) in missing {
        match hooks_object.get(event) {
            Some(property) => {
                property
                    .array_value()
                    .ok_or_else(|| {
                        io::Error::other(format!("hook entries for {event} must be an array"))
                    })?
                    .append(canonical_input(hook_path, action, matcher));
            }
            None => {
                hooks_object.append(
                    event,
                    CstInputValue::Array(vec![canonical_input(hook_path, action, matcher)]),
                );
            }
        }
    }
    verify(cst.to_string(), settings_path, &desired)
}

/// Remove the asset's commands for each `SUBAGENT_HOOKS` event, dropping
/// groups and events they leave empty. Other hooks on those events stay.
pub(crate) fn uninstall(
    content: &str,
    settings_path: &Path,
    hook_path: &Path,
) -> io::Result<String> {
    let original = parse_value(content, settings_path)?;
    let mut desired = original.clone();
    let Some(hooks) = desired
        .as_object_mut()
        .and_then(|root| root.get_mut("hooks"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(content.to_string());
    };
    for (event, action, _) in SUBAGENT_HOOKS {
        let commands = hook_command_variants(hook_path, Some(action));
        let Some(entries) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let before = entries.clone();
        entries.retain_mut(|entry| {
            let Some(command_hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let count = command_hooks.len();
            command_hooks.retain(|hook| {
                !commands
                    .iter()
                    .any(|command| is_matching_command_hook(hook, command))
            });
            command_hooks.len() == count || !command_hooks.is_empty()
        });
        if entries.is_empty() && !before.is_empty() {
            hooks.remove(event);
        }
    }
    if desired == original {
        return Ok(content.to_string());
    }

    let cst = parse_root(content, settings_path)?;
    let Some(hooks_object) = cst
        .value()
        .and_then(|value| value.as_object())
        .and_then(|root| root.get("hooks"))
        .and_then(|property| property.object_value())
    else {
        return Ok(content.to_string());
    };
    for (event, action, _) in SUBAGENT_HOOKS {
        let commands = hook_command_variants(hook_path, Some(action));
        let Some(property) = hooks_object.get(event) else {
            continue;
        };
        let Some(entries) = property.array_value() else {
            continue;
        };
        let had_entries = !entries.elements().is_empty();
        for entry in entries.elements() {
            let Some(command_hooks) = entry
                .as_object()
                .and_then(|object| object.get("hooks"))
                .and_then(|property| property.array_value())
            else {
                continue;
            };
            let mut removed = false;
            for command_hook in command_hooks.elements() {
                let matches = command_hook.to_serde_value().is_some_and(|value| {
                    commands
                        .iter()
                        .any(|command| is_matching_command_hook(&value, command))
                });
                if matches {
                    command_hook.remove();
                    removed = true;
                }
            }
            if removed && command_hooks.elements().is_empty() {
                entry.remove();
            }
        }
        if had_entries && entries.elements().is_empty() {
            property.remove();
        }
    }
    verify(cst.to_string(), settings_path, &desired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (&'static Path, &'static Path) {
        (
            Path::new("/home/test/.claude/settings.json"),
            Path::new("/home/test/.claude/hooks/herdr-agent-state.sh"),
        )
    }

    fn subagent_commands(settings: &Value, event: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|entry| entry["hooks"].as_array().into_iter().flatten())
            .filter_map(|hook| hook["command"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn install_adds_each_event_once_and_keeps_user_hooks() {
        let (settings_path, hook_path) = paths();
        let subagent = hook_command(hook_path, Some("subagent"));
        let stop = hook_command(hook_path, Some("stop"));
        let turn = hook_command(hook_path, Some("turn"));
        let precompact = hook_command(hook_path, Some("precompact"));
        let input = concat!(
            "{\n",
            "  \"permissions\": {\"allow\": [\"Read\"]},\n",
            "  \"hooks\": {\n",
            "    \"SubagentStop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo keep\"}]}],\n",
            "    \"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"~/.claude/hooks/zellij-activity.sh stop\"}]}]\n",
            "  }\n",
            "}\n",
        );

        let installed = install(input, settings_path, hook_path).unwrap();
        let settings: Value = serde_json::from_str(&installed).unwrap();
        assert_eq!(
            subagent_commands(&settings, "SubagentStart"),
            std::slice::from_ref(&subagent)
        );
        assert_eq!(
            subagent_commands(&settings, "SubagentStop"),
            ["echo keep".to_string(), subagent.clone()]
        );
        assert_eq!(
            subagent_commands(&settings, "Stop"),
            [
                "~/.claude/hooks/zellij-activity.sh stop".to_string(),
                stop.clone()
            ],
            "beside the user's own Stop hook"
        );
        assert_eq!(
            subagent_commands(&settings, "UserPromptSubmit"),
            std::slice::from_ref(&turn)
        );
        assert!(turn.ends_with(" turn"), "{turn}");
        // Fork (agent handoff): PreCompact runs the asset's precompact action.
        assert_eq!(
            subagent_commands(&settings, "PreCompact"),
            std::slice::from_ref(&precompact)
        );
        assert!(precompact.ends_with(" precompact"), "{precompact}");
        // Fork (browser steering): PreToolUse runs the browser action for the
        // matched tools only.
        let browser = hook_command(hook_path, Some("browser"));
        assert_eq!(
            subagent_commands(&settings, "PreToolUse"),
            std::slice::from_ref(&browser)
        );
        assert!(browser.ends_with(" browser"), "{browser}");
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["matcher"],
            BROWSER_STEER_MATCHER
        );
        assert!(subagent.ends_with(" subagent"), "{subagent}");
        assert!(stop.ends_with(" stop"), "{stop}");
        assert!(installed.contains("\"permissions\": {\"allow\": [\"Read\"]}"));
        assert_eq!(settings["hooks"]["Stop"][1]["hooks"][0]["timeout"], 10);

        assert_eq!(
            install(&installed, settings_path, hook_path).unwrap(),
            installed,
            "idempotent"
        );

        let removed = uninstall(&installed, settings_path, hook_path).unwrap();
        let settings: Value = serde_json::from_str(&removed).unwrap();
        assert!(settings["hooks"].get("SubagentStart").is_none());
        assert!(settings["hooks"].get("UserPromptSubmit").is_none());
        assert!(settings["hooks"].get("PreCompact").is_none());
        assert!(settings["hooks"].get("PreToolUse").is_none());
        assert_eq!(subagent_commands(&settings, "SubagentStop"), ["echo keep"]);
        assert_eq!(
            subagent_commands(&settings, "Stop"),
            ["~/.claude/hooks/zellij-activity.sh stop"],
            "only herdr's Stop hook goes"
        );
        assert_eq!(
            uninstall(&removed, settings_path, hook_path).unwrap(),
            removed
        );
    }

    #[test]
    fn install_creates_the_hooks_object_and_rejects_bad_shapes() {
        let (settings_path, hook_path) = paths();
        let installed = install("{}", settings_path, hook_path).unwrap();
        let settings: Value = serde_json::from_str(&installed).unwrap();
        for (event, action, matcher) in SUBAGENT_HOOKS {
            assert_eq!(
                settings["hooks"][event],
                Value::Array(vec![canonical_value(hook_path, action, matcher)])
            );
        }
        for input in [
            "[]",
            r#"{"hooks": []}"#,
            r#"{"hooks":{"SubagentStart":{}}}"#,
        ] {
            assert!(install(input, settings_path, hook_path).is_err(), "{input}");
        }
    }

    // Fork (browser steering): the shipped asset's `browser` action, run
    // the way Claude Code runs a PreToolUse hook (JSON on stdin, the
    // decision on stdout). No socket: the action never connects.
    #[cfg(unix)]
    mod browser_steering {
        use std::path::PathBuf;
        use std::process::{Command, Stdio};

        use serde_json::{json, Value};
        use sha2::{Digest, Sha256};

        const ASSET: &str = include_str!("assets/claude/herdr-agent-state.sh");

        struct Fixture {
            dir: PathBuf,
            state: PathBuf,
            config: PathBuf,
        }

        impl Fixture {
            fn new(name: &str) -> Self {
                let dir = std::env::temp_dir()
                    .join(format!("herdr-browser-steer-{name}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(dir.join("claude")).unwrap();
                std::fs::write(dir.join("hook.sh"), ASSET).unwrap();
                std::fs::write(
                    dir.join("claude/.claude.json"),
                    r#"{"mcpServers":{"herdr-browser":{"type":"stdio"}}}"#,
                )
                .unwrap();
                Fixture {
                    state: dir.join("state/browser-steer"),
                    config: dir.join("config.toml"),
                    dir,
                }
            }

            fn run_with(&self, input: &Value, steer_env: bool) -> String {
                use std::io::Write as _;
                let mut command = Command::new("sh");
                command
                    .arg(self.dir.join("hook.sh"))
                    .arg("browser")
                    .env("HERDR_ENV", "1")
                    .env("HERDR_SOCKET_PATH", self.dir.join("unused.sock"))
                    .env("HERDR_PANE_ID", "w1:p1")
                    .env("CLAUDE_CONFIG_DIR", self.dir.join("claude"))
                    .env_remove("CURSOR_VERSION")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if steer_env {
                    command
                        .env("HERDR_BROWSER_STEER_STATE", &self.state)
                        .env("HERDR_BROWSER_STEER_CONFIG", &self.config);
                } else {
                    command
                        .env_remove("HERDR_BROWSER_STEER_STATE")
                        .env_remove("HERDR_BROWSER_STEER_CONFIG");
                }
                let mut child = command.spawn().expect("run the claude hook asset");
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(input.to_string().as_bytes())
                    .unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    output.stderr.is_empty(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                String::from_utf8(output.stdout).unwrap()
            }

            fn run(&self, input: &Value) -> String {
                self.run_with(input, true)
            }

            /// The deny reason, or None when the call is allowed (nothing printed).
            fn denied(&self, input: &Value) -> Option<String> {
                let out = self.run(input);
                if out.trim().is_empty() {
                    return None;
                }
                let decision: Value = serde_json::from_str(&out).expect(&out);
                let output = &decision["hookSpecificOutput"];
                assert_eq!(output["hookEventName"], "PreToolUse");
                assert_eq!(output["permissionDecision"], "deny");
                Some(
                    output["permissionDecisionReason"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                )
            }
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }

        fn tool(session: &str, name: &str, input: Value) -> Value {
            json!({
                "hook_event_name": "PreToolUse",
                "session_id": session,
                "cwd": "/work/site",
                "tool_name": name,
                "tool_input": input,
            })
        }

        fn bash(session: &str, command: &str) -> Value {
            tool(session, "Bash", json!({ "command": command }))
        }

        #[test]
        fn fork_smoke_browser_steering_denies_once_and_allows_the_retry() {
            let fx = Fixture::new("retry");
            let fetch = tool(
                "s1",
                "WebFetch",
                json!({ "url": "https://docs.rs/x", "prompt": "p" }),
            );
            let reason = fx.denied(&fetch).expect("WebFetch is steered");
            assert!(
                reason.contains("browser_open https://docs.rs/x"),
                "{reason}"
            );
            assert!(reason.contains("browser_read"), "{reason}");
            assert!(reason.contains("again within 2 minutes"), "{reason}");
            assert_eq!(fx.denied(&fetch), None, "the retry runs");
            assert_eq!(fx.denied(&fetch), None, "and keeps the window open");
            // Another session or another target is a first call.
            assert!(fx
                .denied(&tool(
                    "s2",
                    "WebFetch",
                    json!({ "url": "https://docs.rs/x" })
                ))
                .is_some());
            assert!(fx
                .denied(&tool(
                    "s1",
                    "WebFetch",
                    json!({ "url": "https://docs.rs/y" })
                ))
                .is_some());
            // An expired window denies again.
            let key = Sha256::digest("s1\0fetch:https://docs.rs/x".as_bytes());
            let key: String = key.iter().map(|b| format!("{b:02x}")).collect();
            let marker = fx.state.join(&key[..32]);
            let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
            std::fs::File::options()
                .write(true)
                .open(&marker)
                .expect("the denial marker")
                .set_modified(old)
                .unwrap();
            assert!(fx.denied(&fetch).is_some(), "past the window");
        }

        #[test]
        fn bash_commands_that_open_a_page_are_steered_and_others_are_not() {
            let fx = Fixture::new("bash");
            for command in [
                "open https://example.com",
                "cd out && open report.html",
                "open -a \"Google Chrome\" http://localhost:3000/app",
                "FOO=1 open file:///tmp/a.html",
                "xdg-open 'https://example.com/a b'",
                "gio open index.htm",
                "python3 -m webbrowser -t https://example.com",
                "osascript -e 'open location \"https://example.com\"'",
                "sensible-browser https://example.com",
            ] {
                assert!(fx.denied(&bash(command, command)).is_some(), "{command}");
            }
            for command in [
                "open shot.png",
                "open report.pdf",
                "open .",
                "open -a Finder ~/Downloads",
                "open -e page.html",
                "open -R page.html",
                "echo \"open https://example.com\"",
                "grep -n open page.html",
                "# open https://example.com",
                "curl -s https://example.com",
                "opener https://example.com",
                "cat \"unterminated",
            ] {
                assert_eq!(fx.denied(&bash(command, command)), None, "{command}");
            }
            let reason = fx.denied(&bash("s", "open docs/index.html")).unwrap();
            assert!(reason.contains("browser_show"), "{reason}");
            assert!(reason.contains("/work/site/docs/index.html"), "{reason}");
            let reason = fx.denied(&bash("s", "open https://example.com/x")).unwrap();
            assert!(
                reason.contains("browser_open https://example.com/x"),
                "{reason}"
            );
        }

        #[test]
        fn other_browser_mcp_tools_are_steered_but_not_herdr_browser() {
            let fx = Fixture::new("mcp");
            for name in [
                "mcp__playwright__browser_navigate",
                "mcp__puppeteer__puppeteer_navigate",
                "mcp__chrome-devtools__navigate_page",
                "mcp__claude-in-chrome__navigate",
            ] {
                let reason = fx.denied(&tool("s", name, json!({}))).expect(name);
                assert!(reason.contains("browser_open"), "{reason}");
            }
            for name in [
                "mcp__herdr-browser__browser_open",
                "mcp__herdr-browser__browser_use",
                "mcp__herdr_agents__agents_notify",
                "mcp__context7__query-docs",
                "WebSearch",
                "Read",
            ] {
                assert_eq!(
                    fx.denied(&tool("s", name, json!({ "url": "https://x.y" }))),
                    None,
                    "{name}"
                );
            }
        }

        #[test]
        fn steering_off_allows_everything() {
            let fx = Fixture::new("off");
            let fetch = tool("s", "WebFetch", json!({ "url": "https://example.com" }));
            assert_eq!(
                fx.run_with(&fetch, false),
                "",
                "a launch the wrap did not mark"
            );
            for config in [
                "[browser]\nsteer_agents = false\n",
                "[ browser ]  # mine\nenabled = false # off\n",
                "browser.steer_agents = false\n",
            ] {
                std::fs::write(&fx.config, config).unwrap();
                assert_eq!(fx.denied(&fetch), None, "{config}");
            }
            std::fs::write(
                &fx.config,
                "[news]\nenabled = false\n[browser]\nsteer_agents = true\n",
            )
            .unwrap();
            assert!(fx.denied(&fetch).is_some(), "another table's key");
            std::fs::write(fx.dir.join("claude/.claude.json"), r#"{"mcpServers":{}}"#).unwrap();
            assert_eq!(
                fx.denied(&fetch),
                None,
                "no herdr-browser server for claude"
            );
            std::fs::remove_file(fx.dir.join("claude/.claude.json")).unwrap();
            assert_eq!(fx.denied(&fetch), None, "no MCP store");
        }

        #[test]
        fn errors_allow_the_call() {
            let fx = Fixture::new("errors");
            std::fs::create_dir_all(fx.state.parent().unwrap()).unwrap();
            std::fs::write(&fx.state, "not a directory").unwrap();
            let fetch = tool("s", "WebFetch", json!({ "url": "https://example.com" }));
            assert_eq!(fx.denied(&fetch), None, "the denial cannot be recorded");
            std::fs::remove_file(&fx.state).unwrap();
            for input in [
                json!({ "hook_event_name": "PostToolUse", "tool_name": "WebFetch", "tool_input": { "url": "https://x.y" } }),
                json!({ "hook_event_name": "PreToolUse", "tool_name": "WebFetch", "tool_input": "x" }),
                json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash" }),
            ] {
                assert_eq!(fx.denied(&input), None, "{input}");
            }
        }
    }
}
