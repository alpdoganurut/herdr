//! App-lane browser methods (fork): the fast ones the client shell and the
//! CLI call — `browser.get`, `browser.status`, `browser.focus`,
//! `browser.start`, `browser.stop`, `browser.log`, the profile methods — and
//! the internal `browser.resolve_caller` the connection lane uses to turn a
//! pane id into an actor. Everything reads the process-global hub;
//! `browser.run` never reaches here (it answers `browser_lane`).

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{
    BrowserActor, BrowserCaller, BrowserFixParams, BrowserGetParams, BrowserLogParams,
    BrowserProfileCreateParams, BrowserProfileName, BrowserProfileTarget, BrowserSettingsSetParams,
    BrowserStopParams, BrowserTabTarget, ResponseResult,
};
use crate::browser::{hub, BrowserError};

impl App {
    pub(crate) fn install_browser_hub(&self, config: &crate::config::BrowserConfig) {
        let hub = hub();
        hub.apply_config(config);
        if self.policy.persist_session {
            hub.enable_persistence(&crate::session::data_dir());
        }
        if config.enabled && config.autostart {
            let _ = hub.start(None);
        }
        // The checks behind the Browser row's `setup needed` hint, off this
        // thread (not under cfg(test): every test process would probe the
        // real home directory).
        if !cfg!(test) && config.enabled {
            hub.refresh_checks(false);
        }
    }

    /// `browser.settings`: the section's picture; a stale check cache is
    /// refreshed off this thread and the next pull sees it.
    pub(super) fn handle_browser_settings(&mut self, id: String) -> String {
        hub().refresh_checks(false);
        encode_success(
            id,
            ResponseResult::BrowserSettings {
                settings: hub().setup_info(),
            },
        )
    }

