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
        if !cfg!(test) {
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
    /// file (`ConfigEdit`), reloaded live; `mcp_agents` and `shell_hook`
    /// also run their file-editing fix (the request is the user's explicit
    /// act), the others refresh the checks.
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
            "wrap_agents" => ConfigEdit::BrowserBool {
                key: "wrap_agents",
                value: as_bool()?,
            },
            "disable_native_browser" => ConfigEdit::BrowserBool {
                key: "disable_native_browser",
                value: as_bool()?,
            },
            "shell_hook" => ConfigEdit::BrowserBool {
                key: "shell_hook",
                value: as_bool()?,
            },
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
        crate::config::write_edit(edit)
            .map_err(|err| BrowserError::new("browser_config_write_failed", err))?;
        let report = self.reload_config();
        tracing::info!(
            event = "browser.settings.set",
            key,
            status = ?report.status,
            "browser setting changed"
        );
        match key {
            "mcp_agents" => hub().run_fixes(vec!["mcp_claude".into(), "mcp_codex".into()]),
            "shell_hook" => hub().run_fixes(vec!["shell_hook".into()]),
            _ => hub().refresh_checks(true),
        }
        Ok(())
    }

    pub(super) fn handle_browser_get(&mut self, id: String, params: BrowserGetParams) -> String {
        self.release_gone_panes();
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
        match self.browser_actor_for_pane(&params.pane_id) {
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
