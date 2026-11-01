//! Autosuspend on the server loop (fork): the panes attached clients show,
//! for the scan in src/app/agent_autosuspend.rs. The server owns client
//! focus, so it passes every pane of each client's shown tab (a split tab
//! shows all of them); computed only when a scan is due (about once a
//! minute while `[agents.autosuspend]` is on), never per render.

use std::collections::HashSet;
use std::time::Instant;

use super::HeadlessServer;
use crate::terminal::TerminalId;

impl HeadlessServer {
    /// Run the autosuspend scan when it is due. Returns whether anything
    /// changed.
    pub(super) fn handle_agent_autosuspend(&mut self, now: Instant) -> bool {
        if !self.app.agent_autosuspend_due(now) {
            return false;
        }
        // Off (only a quiet wake retries): no shown panes needed.
        let shown = if self.app.agent_autosuspend_needs_shown() {
            self.autosuspend_shown_terminals()
        } else {
            HashSet::new()
        };
        self.app.run_agent_autosuspend(now, &shown)
    }

    /// The terminals of every pane in a tab some attached client shows.
    pub(super) fn autosuspend_shown_terminals(&self) -> HashSet<TerminalId> {
        let mut shown = HashSet::new();
        for &client_id in self.clients.keys() {
            let Some(target) = self.shell_target_for_client(client_id) else {
                continue;
            };
            let Some(tab) = self
                .app
                .state
                .workspaces
                .get(target.workspace_index)
                .and_then(|workspace| workspace.tabs.get(target.tab_index))
            else {
                continue;
            };
            shown.extend(
                tab.panes
                    .values()
                    .map(|pane| pane.attached_terminal_id.clone()),
            );
        }
        shown
    }
}