    /// `browser.settings.set`: one `[browser]` key into the server's config
    /// file (`ConfigEdit`), reloaded live; `mcp_agents` also runs its
    /// file-editing fix (the request is the user's explicit act), the others
    /// refresh the checks. `wrap_agents` / `steer_wrap` write `[agents] wrap`
    /// (dropping the legacy key); `shell_hook` moved to the Agents section
    /// (`moved`), so nothing here edits `.zshrc`.
    pub(super) fn handle_browser_settings_set(
        &mut self,
        id: String,
        params: BrowserSettingsSetParams,
    ) -> String {
        match self.set_browser_setting(&params.key, &params.value) {
            Ok(()) => encode_success(
                id,
                ResponseResult::BrowserSettings {
                    settings: hub().setup_info(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    /// `browser.fix`: the fixes run off this thread; the reply shows `fixing`.
    pub(super) fn handle_browser_fix(&mut self, id: String, params: BrowserFixParams) -> String {
        hub().run_fixes(params.ids);
        encode_success(
            id,
            ResponseResult::BrowserSettings {
                settings: hub().setup_info(),
            },
        )
    }

    fn set_browser_setting(
        &mut self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), BrowserError> {
        use crate::config::ConfigEdit;
        let invalid = |what: &str| BrowserError::new("invalid_request", what.to_string());
        let as_bool = || {
            value.as_bool().ok_or_else(|| {
                invalid(&format!(
                    "browser.settings.set {key}: value must be true or false"
                ))
            })
        };
        let list: Vec<String>;
        let text: String;
        let steer: [(&'static str, bool); 1];
        let edit = match key {
            "enabled" => ConfigEdit::BrowserBool {
                key: "enabled",
                value: as_bool()?,
            },
            "show_activity" => ConfigEdit::BrowserBool {
                key: "show_activity",
                value: as_bool()?,
            },
            "pin_dashboard" => ConfigEdit::BrowserBool {
                key: "pin_dashboard",
                value: as_bool()?,
            },
            "steer_agents" => ConfigEdit::BrowserBool {
                key: "steer_agents",
                value: as_bool()?,
            },
            // compatibility: the wrap switch is `[agents] wrap` now
            "wrap_agents" => ConfigEdit::AgentsWrap(as_bool()?),
            "disable_native_browser" => ConfigEdit::BrowserBool {
                key: "disable_native_browser",
                value: as_bool()?,
            },
            "shell_hook" => {
                return Err(BrowserError::new(
                    "moved",
                    "the shell hook is in Settings → Agents (it edits ~/.zshrc only after a confirmation)",
                ))
            }
            // the old settings row `agents use herdr's browser`: steer_agents
            // and `[agents] wrap`, one write
            "steer_wrap" => {
                let value = as_bool()?;
                steer = [("steer_agents", value)];
                ConfigEdit::AgentsWrapWithBrowser {
                    wrap: value,
                    browser: &steer,
                }
            }
            "activity_color" => {
                text = value
                    .as_str()
                    .map(str::trim)
                    .filter(|c| {
                        c.len() == 7
                            && c.starts_with('#')
                            && c[1..].chars().all(|ch| ch.is_ascii_hexdigit())
                    })
                    .ok_or_else(|| {
                        invalid("browser.settings.set activity_color: value must be \"#rrggbb\"")
                    })?
                    .to_ascii_lowercase();
                ConfigEdit::BrowserString {
                    key: "activity_color",
                    value: &text,
                }
            }
            "mcp_agents" => {
                list = value
                    .as_array()
                    .ok_or_else(|| invalid("browser.settings.set mcp_agents: value must be an array of agent names"))?
                    .iter()
                    .map(|v| v.as_str().map(str::to_string))
                    .collect::<Option<Vec<String>>>()
                    .ok_or_else(|| invalid("browser.settings.set mcp_agents: every entry must be a string"))?;
                if let Some(unknown) = list
                    .iter()
                    .find(|a| !crate::config::MCP_AGENTS.contains(&a.as_str()))
                {
                    return Err(invalid(&format!(
                        "browser.settings.set mcp_agents: unknown agent {unknown:?} (claude, codex)"
                    )));
                }
                ConfigEdit::BrowserList {
                    key: "mcp_agents",
                    values: &list,
                }
            }
            other => {
                return Err(invalid(&format!(
                    "browser.settings.set: unknown key {other:?}"
                )))
            }
        };
        // What the running config must say afterwards (normalised like the edit).
        let requested = match &edit {
            ConfigEdit::BrowserBool { value, .. } => serde_json::Value::Bool(*value),
            ConfigEdit::BrowserString { value, .. } => serde_json::Value::String(value.to_string()),
            ConfigEdit::BrowserList { values, .. } => serde_json::json!(values),
            ConfigEdit::AgentsWrap(wrap) | ConfigEdit::AgentsWrapWithBrowser { wrap, .. } => {
                serde_json::Value::Bool(*wrap)
            }
            _ => serde_json::Value::Null,
        };
        crate::config::write_edit(edit)
            .map_err(|err| BrowserError::new("browser_config_write_failed", err))?;
        let report = self.reload_config();
        // The reload keeps an invalid [browser] section's previous values (and
        // a file that does not parse at all): then the write did not apply,
        // and a fix would act on the old setting.
        let applied = hub().config();
        let actual = browser_config_value(&applied, key);
        if actual != requested {
            tracing::warn!(
                event = "browser.settings.set",
                key,
                status = ?report.status,
                "browser setting written but not applied by the reload"
            );
            let why = report
                .diagnostics
                .iter()
                .find(|d| d.contains("browser") || d.contains("agents"))
                .cloned()
                .unwrap_or_else(|| "the [browser] section did not reload".to_string());
            return Err(BrowserError::new(
                "browser_config_write_failed",
                format!("written but not applied: {why}"),
            ));
        }
        tracing::info!(
            event = "browser.settings.set",
            key,
            status = ?report.status,
            "browser setting changed"
        );
        match key {
            "mcp_agents" => hub().run_fixes(vec!["mcp_claude".into(), "mcp_codex".into()]),
            _ => hub().refresh_checks(true),
        }
        Ok(())
    }

    pub(super) fn handle_browser_get(&mut self, id: String, params: BrowserGetParams) -> String {
        self.release_gone_panes();
        // The `!` hint notices external changes (a hand-edited .zshrc, a
        // removed registration) on a slow cadence: node and launchctl are
        // not worth spawning every 2 s pull.
        hub().refresh_checks_if_older(std::time::Duration::from_secs(5 * 60));
        let mut browser = hub().get(params.since_seq);
        browser.setup_needed = hub().setup_needed();
        // Actors whose pane is gone: re-resolved here, where the App knows.
        for tab in &mut browser.tabs {
            self.mark_gone(&mut tab.opened_by);
            self.mark_gone(&mut tab.last_actor);
            if let Some(last) = tab.last.as_mut() {
                self.mark_gone(&mut last.actor);
            }
        }
        // Cursors: drop vanished panes and re-resolve the pane's herdr tab (a
        // pane moved to another tab must not leave the glyph behind).
        browser
            .recent_panes
            .retain(|cursor| self.parse_pane_id(&cursor.pane_id).is_some());
        for cursor in &mut browser.recent_panes {
            if let Some(tab_id) = self
                .browser_actor_for_pane(&cursor.pane_id)
                .and_then(|actor| actor.tab_id().map(str::to_string))
            {
                cursor.tab_id = Some(tab_id);
            }
        }
        encode_success(id, ResponseResult::BrowserGet { browser })
    }

    /// Gone panes: their tab groups in the window are dissolved (the hub asks
    /// the sidecar, off this thread). Read from the hub's own ledger on every
    /// call, so an unchanged (`since_seq`) answer still notices them.
    fn release_gone_panes(&self) {
        let gone: Vec<String> = hub()
            .known_pane_ids()
            .into_iter()
            .filter(|pane| self.parse_pane_id(pane).is_none())
            .collect();
        if !gone.is_empty() {
            hub().release_panes(gone);
        }
    }

    fn mark_gone(&self, actor: &mut BrowserActor) {
        if let BrowserActor::Pane { pane_id, gone, .. } = actor {
            *gone = self.parse_pane_id(pane_id).is_none();
        }
    }

    pub(super) fn handle_browser_status(&mut self, id: String) -> String {
        let mut status = hub().status();
        for tab in &mut status.get.tabs {
            self.mark_gone(&mut tab.opened_by);
            self.mark_gone(&mut tab.last_actor);
        }
        self.release_gone_panes();
        encode_success(id, ResponseResult::BrowserStatus { status })
    }

    pub(super) fn handle_browser_focus(&mut self, id: String, params: BrowserTabTarget) -> String {
        match hub().focus(params.profile.as_deref(), &params.tab) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_browser_start(
        &mut self,
        id: String,
        params: BrowserProfileTarget,
    ) -> String {
        match hub().start(params.profile.as_deref()) {
            Ok(browser) => encode_success(id, ResponseResult::BrowserGet { browser }),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_browser_stop(&mut self, id: String, params: BrowserStopParams) -> String {
        match hub().stop(params.profile.as_deref(), params.all) {
            Ok(browser) => encode_success(id, ResponseResult::BrowserGet { browser }),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_browser_log(&mut self, id: String, params: BrowserLogParams) -> String {
        let entries = hub().log(&params);
        encode_success(id, ResponseResult::BrowserLog { entries })
    }

    pub(super) fn handle_browser_profiles(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::BrowserProfiles {
                profiles: hub().profiles(),
            },
        )
    }

    pub(super) fn handle_browser_profile_create(
        &mut self,
        id: String,
        params: BrowserProfileCreateParams,
    ) -> String {
        match hub().profile_create(&params.name, params.temporary) {
            Ok(_) => encode_success(
                id,
                ResponseResult::BrowserProfiles {
                    profiles: hub().profiles(),
                },
            ),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    pub(super) fn handle_browser_profile_delete(
        &mut self,
        id: String,
        params: BrowserProfileName,
    ) -> String {
        match hub().profile_delete(&params.name) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(err) => encode_error(id, err.code(), err.message()),
        }
    }

    /// `browser.resolve_caller`: the pane as it looks now (alias-aware).
    pub(super) fn handle_browser_resolve_caller(
        &mut self,
        id: String,
        params: BrowserCaller,
    ) -> String {
        let actor = self
            .resolve_caller_pane(&params.pane_id)
            .and_then(|(ws_idx, raw)| self.browser_actor_for(ws_idx, raw));
        match actor {
            Some(actor) => encode_success(id, ResponseResult::BrowserActor { actor }),
            None => encode_error(
                id,
                "pane_not_found",
                format!("pane {} not found", params.pane_id),
            ),
        }
    }

    pub(crate) fn browser_actor_for_pane(&self, pane_id: &str) -> Option<BrowserActor> {
        let (ws_idx, raw) = self.parse_pane_id(pane_id)?;
        self.browser_actor_for(ws_idx, raw)
    }

    fn browser_actor_for(&self, ws_idx: usize, raw: crate::layout::PaneId) -> Option<BrowserActor> {
        let pane = self.pane_info(ws_idx, raw)?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(raw)?;
        let tab = self.tab_info(ws_idx, tab_idx)?;
        let workspace = self.workspace_info(ws_idx);
        let shell_pid = ws
            .pane_state(raw)
            .and_then(|state| self.terminal_runtimes.get(&state.attached_terminal_id))
            .and_then(|runtime| runtime.child_pid());
        Some(BrowserActor::Pane {
            pane_id: pane.pane_id,
            tab_id: pane.tab_id,
            workspace_id: pane.workspace_id,
            tab_label: tab.label,
            workspace_label: Some(workspace.label).filter(|label| !label.is_empty()),
            agent: pane.agent,
            session: crate::session::active_name().unwrap_or_else(|| "default".into()),
            gone: false,
            shell_pid,
        })
    }
}

/// The running config's value for a `browser.settings.set` key, in the
/// shape the request carried. `wrap_agents` is the effective `[agents] wrap`
/// (`Config::agents_wrap().0`, carried on the section as `effective_wrap`);
/// `steer_wrap` is its and `steer_agents`' common value, `Null` when they
/// disagree.
fn browser_config_value(config: &crate::config::BrowserConfig, key: &str) -> serde_json::Value {
    match key {
        "enabled" => config.enabled.into(),
        "show_activity" => config.show_activity.into(),
        "pin_dashboard" => config.pin_dashboard.into(),
        "steer_agents" => config.steer_agents.into(),
        "wrap_agents" => config.effective_wrap.into(),
        "disable_native_browser" => config.disable_native_browser.into(),
        "steer_wrap" if config.steer_agents == config.effective_wrap => config.steer_agents.into(),
        "activity_color" => config.activity_color.trim().to_ascii_lowercase().into(),
        "mcp_agents" => serde_json::json!(config.mcp_agents),
        _ => serde_json::Value::Null,
    }
}
