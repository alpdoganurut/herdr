//! Fork (agents show): `agents.show` moves the user's view to a tab. The
//! App decides (src/app/agents_show.rs, policy `Action::Show`: only in the
//! caller's user's turn); the server then moves only the foreground client
//! shell, the one the user works in, by running the target's `pane.focus`
//! (or `tab.focus`) as that client's own request. So it is the same move as
//! the user's click: that client's location, the default target, focus
//! events, tab geometry, and the visit that clears the tab's agent cards.
//! Every other attached client stays where it is.

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
        let shown = match self.show_client_id() {
            Some(client_id) => self.navigate_client(client_id, request.navigation.clone()),
            None => Err(crate::app::agents_show::no_client()),
        };
        let result = self.app.finish_agents_show(request, shown);
        App::model_reply(
            id,
            result.map(|tab| api::schema::ResponseResult::TabInfo { tab }),
        )
    }

    /// The client shell the user works in: the foreground client, only
    /// while it is an active client shell (no other window is moved).
    fn show_client_id(&self) -> Option<u64> {
        self.foreground_client_id.filter(|client_id| {
            self.clients.get(client_id).is_some_and(|client| {
                client.is_active_shell_client() && client.shell_location.is_some()
            })
        })
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
