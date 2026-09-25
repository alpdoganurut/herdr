//! Agent notification text for the `tabs` sidebar layout.
//!
//! The server phrases an agent notification as "<agent> finished" with the
//! body "<space> · <space number> · <tab>". In the `tabs` layout the tab is
//! what the user knows, so the client rewrites it once, when the notification
//! leaves the pending list: "<tab> finished" / "<tab> needs attention", body
//! "<agent> · <directory>". Every delivery path (in-app card, mobile banner,
//! terminal and system notifications) then shows the same text. The `spaces`
//! layout keeps the server's text. Idle reminders build the same body with
//! `agent_notification_body`.

use super::*;

const FINISHED_SUFFIX: &str = " finished";
const NEEDS_ATTENTION_SUFFIX: &str = " needs attention";

/// Last path component of `path`, ignoring trailing separators.
fn directory_name(path: &str) -> Option<&str> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
}

/// "<agent> · <directory>" for `pane_id` (directory: the basename of the
/// pane's cwd, else the label of the tab's space); missing parts are left out.
pub(super) fn agent_notification_body(
    snapshot: &ClientShellSnapshot,
    agent: Option<&str>,
    pane_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Option<String> {
    let pane =
        pane_id.and_then(|pane_id| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id));
    let directory = pane
        .and_then(|pane| pane.cwd.as_deref())
        .and_then(directory_name)
        .map(str::to_owned)
        .or_else(|| {
            let workspace_id = workspace_id.or(pane.map(|pane| pane.workspace_id.as_str()))?;
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| workspace.label.clone())
        });
    let parts: Vec<String> = [agent.map(str::to_owned), directory]
        .into_iter()
        .flatten()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

impl ClientShellState {
    /// Rewrite a server agent notification ("<agent> finished") into the
    /// `tabs` layout's form. Other kinds, other layouts, titles the server did
    /// not phrase that way (idle reminders, custom text) and tabs the endpoint
    /// snapshot cannot resolve are left as they are.
    pub(super) fn format_agent_notification(
        &self,
        endpoint_id: &ClientEndpointId,
        mut event: SemanticNotification,
    ) -> SemanticNotification {
        if self.config.sidebar_layout != crate::config::SidebarLayoutConfig::Tabs {
            return event;
        }
        let suffix = match event.kind {
            SemanticNotificationKind::Finished => FINISHED_SUFFIX,
            SemanticNotificationKind::NeedsAttention => NEEDS_ATTENTION_SUFFIX,
            SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => {
                return event;
            }
        };
        let Some(server_agent) = event.title.strip_suffix(suffix) else {
            return event;
        };
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return event;
        };
        let pane = event
            .pane_id
            .as_deref()
            .and_then(|pane_id| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id));
        let tab_id = event
            .tab_id
            .as_deref()
            .or(pane.map(|pane| pane.tab_id.as_str()));
        let Some(tab) =
            tab_id.and_then(|tab_id| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
        else {
            return event;
        };
        let agent = Some(server_agent.trim())
            .filter(|agent| !agent.is_empty())
            .map(str::to_owned)
            .or_else(|| event.agent.clone());
        let body = agent_notification_body(
            snapshot,
            agent.as_deref(),
            event.pane_id.as_deref(),
            Some(tab.workspace_id.as_str()),
        );
        event.title = format!("{}{suffix}", tab.label);
        event.body = body;
        event
    }
}
