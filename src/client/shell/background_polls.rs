//! Fork: the overlays' own pulls when they are cancelled (an endpoint switch
//! before the target's fence, a lost connection, a failed send). The user
//! asked for none of them: no notice, and the next tick that may send asks
//! again. The tick's news, browser and coordinator reads are machine tabs'
//! (`retry_cancelled_poll`).

use super::teams::TeamRequestKind;
use super::*;
use crate::api::schema::ResponseResult;

impl ClientShellState {
    /// Takes a cancelled overlay pull's result (the team overlay's
    /// `team.get`, the settings browser section's `browser.settings`): the
    /// state shown stays and the pull is due again. `None` for anything else
    /// (a user's action, an answer, another error). Never asks for a repaint
    /// or an action: pulls are cancelled on every tick until the target's
    /// fence opens.
    pub(super) fn retry_cancelled_overlay_poll(
        &mut self,
        kind: &PendingEndpointKind,
        result: &Result<ResponseResult, ClientShellEndpointError>,
    ) -> Option<(bool, Vec<ClientShellAction>)> {
        if !is_cancelled(result) {
            return None;
        }
        match kind {
            PendingEndpointKind::Team(TeamRequestKind::Get { workspace_id }) => {
                if let Some(ClientShellOverlay::TeamInfo(overlay)) = self.overlay.as_mut() {
                    if overlay.workspace_id == *workspace_id {
                        overlay.loading = false;
                        overlay.refresh_due = true;
                    }
                }
            }
            PendingEndpointKind::BrowserSettings => self.retry_browser_settings(),
            _ => return None,
        }
        Some((false, Vec::new()))
    }
}

/// herdr itself took the request back (`cancel_endpoint_request`).
pub(super) fn is_cancelled(result: &Result<ResponseResult, ClientShellEndpointError>) -> bool {
    result
        .as_ref()
        .is_err_and(|error| error.code.as_deref() == Some("endpoint_cancelled"))
}
