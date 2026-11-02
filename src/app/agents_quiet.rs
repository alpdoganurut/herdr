//! Mute and do-not-disturb for agents (fork, agents model):
//! `agents.set_quiet`. One permission check (src/agents_model/policy.rs:
//! `SoftEdit::Quiet`, free inside the caller's own team like suspend, the
//! coordinator in its user's turn, never the coordinator's tab), then the
//! existing `tab.set_muted` through [`App::call_method`] and `tab.set_dnd`'s
//! own [`App::set_tab_dnd`] (so the mutes push, the DND quiet store and its
//! release behave exactly as from the tab menu; a do-not-disturb an agent
//! or the coordinator turns on is recorded as theirs, `Tab.dnd_by_agent`,
//! and lets the coordinator through), and one action-log line naming who
//! did it.

use super::agents_model::{ModelError, ModelResult};
use super::App;
use crate::agents_model::policy::{self, Action, Actor};
use crate::api::schema::agents_model::{error_code, AgentsActionOutcome, AgentsSetQuietParams};
use crate::api::schema::{Method, ResponseResult, TabInfo, TabSetMutedParams};

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
        let mut tab = None;
        if let Some(muted) = params.muted {
            match self.call_method(Method::TabSetMuted(TabSetMutedParams {
                tab_id: tab_id.clone(),
                muted,
            })) {
                Ok(ResponseResult::TabInfo { tab: info }) => tab = Some(info),
                Ok(_) => {}
                Err(err) => {
                    line.code = Some(err.code.clone());
                    self.log_model_action(caller.as_ref(), AgentsActionOutcome::Failed, line);
                    return Err(err);
                }
            }
        }
        // Fork: `tab.set_dnd`'s own path, recording that an agent (or the
        // coordinator) turned it on, so the coordinator gets through it; a
        // caller acting as the user (a shell pane) is the user.
        let by_agent = caller
            .as_ref()
            .is_some_and(|caller| !matches!(caller.actor, Actor::User));
        if let Some(dnd) = params.dnd {
            if !self.set_tab_dnd(target.ws_idx, target.tab_idx, dnd, by_agent) {
                line.code = Some(error_code::NOT_FOUND.to_string());
                self.log_model_action(caller.as_ref(), AgentsActionOutcome::Failed, line);
                return Err(ModelError::new(error_code::NOT_FOUND, "tab not found"));
            }
            tab = self.tab_info(target.ws_idx, target.tab_idx);
        }
        self.log_model_action(caller.as_ref(), AgentsActionOutcome::Ok, line);
        tab.ok_or_else(|| ModelError::new(error_code::FAILED, "no tab info"))
    }
}

#[cfg(test)]
mod tests;
