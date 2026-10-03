//! Fork: "Copy session ID" in the tab and pane context menus.
//!
//! The client snapshot carries no agent session id, so opening a tab or pane
//! menu on an agent sends one `pane.get` for that agent's pane; the reply's
//! `agent_session` (an id-kind ref: a Claude session uuid, a Codex thread id)
//! fills the open menu's target and the item appears. Errors, a server without
//! `pane.get`, or a menu closed before the reply leave the menu unchanged and
//! raise no notice.

use super::*;

impl ClientShellState {
    /// Called right after a tab or pane context menu opened: asks the server
    /// for the menu's agent pane, if it has one.
    pub(super) fn request_context_menu_session(&mut self, outcome: &mut ClientShellInput) {
        let Some(pane_id) = self.context_menu_session_pane() else {
            return;
        };
        let method = crate::api::schema::Method::PaneGet(crate::api::schema::PaneTarget {
            pane_id: pane_id.clone(),
        });
        // A background lookup: skip silently where push would raise a notice.
        if !self.endpoint_is_online(&self.active_endpoint_id)
            || !self.supports_endpoint_method(&method)
        {
            return;
        }
        self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::ContextMenuSession { pane_id },
            outcome,
        );
    }

    /// The agent pane the open menu's session item would copy from: for a
    /// tab, its focused agent pane, else its first agent pane (the one the
    /// agent items act on); for a pane, the pane when it runs an agent.
    fn context_menu_session_pane(&self) -> Option<String> {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_ref() else {
            return None;
        };
        let snapshot = self.snapshot.as_deref()?;
        match &menu.target {
            ClientContextMenuTarget::Tab { tab_id, .. } => {
                let mut agents = snapshot
                    .agents
                    .iter()
                    .filter(|agent| &agent.tab_id == tab_id);
                let first = agents.clone().next()?;
                Some(
                    agents
                        .find(|agent| agent.focused)
                        .unwrap_or(first)
                        .pane_id
                        .clone(),
                )
            }
            ClientContextMenuTarget::Pane { pane_id, .. } => snapshot
                .agents
                .iter()
                .any(|agent| &agent.pane_id == pane_id)
                .then(|| pane_id.clone()),
            _ => None,
        }
    }

    /// The `pane.get` reply: fills the session id of the menu still open for
    /// that pane. Never a notice.
    pub(super) fn complete_context_menu_session(
        &mut self,
        pane_id: String,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let Ok(crate::api::schema::ResponseResult::PaneInfo { pane }) = result else {
            return (false, Vec::new());
        };
        if pane.pane_id != pane_id {
            return (false, Vec::new());
        }
        let Some(session_id) = pane
            .agent_session
            .filter(|session| session.kind == crate::agent_resume::AgentSessionRefKind::Id)
            .map(|session| session.value)
            .filter(|value| !value.is_empty())
        else {
            return (false, Vec::new());
        };
        if self.context_menu_session_pane().as_deref() != Some(pane_id.as_str()) {
            return (false, Vec::new());
        }
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        let slot = match &mut menu.target {
            ClientContextMenuTarget::Tab { session_id, .. }
            | ClientContextMenuTarget::Pane { session_id, .. } => session_id,
            _ => return (false, Vec::new()),
        };
        if slot.as_deref() == Some(session_id.as_str()) {
            return (false, Vec::new());
        }
        let had_item = slot.is_some();
        *slot = Some(session_id);
        if !had_item {
            // Keep the keyboard highlight on the item it was on.
            let index = menu
                .items()
                .iter()
                .position(|item| item.action == ClientContextMenuAction::CopySessionId);
            if index.is_some_and(|index| menu.highlighted >= index) {
                menu.highlighted += 1;
            }
        }
        (true, Vec::new())
    }

    /// The menu item: copies the id through the same clipboard path and toast
    /// as a selection copy, without focusing the tab.
    pub(super) fn copy_context_menu_session_id(
        &mut self,
        session_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.show_copy_feedback(std::time::Instant::now());
        outcome
            .actions
            .push(ClientShellAction::ClipboardWrite(session_id.into_bytes()));
    }
}
