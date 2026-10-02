//! The Agents settings section's methods (fork): `agents.settings`,
//! `agents.settings.set` and `agents.fix`.
//!
//! The facts come from `agent_wrap::settings_snapshot` over the applied
//! `[agents]` / `[browser]` config and the server's environment. A setter
//! writes one `[agents]` key (`ConfigEdit`), reloads live and checks the
//! reload applied it. `agents.fix` repairs the shell hook, the only fix
//! that exists, and it edits `~/.zshrc`: it runs only when the request says
//! `confirm: true`; otherwise the answer is `confirm_required` and nothing
//! is written.

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::agent_wrap::{self, WrapCheck, WrapSnapshot};
use crate::api::schema::agent_wrap::{check, error_code, key};
use crate::api::schema::{
    AgentsCheckInfo, AgentsCheckState, AgentsFixParams, AgentsSettingsInfo,
    AgentsSettingsSetParams, AgentsWrapSource, ResponseResult,
};
use crate::browser::setup::SetupEnv;
use crate::config::{ConfigEdit, WrapSource};

/// A refusal: an error code and message.
type Refusal = (&'static str, String);

impl App {
    /// Where the checks look and the fix writes: the server's environment,
    /// or the test's temporary home.
    fn agents_setup_env(&self) -> SetupEnv {
        self.agents_setup_env
            .clone()
            .unwrap_or_else(SetupEnv::from_process)
    }

    /// The applied config the section describes (`[agents]` as last loaded,
    /// `[browser]` as the hub holds it).
    fn agents_config_view(&self) -> crate::config::Config {
        let mut config = crate::config::Config::default();
        config.agents = self.agents_config.clone();
        config.browser = crate::browser::hub().config();
        config
    }

    fn agents_snapshot(&self, env: &SetupEnv) -> WrapSnapshot {
        agent_wrap::settings_snapshot(&self.agents_config_view(), env)
    }

    /// `agents.settings`.
    pub(super) fn handle_agents_settings(&mut self, id: String) -> String {
        let env = self.agents_setup_env();
        encode_success(
            id,
            ResponseResult::AgentsSettings {
                info: settings_info(self.agents_snapshot(&env)),
            },
        )
    }

    /// `agents.settings.set`: one `[agents]` key into the server's config
    /// file, reloaded live. Never touches `.zshrc`.
    pub(super) fn handle_agents_settings_set(
        &mut self,
        id: String,
        params: AgentsSettingsSetParams,
    ) -> String {
        match self.set_agents_setting(&params.key, &params.value) {
            Ok(()) => {
                let env = self.agents_setup_env();
                encode_success(
                    id,
                    ResponseResult::AgentsSettings {
                        info: settings_info(self.agents_snapshot(&env)),
                    },
                )
            }
            Err((code, message)) => encode_error(id, code, message),
        }
    }

    fn set_agents_setting(&mut self, name: &str, value: &serde_json::Value) -> Result<(), Refusal> {
        let invalid = |what: String| (error_code::INVALID_REQUEST, what);
        let as_bool = || {
            value.as_bool().ok_or_else(|| {
                invalid(format!(
                    "agents.settings.set {name}: value must be true or false"
                ))
            })
        };
        let file: Option<String>;
        let edit = match name {
            key::WRAP => ConfigEdit::AgentsWrap(as_bool()?),
            key::TOOLS => ConfigEdit::AgentsBool {
                key: key::TOOLS,
                value: as_bool()?,
            },
            key::INSTRUCTIONS => ConfigEdit::AgentsBool {
                key: key::INSTRUCTIONS,
                value: as_bool()?,
            },
            key::NOTICES => ConfigEdit::AgentsBool {
                key: key::NOTICES,
                value: as_bool()?,
            },
            key::INSTRUCTIONS_FILE => {
                let text = value.as_str().map(str::trim).ok_or_else(|| {
                    invalid(format!(
                        "agents.settings.set {name}: value must be a string (\"default\", \"file\" or a path)"
                    ))
                })?;
                if text.chars().any(char::is_control) {
                    return Err(invalid(format!(
                        "agents.settings.set {name}: the path has control characters"
                    )));
                }
                file = match text {
                    "" | "default" => None,
                    "file" => {
                        let path = agent_wrap::instructions::default_file_path();
                        agent_wrap::seed_instructions_file(&path).map_err(|err| {
                            (
                                error_code::CONFIG_WRITE_FAILED,
                                format!("could not create {}: {err}", path.display()),
                            )
                        })?;
                        let env = self.agents_setup_env();
                        Some(crate::browser::setup::shorten_home(
                            &path,
                            env.home.as_deref(),
                        ))
                    }
                    path => Some(path.to_string()),
                };
                ConfigEdit::AgentsInstructionsFile(file.as_deref())
            }
            other => {
                return Err(invalid(format!(
                    "agents.settings.set: unknown key {other:?} (wrap, tools, instructions, instructions_file, notices)"
                )))
            }
        };
        // What the running config must say afterwards.
        let requested = match &edit {
            ConfigEdit::AgentsWrap(wrap) => serde_json::Value::Bool(*wrap),
            ConfigEdit::AgentsBool { value, .. } => serde_json::Value::Bool(*value),
            ConfigEdit::AgentsInstructionsFile(path) => {
                serde_json::Value::String(path.unwrap_or_default().trim().to_string())
            }
            _ => serde_json::Value::Null,
        };
        crate::config::write_edit(edit).map_err(|err| (error_code::CONFIG_WRITE_FAILED, err))?;
        let report = self.reload_config();
        let actual = self.agents_setting_value(name);
        if actual != requested {
            tracing::warn!(
                event = "agents.settings.set",
                key = name,
                status = ?report.status,
                "agents setting written but not applied by the reload"
            );
            let why = report
                .diagnostics
                .iter()
                .find(|d| d.contains("agents") || d.contains("browser"))
                .cloned()
                .unwrap_or_else(|| "the [agents] section did not reload".to_string());
            return Err((
                error_code::CONFIG_WRITE_FAILED,
                format!("written but not applied: {why}"),
            ));
        }
        tracing::info!(
            event = "agents.settings.set",
            key = name,
            status = ?report.status,
            "agents setting changed"
        );
        Ok(())
    }

    /// The running config's value for a `agents.settings.set` key, in the
    /// shape the request carried (`wrap`: only `[agents] wrap` counts).
    fn agents_setting_value(&self, name: &str) -> serde_json::Value {
        let agents = &self.agents_config;
        match name {
            key::WRAP => {
                let (wrap, source) = self.agents_config_view().agents_wrap();
                if source == WrapSource::Agents {
                    wrap.into()
                } else {
                    serde_json::Value::Null
                }
            }
            key::TOOLS => agents.tools.into(),
            key::INSTRUCTIONS => agents.instructions.into(),
            key::NOTICES => agents.notices.into(),
            key::INSTRUCTIONS_FILE => agents.instructions_file.trim().into(),
            _ => serde_json::Value::Null,
        }
    }

    /// `agents.fix`: the requested checks (empty = every fixable one). A fix
    /// that edits files runs only with `confirm`; without it nothing runs.
    pub(super) fn handle_agents_fix(&mut self, id: String, params: AgentsFixParams) -> String {
        let env = self.agents_setup_env();
        let snapshot = self.agents_snapshot(&env);
        let known = [check::SHELL_HOOK, check::CLAUDE, check::CODEX];
        if let Some(unknown) = params.ids.iter().find(|id| !known.contains(&id.as_str())) {
            return encode_error(
                id,
                error_code::INVALID_REQUEST,
                format!("agents.fix: unknown check {unknown:?} (shell_hook, claude, codex)"),
            );
        }
        let selected: Vec<&WrapCheck> = snapshot
            .checks
            .iter()
            .filter(|check| {
                if params.ids.is_empty() {
                    check.fixable
                } else {
                    params.ids.iter().any(|id| id == check.id)
                }
            })
            .collect();
        // The shell hook's fix always edits `.zshrc`: ask first, whatever
        // its current state.
        let needs_confirm = selected
            .iter()
            .any(|check| check.edits_files || check.id == check::SHELL_HOOK);
        if needs_confirm && !params.confirm {
            return encode_error(
                id,
                error_code::CONFIRM_REQUIRED,
                "agents.fix: this fix edits your files (~/.zshrc); confirm it first (confirm: true)",
            );
        }
        for check in selected.into_iter().filter(|check| check.fixable) {
            if check.id != check::SHELL_HOOK {
                continue;
            }
            match agent_wrap::fix_shell_hook(&env) {
                Ok(note) => {
                    tracing::info!(event = "agents.fix", check = check.id, note, "fixed");
                }
                Err(err) => {
                    tracing::warn!(event = "agents.fix", check = check.id, err, "fix failed");
                    return encode_error(id, error_code::FIX_FAILED, err);
                }
            }
        }
        let after = self.agents_snapshot(&env);
        encode_success(
            id,
            ResponseResult::AgentsFix {
                results: after.checks.into_iter().map(check_info).collect(),
            },
        )
    }
}

fn wrap_source(source: WrapSource) -> AgentsWrapSource {
    match source {
        WrapSource::Agents => AgentsWrapSource::Agents,
        WrapSource::LegacyBrowser => AgentsWrapSource::BrowserLegacy,
        WrapSource::Default => AgentsWrapSource::Default,
    }
}

fn check_state(state: &str) -> AgentsCheckState {
    match state {
        "ok" => AgentsCheckState::Ok,
        "outdated" => AgentsCheckState::Outdated,
        "missing" => AgentsCheckState::Missing,
        "absent" => AgentsCheckState::Absent,
        _ => AgentsCheckState::Unknown,
    }
}

fn check_info(check: WrapCheck) -> AgentsCheckInfo {
    AgentsCheckInfo {
        id: check.id.to_string(),
        state: check_state(check.state),
        detail: check.detail,
        fixable: check.fixable,
        edits_files: check.edits_files,
    }
}

fn settings_info(snapshot: WrapSnapshot) -> AgentsSettingsInfo {
    AgentsSettingsInfo {
        wrap: snapshot.wrap,
        wrap_source: wrap_source(snapshot.wrap_source),
        tools: snapshot.tools,
        instructions: snapshot.instructions,
        instructions_file: snapshot.instructions_file,
        instructions_detail: snapshot.instructions_detail,
        steer_browser: snapshot.steer_browser,
        notices: snapshot.notices,
        checks: snapshot.checks.into_iter().map(check_info).collect(),
        hook_preview: snapshot.hook_preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_states_and_sources_map_onto_the_wire() {
        assert_eq!(check_state("ok"), AgentsCheckState::Ok);
        assert_eq!(check_state("outdated"), AgentsCheckState::Outdated);
        assert_eq!(check_state("missing"), AgentsCheckState::Missing);
        assert_eq!(check_state("absent"), AgentsCheckState::Absent);
        assert_eq!(check_state("??"), AgentsCheckState::Unknown);
        for (source, wire) in [
            (WrapSource::Agents, "agents"),
            (WrapSource::LegacyBrowser, "browser_legacy"),
            (WrapSource::Default, "default"),
        ] {
            assert_eq!(serde_json::to_value(wrap_source(source)).unwrap(), wire);
        }
    }
}
