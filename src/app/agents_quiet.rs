//! Mute and do-not-disturb for agents (fork, agents model):
//! `agents.set_quiet`. One permission check (src/agents_model/policy.rs:
//! `SoftEdit::Quiet`, free inside the caller's own team like suspend, the
//! coordinator in its user's turn, never the coordinator's tab), then the
//! existing `tab.set_muted` / `tab.set_dnd` through [`App::call_method`]
//! (so the mutes push, the DND quiet store and its release behave exactly
//! as from the tab menu), and one action-log line naming who did it.

use super::agents_model::{ModelError, ModelResult};
use super::App;
use crate::agents_model::policy::{self, Action};
use crate::api::schema::agents_model::{error_code, AgentsActionOutcome, AgentsSetQuietParams};
use crate::api::schema::{Method, ResponseResult, TabInfo, TabSetDndParams, TabSetMutedParams};

/// The action-log detail of a `agents.set_quiet` request.
fn quiet_detail(muted: Option<bool>, dnd: Option<bool>) -> String {
    [
        muted.map(|muted| if muted { "muted" } else { "unmuted" }),
        dnd.map(|dnd| if dnd { "dnd on" } else { "dnd off" }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

impl App {
    pub(super) fn handle_agents_set_quiet(
        &mut self,
        id: String,
        params: AgentsSetQuietParams,
    ) -> String {
        let result = self.agents_set_quiet(&params);
        Self::model_reply(id, result.map(|tab| ResponseResult::TabInfo { tab }))
    }

    fn agents_set_quiet(&mut self, params: &AgentsSetQuietParams) -> ModelResult<TabInfo> {
        if params.muted.is_none() && params.dnd.is_none() {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "pass muted, dnd or both",
            ));
        }
        let caller = self.model_caller(params.caller_pane.as_deref())?;
        let target = self.resolve_model_tab(&params.target)?;
        let mut line = self.tab_line("set_quiet", target);
        line.detail = Some(quiet_detail(params.muted, params.dnd));
        if let Some(caller) = &caller {
            self.authorize_on_tab(
                caller,
                target,
                Action::SoftEdit(policy::SoftEdit::Quiet),
                &line,
            )?;
            self.take_soft_edit(caller, &line)?;
        }
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        let mut methods = Vec::new();
        if let Some(muted) = params.muted {
            methods.push(Method::TabSetMuted(TabSetMutedParams {
                tab_id: tab_id.clone(),
                muted,
            }));
        }
        if let Some(dnd) = params.dnd {
            methods.push(Method::TabSetDnd(TabSetDndParams {
                tab_id: tab_id.clone(),
                dnd,
            }));
        }
        let mut tab = None;
        for method in methods {
            match self.call_method(method) {
                Ok(ResponseResult::TabInfo { tab: info }) => tab = Some(info),
                Ok(_) => {}
                Err(err) => {
                    line.code = Some(err.code.clone());
                    self.log_model_action(caller.as_ref(), AgentsActionOutcome::Failed, line);
                    return Err(err);
                }
            }
        }
        self.log_model_action(caller.as_ref(), AgentsActionOutcome::Ok, line);
        tab.ok_or_else(|| ModelError::new(error_code::FAILED, "no tab info"))
    }
}

#[cfg(test)]
mod tests;
