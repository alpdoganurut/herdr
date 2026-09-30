//! App-lane browser methods (fork): the fast ones the client shell and the
//! CLI call — `browser.get`, `browser.status`, `browser.focus`,
//! `browser.start`, `browser.stop`, `browser.log`, the profile methods — and
//! the internal `browser.resolve_caller` the connection lane uses to turn a
//! pane id into an actor. Everything reads the process-global hub;
//! `browser.run` never reaches here (it answers `browser_lane`).

use super::api::responses::{encode_error, encode_success};
use super::App;
use crate::api::schema::{
    BrowserActor, BrowserCaller, BrowserGetParams, BrowserLogParams, BrowserProfileCreateParams,
    BrowserProfileName, BrowserProfileTarget, BrowserStopParams, BrowserTabTarget, ResponseResult,
};
use crate::browser::hub;

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
    }

    pub(super) fn handle_browser_get(&mut self, id: String, params: BrowserGetParams) -> String {
        let mut browser = hub().get(params.since_seq);
        // Actors whose pane is gone: re-resolved here, where the App knows.
        for tab in &mut browser.tabs {
            self.mark_gone(&mut tab.opened_by);
            self.mark_gone(&mut tab.last_actor);
            if let Some(last) = tab.last.as_mut() {
                self.mark_gone(&mut last.actor);
            }
        }
        self.release_gone_panes(&browser);
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
    /// the sidecar once per pane, off this thread).
    fn release_gone_panes(&self, browser: &crate::api::schema::BrowserGetInfo) {
        let mut gone: Vec<String> = browser
            .tabs
            .iter()
            .flat_map(|tab| [&tab.opened_by, &tab.last_actor])
            .filter_map(|actor| match actor {
                BrowserActor::Pane {
                    pane_id,
                    gone: true,
                    ..
                } => Some(pane_id.clone()),
                _ => None,
            })
            .chain(
                browser
                    .recent_panes
                    .iter()
                    .filter(|cursor| self.parse_pane_id(&cursor.pane_id).is_none())
                    .map(|cursor| cursor.pane_id.clone()),
            )
            .collect();
        gone.sort();
        gone.dedup();
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
        self.release_gone_panes(&status.get);
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
        Some(BrowserActor::Pane {
            pane_id: pane.pane_id,
            tab_id: pane.tab_id,
            workspace_id: pane.workspace_id,
            tab_label: tab.label,
            workspace_label: Some(workspace.label).filter(|label| !label.is_empty()),
            agent: pane.agent,
            session: crate::session::active_name().unwrap_or_else(|| "default".into()),
            gone: false,
        })
    }
}
