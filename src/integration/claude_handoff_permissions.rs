//! Fork (agent handoff, src/app/agent_handoff.rs): `permissions.allow`
//! rules in Claude's settings that let the agent write and read its handoff
//! under herdr's state dir (`<state_dir>/handoffs/`) without a permission
//! prompt. The handoff path is outside the agent's working directory, so in
//! Claude's default mode (and for a path outside the cwd in `acceptEdits`)
//! its Write would wait on the user and the automatic handoff would stall.
//!
//! Claude checks file permissions against `Edit(path)` and `Read(path)`
//! rules only (an `Edit` rule covers the Write tool); `//` anchors a path at
//! the filesystem root. The rules name only herdr's handoffs directory.
//! Applied after the hook edits (`claude_settings`, `claude_subagent_hooks`)
//! with the same CST-preserving edit and re-parse check; `uninstall` removes
//! exactly these rules and leaves the user's.

use std::io;
use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstRootNode};
use jsonc_parser::ParseOptions;
use serde_json::{Map, Value};

/// The rules for a handoffs directory (an absolute path).
pub(crate) fn rules(handoffs_dir: &Path) -> Vec<String> {
    let path = handoffs_dir.to_string_lossy().replace('\\', "/");
    let path = path.trim_end_matches('/');
    let anchored = format!("//{}/**", path.trim_start_matches('/'));
    vec![format!("Edit({anchored})"), format!("Read({anchored})")]
}

/// The rules for this install's state dir.
pub(crate) fn current_rules() -> Vec<String> {
    rules(&crate::agent_handoff::root(&crate::config::state_dir()))
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

fn invalid(settings_path: &Path, what: &str) -> io::Error {
    io::Error::other(format!(
        "claude {what} at {} has an unexpected shape",
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

/// Add each of `rules` to `permissions.allow` unless it is there.
pub(crate) fn install(content: &str, settings_path: &Path, rules: &[String]) -> io::Result<String> {
    let original = parse_value(content, settings_path)?;
    let mut desired = original.clone();
    let root = desired
        .as_object_mut()
        .ok_or_else(|| invalid(settings_path, "settings"))?;
    let allow = root
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| invalid(settings_path, "settings permissions"))?
        .entry("allow")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| invalid(settings_path, "settings permissions.allow"))?;
    let missing: Vec<&String> = rules
        .iter()
        .filter(|rule| {
            !allow
                .iter()
                .any(|value| value.as_str() == Some(rule.as_str()))
        })
        .collect();
    for rule in &missing {
        allow.push(Value::String((*rule).clone()));
    }
    if desired == original {
        return Ok(content.to_string());
    }

    let cst = parse_root(content, settings_path)?;
    let root_object = cst
        .value()
        .and_then(|value| value.as_object())
        .ok_or_else(|| invalid(settings_path, "settings"))?;
    let permissions = match root_object.get("permissions") {
        Some(property) => property
            .object_value()
            .ok_or_else(|| invalid(settings_path, "settings permissions"))?,
        None => root_object
            .append("permissions", CstInputValue::Object(Vec::new()))
            .object_value()
            .ok_or_else(|| invalid(settings_path, "settings permissions"))?,
    };
    match permissions.get("allow") {
        Some(property) => {
            let allow = property
                .array_value()
                .ok_or_else(|| invalid(settings_path, "settings permissions.allow"))?;
            for rule in missing {
                allow.append(CstInputValue::String(rule.clone()));
            }
        }
        None => {
            permissions.append(
                "allow",
                CstInputValue::Array(
                    missing
                        .into_iter()
                        .map(|rule| CstInputValue::String(rule.clone()))
                        .collect(),
                ),
            );
        }
    }
    verify(cst.to_string(), settings_path, &desired)
}

/// Remove each of `rules` from `permissions.allow`; other rules stay, and
/// so does the (possibly empty) array.
pub(crate) fn uninstall(
    content: &str,
    settings_path: &Path,
    rules: &[String],
) -> io::Result<String> {
    let original = parse_value(content, settings_path)?;
    let mut desired = original.clone();
    let Some(allow) = desired
        .as_object_mut()
        .and_then(|root| root.get_mut("permissions"))
        .and_then(Value::as_object_mut)
        .and_then(|permissions| permissions.get_mut("allow"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(content.to_string());
    };
    allow.retain(|value| {
        !value
            .as_str()
            .is_some_and(|rule| rules.iter().any(|r| r == rule))
    });
    if desired == original {
        return Ok(content.to_string());
    }

    let cst = parse_root(content, settings_path)?;
    let Some(allow) = cst
        .value()
        .and_then(|value| value.as_object())
        .and_then(|root| root.get("permissions"))
        .and_then(|property| property.object_value())
        .and_then(|permissions| permissions.get("allow"))
        .and_then(|property| property.array_value())
    else {
        return Ok(content.to_string());
    };
    for element in allow.elements() {
        let matches = element
            .to_serde_value()
            .and_then(|value| value.as_str().map(str::to_string))
            .is_some_and(|rule| rules.contains(&rule));
        if matches {
            element.remove();
        }
    }
    verify(cst.to_string(), settings_path, &desired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_path() -> &'static Path {
        Path::new("/home/test/.claude/settings.json")
    }

    fn handoff_rules() -> Vec<String> {
        rules(Path::new("/home/test/.local/state/herdr/handoffs"))
    }

    #[test]
    fn the_rules_anchor_the_handoffs_dir_at_the_filesystem_root() {
        assert_eq!(
            handoff_rules(),
            vec![
                "Edit(//home/test/.local/state/herdr/handoffs/**)".to_string(),
                "Read(//home/test/.local/state/herdr/handoffs/**)".to_string(),
            ]
        );
    }

    #[test]
    fn install_adds_the_rules_once_and_keeps_the_users() {
        let rules = handoff_rules();
        let content = "{\n  \"permissions\": {\"allow\": [\"Read\"]},\n  \"model\": \"opus\"\n}\n";
        let installed = install(content, settings_path(), &rules).unwrap();
        let value: Value = serde_json::from_str(&installed).unwrap();
        assert_eq!(
            value["permissions"]["allow"],
            serde_json::json!(["Read", rules[0], rules[1]])
        );
        assert_eq!(value["model"], "opus");
        assert_eq!(
            install(&installed, settings_path(), &rules).unwrap(),
            installed
        );
        // Without a permissions object.
        let installed = install("{}", settings_path(), &rules).unwrap();
        let value: Value = serde_json::from_str(&installed).unwrap();
        assert_eq!(value["permissions"]["allow"], serde_json::json!(rules));
    }

    #[test]
    fn uninstall_removes_only_the_rules() {
        let rules = handoff_rules();
        let content = "{\"permissions\": {\"allow\": [\"Read\"]}}";
        let installed = install(content, settings_path(), &rules).unwrap();
        let removed = uninstall(&installed, settings_path(), &rules).unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(value["permissions"]["allow"], serde_json::json!(["Read"]));
        assert_eq!(uninstall("{}", settings_path(), &rules).unwrap(), "{}");
    }

    #[test]
    fn a_permissions_value_of_another_shape_is_an_error() {
        let rules = handoff_rules();
        assert!(install("{\"permissions\": []}", settings_path(), &rules).is_err());
        assert!(install(
            "{\"permissions\": {\"allow\": {}}}",
            settings_path(),
            &rules
        )
        .is_err());
    }
}
