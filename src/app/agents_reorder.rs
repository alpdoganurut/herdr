//! Reordering groups and tabs for agents (fork, agents model):
//! `agents.reorder_group` and `agents.reorder_tab`. One permission check
//! (src/agents_model/policy.rs: `ReorderGroup` is the user's whole sidebar,
//! so only the coordinator in its user's turn; `ReorderTab` is free inside
//! the caller's own team), then the upstream `workspace.move` / `tab.move`
//! through [`App::call_method`], and one action-log line.

use super::agents_model::{LogLine, ModelError, ModelResult};
use super::App;
use crate::agents_model::policy::{Action, Relation};
use crate::api::schema::agents_model::{
    error_code, AgentsActionOutcome, AgentsReorderGroupParams, AgentsReorderResult,
    AgentsReorderTabParams,
};
use crate::api::schema::{Method, ResponseResult};

/// Where `source` must be inserted (`move_workspace` / `move_tab`'s
/// `insert_idx`, counted before the removal) to end at index `target`.
pub(crate) fn insert_index_for(source: usize, target: usize) -> usize {
    if target > source {
        target + 1
    } else {
        target
    }
}

impl App {
    pub(super) fn handle_agents_reorder_group(
        &mut self,
        id: String,
        params: AgentsReorderGroupParams,
    ) -> String {
        let result = self.agents_reorder_group(&params);
        Self::model_reply(
            id,
            result.map(|reorder| ResponseResult::AgentsReorder { reorder }),
        )
    }

    pub(super) fn handle_agents_reorder_tab(
        &mut self,
        id: String,
        params: AgentsReorderTabParams,
    ) -> String {
        let result = self.agents_reorder_tab(&params);
        Self::model_reply(
            id,
            result.map(|reorder| ResponseResult::AgentsReorder { reorder }),
        )
    }

    /// A group (workspace index ≥ 1: the first space stays first).
    fn reorder_group_index(&self, group: &str) -> ModelResult<usize> {
        match self.resolve_group(group) {
            Some(0) => Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "the first space (the ungrouped tabs) stays first",
            )),
            Some(ws_idx) => Ok(ws_idx),
            None => Err(ModelError::new(
                error_code::NOT_FOUND,
                format!("no group {group:?} (a group id, label or number)"),
            )),
        }
    }

    fn agents_reorder_group(
        &mut self,
        params: &AgentsReorderGroupParams,
    ) -> ModelResult<AgentsReorderResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let source = self.reorder_group_index(&params.group)?;
        let groups = self.state.workspaces.len() - 1;
        // The final index among the workspaces (1..=groups).
        let target = match (params.position, &params.before, &params.after) {
            (Some(position), None, None) => (position.max(1) as usize).min(groups),
            (None, Some(other), None) | (None, None, Some(other)) => {
                let other_idx = self.reorder_group_index(other)?;
                if other_idx == source {
                    return Err(ModelError::new(
                        error_code::INVALID_PARAMS,
                        "before / after names the group itself",
                    ));
                }
                // Its index once the moved group is taken out.
                let other_after_removal = if other_idx > source {
                    other_idx - 1
                } else {
                    other_idx
                };
                if params.after.is_some() {
                    other_after_removal + 1
                } else {
                    other_after_removal
                }
            }
            _ => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "pass exactly one of position, before and after",
                ))
            }
        };
        let label = self.group_label(source);
        let workspace_id = self.public_workspace_id(source);
        let line = LogLine {
            action: "reorder_group",
            target_name: Some(label.clone()),
            detail: Some(format!("to position {target} of {groups}")),
            ..LogLine::default()
        };
        let facts = self.model_facts(&caller, label.clone(), None);
        self.model_authorize(
            &caller,
            Relation::Other,
            Action::ReorderGroup,
            &facts,
            &line,
        )?;
        let moved = target != source;
        if moved {
            self.call_method(Method::WorkspaceMove(
                crate::api::schema::WorkspaceMoveParams {
                    workspace_id: workspace_id.clone(),
                    insert_index: insert_index_for(source, target),
                },
            ))?;
        }
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(AgentsReorderResult {
            // The group's id follows its new place.
            id: self.public_workspace_id(target),
            label,
            position: target as u32,
            of: groups as u32,
            moved,
        })
    }

    fn agents_reorder_tab(
        &mut self,
        params: &AgentsReorderTabParams,
    ) -> ModelResult<AgentsReorderResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let tabs = self.state.workspaces[target.ws_idx].tabs.len();
        let position = (params.position.max(1) as usize).min(tabs);
        let index = position - 1;
        let mut line = self.tab_line("reorder_tab", target);
        line.detail = Some(format!(
            "to position {position} of {tabs} in {}",
            self.group_label(target.ws_idx)
        ));
        self.authorize_on_tab(&caller, target, Action::ReorderTab, &line)?;
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        let label = self.tab_label(target.ws_idx, target.tab_idx);
        let moved = index != target.tab_idx;
        if moved {
            self.take_soft_edit(&caller, &line)?;
            self.call_method(Method::TabMove(crate::api::schema::TabMoveParams {
                tab_id: tab_id.clone(),
                insert_index: insert_index_for(target.tab_idx, index),
            }))?;
        }
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        // The tab id follows its new place.
        let id = self.public_tab_id(target.ws_idx, index).unwrap_or(tab_id);
        Ok(AgentsReorderResult {
            id,
            label,
            position: position as u32,
            of: tabs as u32,
            moved,
        })
    }
}

#[cfg(test)]
mod tests;
