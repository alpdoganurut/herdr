//! Agent context use on the client (fork): how full each pane agent's
//! context window is, as the endpoint's last `endpoint.agent-context.v1`
//! push listed it (server side: src/server/headless/agent_context.rs). A
//! state is used only while its `boot_id` is the active snapshot's; a push
//! marks the sidebar model dirty, which folds the values into
//! `TabFacts::context` at its next build (never per frame).

use std::collections::HashMap;

use super::*;
use crate::agent_context::ContextUsage;
use crate::server::headless::agent_context::AgentContextPayload;

/// From this percent the tab row shows the use (yellow).
pub(crate) const ROW_WARN_PERCENT: u8 = 75;
/// From this percent the tab row's use turns red.
pub(crate) const ROW_ALERT_PERCENT: u8 = 90;

/// One endpoint's context use, as its last push listed it.
#[derive(Debug, Default, Clone)]
pub(crate) struct ClientAgentContextState {
    pub(crate) boot_id: String,
    pub(crate) revision: u64,
    /// Public pane id → context use.
    pub(crate) panes: HashMap<String, ContextUsage>,
}

impl ClientAgentContextState {
    pub(crate) fn from_payload(payload: AgentContextPayload) -> Self {
        Self {
            boot_id: payload.boot_id,
            revision: payload.revision,
            panes: payload
                .panes
                .into_iter()
                .filter(|pane| pane.window > 0)
                .map(|pane| {
                    (
                        pane.pane_id,
                        ContextUsage {
                            used: pane.used,
                            window: pane.window,
                        },
                    )
                })
                .collect(),
        }
    }

    pub(crate) fn usage_of(&self, pane_id: &str) -> Option<ContextUsage> {
        self.panes.get(pane_id).copied()
    }
}

/// The fuller of two uses (by percent, then by tokens in use).
pub(crate) fn fuller(current: Option<ContextUsage>, next: ContextUsage) -> ContextUsage {
    match current {
        Some(current) if (current.percent(), current.used) >= (next.percent(), next.used) => {
            current
        }
        _ => next,
    }
}

/// The active endpoint's context use, while it belongs to the active
/// snapshot's server.
pub(crate) fn active_agent_context_of<'a>(
    context: &'a HashMap<ClientEndpointId, ClientAgentContextState>,
    endpoint_id: &ClientEndpointId,
    snapshot: Option<&ClientShellSnapshot>,
) -> Option<&'a ClientAgentContextState> {
    let snapshot = snapshot?;
    context
        .get(endpoint_id)
        .filter(|context| context.boot_id == snapshot.boot_id)
}

/// The tab row's `NN%` text for a use at or over `ROW_WARN_PERCENT`
/// (`None` below), from a static table (no allocation per frame).
pub(crate) fn row_percent_text(usage: ContextUsage) -> Option<&'static str> {
    const TEXT: [&str; 26] = [
        "75%", "76%", "77%", "78%", "79%", "80%", "81%", "82%", "83%", "84%", "85%", "86%", "87%",
        "88%", "89%", "90%", "91%", "92%", "93%", "94%", "95%", "96%", "97%", "98%", "99%", "100%",
    ];
    let percent = usage.percent();
    percent
        .checked_sub(ROW_WARN_PERCENT)
        .and_then(|index| TEXT.get(usize::from(index)).copied())
}

/// The tab row's color for a use: yellow (dimmed) below `ROW_ALERT_PERCENT`, red from it.
pub(crate) fn row_percent_color(usage: ContextUsage, palette: &Palette) -> ratatui::style::Color {
    if usage.percent() >= ROW_ALERT_PERCENT {
        palette.red
    } else {
        palette.yellow
    }
}

impl ClientShellState {
    /// An `endpoint.agent-context.v1` push. Returns whether to repaint. An
    /// older or equal revision of the same server is ignored; a new server
    /// replaces the state.
    pub(crate) fn receive_agent_context(
        &mut self,
        endpoint_id: &ClientEndpointId,
        payload: AgentContextPayload,
    ) -> bool {
        if self.agent_context.get(endpoint_id).is_some_and(|current| {
            current.boot_id == payload.boot_id && current.revision >= payload.revision
        }) {
            return false;
        }
        self.agent_context.insert(
            endpoint_id.clone(),
            ClientAgentContextState::from_payload(payload),
        );
        self.sidebar_model.mark_dirty();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_text_starts_at_75_and_caps_at_100() {
        let usage = |used| ContextUsage {
            used,
            window: 200_000,
        };
        assert_eq!(row_percent_text(usage(120_000)), None, "60%");
        assert_eq!(row_percent_text(usage(149_999)), None, "74%");
        assert_eq!(row_percent_text(usage(150_000)), Some("75%"));
        assert_eq!(row_percent_text(usage(164_000)), Some("82%"));
        assert_eq!(row_percent_text(usage(186_000)), Some("93%"));
        assert_eq!(row_percent_text(usage(400_000)), Some("100%"));
        let palette = Palette::catppuccin();
        assert_eq!(row_percent_color(usage(164_000), &palette), palette.yellow);
        assert_eq!(row_percent_color(usage(180_000), &palette), palette.red);
    }

    #[test]
    fn the_fuller_use_wins() {
        let a = ContextUsage {
            used: 100,
            window: 200,
        };
        let b = ContextUsage {
            used: 900,
            window: 1_000,
        };
        assert_eq!(fuller(None, a), a);
        assert_eq!(fuller(Some(a), b), b);
        assert_eq!(fuller(Some(b), a), b);
    }
}
