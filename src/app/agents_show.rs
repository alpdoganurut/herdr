//! Showing the user another agent's tab (fork, agents model):
//! `agents.show`. One permission check (src/agents_model/policy.rs:
//! `Action::Show`, only in the caller's user's turn, any tab), then the
//! headless server moves the foreground client shell there as that
//! client's own navigation (src/server/headless/agents_show.rs), and one
//! action-log line names who did it. The target resolves like every
//! agents_* target ([`App::resolve_model_tab`]: a pane id, a tab id, an
//! agent name or a tab label).

use super::agents_model::{LogLine, ModelCaller, ModelError, ModelResult};
use super::App;
use crate::agents_model::policy::Action;
use crate::api::schema::agents_model::{error_code, AgentsActionOutcome, AgentsShowParams};
use crate::api::schema::{Method, PaneTarget, ResponseResult, TabInfo, TabTarget};

/// An allowed `agents.show`, waiting for the server to move the client.
pub(crate) struct ShowRequest {
    caller: Option<ModelCaller>,
    line: LogLine,
    ws_idx: usize,
    tab_idx: usize,
    /// The navigation that shows the target, as a client sends it for its
    /// own move: `pane.focus` for a pane, `tab.focus` for a whole tab.
    pub(crate) navigation: Method,
}

/// No client shell to show the target in.
pub(crate) fn no_client() -> ModelError {
    ModelError::new(
        error_code::NO_CLIENT,
        "no herdr window is attached, so there is no view to move; tell your user where it is instead",
    )
}

impl App {
    /// The App lane alone (no headless server) has no client to move.
    pub(super) fn handle_agents_show(&mut self, id: String, params: AgentsShowParams) -> String {
        let result = self
            .check_agents_show(&params)
            .and_then(|request| self.finish_agents_show(request, Err(no_client())));
        Self::model_reply(id, result.map(|tab| ResponseResult::TabInfo { tab }))
    }

    /// Resolve the caller and the target and decide; a refusal is logged.
    pub(crate) fn check_agents_show(
        &mut self,
        params: &AgentsShowParams,
    ) -> ModelResult<ShowRequest> {
        let caller = self.model_caller(params.caller_pane.as_deref())?;
        let target = self.resolve_model_tab(&params.target)?;
        let line = self.tab_line("show", target);
        if let Some(caller) = &caller {
            self.authorize_on_tab(caller, target, Action::Show, &line)?;
        }
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        let navigation = match target
            .pane_id
            .and_then(|pane| self.public_pane_id(target.ws_idx, pane))
        {
            Some(pane_id) => Method::PaneFocus(PaneTarget { pane_id }),
            None => Method::TabFocus(TabTarget { tab_id }),
        };
        Ok(ShowRequest {
            caller,
            line,
            ws_idx: target.ws_idx,
            tab_idx: target.tab_idx,
            navigation,
        })
    }

    /// Log how the move went and answer the tab now shown.
    pub(crate) fn finish_agents_show(
        &mut self,
        request: ShowRequest,
        shown: ModelResult<()>,
    ) -> ModelResult<TabInfo> {
        let ShowRequest {
            caller,
            mut line,
            ws_idx,
            tab_idx,
            ..
        } = request;
        if let Err(err) = shown {
            line.code = Some(err.code.clone());
            line.detail = Some(err.message.clone());
            self.log_model_action(caller.as_ref(), AgentsActionOutcome::Failed, line);
            return Err(err);
        }
        self.log_model_action(caller.as_ref(), AgentsActionOutcome::Ok, line);
        self.tab_info(ws_idx, tab_idx)
            .ok_or_else(|| ModelError::new(error_code::FAILED, "no tab info"))
    }
}

#[cfg(test)]
mod tests;
