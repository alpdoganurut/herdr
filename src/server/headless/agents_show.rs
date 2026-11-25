//! Fork (agents show): `agents.show` moves the user's view to a tab. The
//! App decides (src/app/agents_show.rs, policy `Action::Show`: only in the
//! caller's user's turn); the server then moves only the foreground client
//! shell, the one the user works in, by running the target's `pane.focus`
//! (or `tab.focus`) as that client's own request. So it is the same move as
//! the user's click: that client's location, the default target, focus
//! events, tab geometry, and the visit that clears the tab's agent cards.
//! Every other attached client stays where it is.
//!
//! Machine tabs: a client that shows another machine has its surface here
//! inactive, so it is no window to move (`no_client`): an agent of machine B
//! never takes the view off machine A.

use super::HeadlessServer;
use crate::api;
use crate::api::schema::agents_model::{error_code, AgentsShowParams};
use crate::app::agents_model::{ModelError, ModelResult};
use crate::app::App;

impl HeadlessServer {
    /// `agents.show`: the encoded reply.
    pub(super) fn handle_agents_show_api(
        &mut self,
        id: String,
        params: &AgentsShowParams,
    ) -> String {
        let request = match self.app.check_agents_show(params) {
            Ok(request) => request,
            Err(err) => return App::model_reply(id, Err(err)),
        };
        let Some(client_id) = self.show_client_id() else {
            let result = self
                .app
                .finish_agents_show(request, Err(crate::app::agents_show::no_client()));
            return App::model_reply(
                id,
                result.map(|tab| api::schema::ResponseResult::TabInfo { tab }),
            );
        };
        // The user is typing in another pane of that window: moving it would
        // take the keys away mid-word. Their own "show me X" in the caller's
        // pane is the request itself, not typing to protect.
        if let Some(focus) = self.shell_focus_target(client_id) {
            let own = request.caller_pane() == Some((focus.workspace_index, focus.pane_id));
            if !own
                && self
                    .app
                    .pane_user_typing(focus.workspace_index, focus.pane_id)
            {
                let err = self
                    .app
                    .deny_agents_show(request, crate::app::agents_show::user_typing());
                return App::model_reply(id, Err(err));
            }
        }
        let shown = self.navigate_client(client_id, request.navigation.clone());
        let result = self.app.finish_agents_show(request, shown);
        App::model_reply(
            id,
            result.map(|tab| api::schema::ResponseResult::TabInfo { tab }),
        )
    }

    /// The client shell the user works in: the foreground client, only
    /// while it is an active client shell (no other window is moved).
    fn show_client_id(&self) -> Option<u64> {
        let Some(client_id) = self.foreground_client_id else {
            tracing::debug!("agents.show: no foreground client");
            return None;
        };
        let Some(client) = self.clients.get(&client_id) else {
            tracing::debug!(client_id, "agents.show: the foreground client is gone");
            return None;
        };
        if !client.is_active_shell_client() {
            tracing::debug!(
                client_id,
                "agents.show: the foreground client is not an active client shell"
            );
            return None;
        }
        if client.shell_location.is_none() {
            tracing::debug!(
                client_id,
                "agents.show: the foreground client shell has no location yet"
            );
            return None;
        }
        Some(client_id)
    }

    /// Run `method` as client `client_id`'s own navigation request.
    fn navigate_client(&mut self, client_id: u64, method: api::schema::Method) -> ModelResult<()> {
        let (respond_to, response_rx) = std::sync::mpsc::channel();
        self.handle_client_shell_api_request(
            client_id,
            api::ApiRequestMessage {
                request: api::schema::Request {
                    id: "agents-show".into(),
                    method,
                },
                respond_to,
                response_write_complete: None,
                peer_pid: None,
            },
        );
        let response = response_rx
            .try_recv()
            .map_err(|_| ModelError::new(error_code::FAILED, "the client move did not answer"))?;
        let value: serde_json::Value = serde_json::from_str(&response)
            .map_err(|err| ModelError::new(error_code::FAILED, err.to_string()))?;
        match value.get("error").filter(|error| !error.is_null()) {
            None => Ok(()),
            Some(error) => Err(ModelError::new(
                error["code"].as_str().unwrap_or(error_code::FAILED),
                error["message"]
                    .as_str()
                    .unwrap_or("the client move failed"),
            )),
        }
    }
}
