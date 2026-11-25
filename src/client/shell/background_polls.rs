//! Fork: the tick's own requests when they are cancelled (an endpoint switch
//! before the target's fence, a lost connection, a failed send). The user
//! asked for none of them: no notice, and the next tick that may send asks
//! again.

use super::coordinator::CoordinatorRequestKind;
use super::teams::TeamRequestKind;
use super::*;
use crate::api::schema::ResponseResult;

impl ClientShellState {
    /// Takes a cancelled background poll's result: the state shown stays and
    /// the poll is due again. `None` for anything else (a user's action, an
    /// answer, another error). Never asks for a repaint or an action: polls
    /// are cancelled on every tick until the target's fence opens.
    pub(super) fn retry_cancelled_poll(
        &mut self,
        kind: &PendingEndpointKind,
        result: &Result<ResponseResult, ClientShellEndpointError>,
    ) -> Option<(bool, Vec<ClientShellAction>)> {
        if !is_cancelled(result) {
            return None;
        }
        match kind {
            PendingEndpointKind::NewsGet => {
                self.news.loading = false;
                self.refresh_news();
            }
            PendingEndpointKind::BrowserGet => {
                self.browser.loading = false;
                self.refresh_browser();
            }
            PendingEndpointKind::Coordinator(CoordinatorRequestKind::Get) => {
                self.coordinator.get_not_sent();
                self.coordinator.refresh();
            }
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
