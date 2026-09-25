//! Claude Code `SubagentStart` / `SubagentStop` hooks for the fork's subagent
//! count (`pane.report_subagent`).
//!
//! Applied after the upstream settings edit (`claude_settings`), so that
//! module stays as upstream wrote it: `install` adds one entry per event that
//! runs the Claude hook asset with the `subagent` action, `uninstall` removes
//! the asset's `subagent` commands again. User hooks and formatting outside
//! the touched arrays are kept; the result is re-parsed and must equal the
//! intended settings value, as the upstream edit does.

use std::io;
use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::{json, ParseOptions};
use serde_json::{json as serde_json_value, Map, Value};

use super::command::hook_command;
use super::config_edit::{hook_command_variants, is_matching_command_hook};

/// The hook events that report subagents.
pub(crate) const SUBAGENT_HOOK_EVENTS: [&str; 2] = ["SubagentStart", "SubagentStop"];
/// The asset action the subagent hooks run.
pub(crate) const SUBAGENT_HOOK_ACTION: &str = "subagent";
const SUBAGENT_HOOK_TIMEOUT: u64 = 10;

fn canonical_value(hook_path: &Path) -> Value {
    serde_json_value!({
        "hooks": [{
            "type": "command",
            "command": hook_command(hook_path, Some(SUBAGENT_HOOK_ACTION)),
            "timeout": SUBAGENT_HOOK_TIMEOUT,
        }],
    })
}

fn canonical_input(hook_path: &Path) -> CstInputValue {
    let command = hook_command(hook_path, Some(SUBAGENT_HOOK_ACTION));
    json!({
        hooks: [{
            "type": "command",
            command: command,
            timeout: SUBAGENT_HOOK_TIMEOUT,
        }],
    })
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

/// Add a `SubagentStart` and a `SubagentStop` entry running the asset's
/// `subagent` action, unless the event already runs it.
pub(crate) fn install(content: &str, settings_path: &Path, hook_path: &Path) -> io::Result<String> {
    let commands = hook_command_variants(hook_path, Some(SUBAGENT_HOOK_ACTION));
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
    let missing: Vec<&str> = SUBAGENT_HOOK_EVENTS
        .into_iter()
        .filter(|event| !event_has_subagent_hook(hooks, event, &commands))
        .collect();
    for event in &missing {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                io::Error::other(format!("hook entries for {event} must be an array"))
            })?;
        entries.push(canonical_value(hook_path));
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
    for event in missing {
        match hooks_object.get(event) {
            Some(property) => {
                property
                    .array_value()
                    .ok_or_else(|| {
                        io::Error::other(format!("hook entries for {event} must be an array"))
                    })?
                    .append(canonical_input(hook_path));
            }
            None => {
                hooks_object.append(
                    event,
                    CstInputValue::Array(vec![canonical_input(hook_path)]),
                );
            }
        }
    }
    verify(cst.to_string(), settings_path, &desired)
}

/// Remove the asset's `subagent` commands from `SubagentStart` and
/// `SubagentStop`, dropping groups and events they leave empty.
pub(crate) fn uninstall(
    content: &str,
    settings_path: &Path,
    hook_path: &Path,
) -> io::Result<String> {
    let commands = hook_command_variants(hook_path, Some(SUBAGENT_HOOK_ACTION));
    let original = parse_value(content, settings_path)?;
    let mut desired = original.clone();
    let Some(hooks) = desired
        .as_object_mut()
        .and_then(|root| root.get_mut("hooks"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(content.to_string());
    };
    for event in SUBAGENT_HOOK_EVENTS {
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
    for event in SUBAGENT_HOOK_EVENTS {
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
    fn install_adds_both_events_once_and_keeps_user_hooks() {
        let (settings_path, hook_path) = paths();
        let command = hook_command(hook_path, Some(SUBAGENT_HOOK_ACTION));
        let input = concat!(
            "{\n",
            "  \"permissions\": {\"allow\": [\"Read\"]},\n",
            "  \"hooks\": {\n",
            "    \"SubagentStop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo keep\"}]}]\n",
            "  }\n",
            "}\n",
        );

        let installed = install(input, settings_path, hook_path).unwrap();
        let settings: Value = serde_json::from_str(&installed).unwrap();
        assert_eq!(
            subagent_commands(&settings, "SubagentStart"),
            std::slice::from_ref(&command)
        );
        assert_eq!(
            subagent_commands(&settings, "SubagentStop"),
            ["echo keep".to_string(), command.clone()]
        );
        assert!(command.ends_with(" subagent"), "{command}");
        assert!(installed.contains("\"permissions\": {\"allow\": [\"Read\"]}"));
        assert_eq!(
            settings["hooks"]["SubagentStart"][0]["hooks"][0]["timeout"],
            10
        );

        assert_eq!(
            install(&installed, settings_path, hook_path).unwrap(),
            installed
        );

        let removed = uninstall(&installed, settings_path, hook_path).unwrap();
        let settings: Value = serde_json::from_str(&removed).unwrap();
        assert!(settings["hooks"].get("SubagentStart").is_none());
        assert_eq!(subagent_commands(&settings, "SubagentStop"), ["echo keep"]);
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
        for event in SUBAGENT_HOOK_EVENTS {
            assert_eq!(
                settings["hooks"][event],
                Value::Array(vec![canonical_value(hook_path)])
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
}
