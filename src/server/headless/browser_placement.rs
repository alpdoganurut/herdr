//! Fork (companion launcher): the attached clients' browser placements.
//!
//! `browser.placement.set` arrives on a client shell's endpoint lane and is
//! answered here at once: stored on the process's placement board
//! (`crate::browser::placements`) with this server's facts about the
//! client (its terminal has the user's focus; it is the foreground
//! client). It never takes the command lane, never promotes the client to
//! the foreground and never reaches the App. The facts follow the client
//! (focus and foreground changes) and the placement leaves with it.

use super::*;

impl HeadlessServer {
    /// `browser.placement.set` from client `client_id`: stored, answered `Ok`.
    pub(super) fn handle_client_shell_browser_placement(
        &mut self,
        client_id: u64,
        boot_id: String,
        request_id: String,
        params: api::schema::BrowserPlacementSetParams,
    ) {
        let facts = self.browser_placement_facts(client_id);
        crate::browser::placements::board().set(
            client_id,
            params.placement,
            facts,
            crate::browser::unix_now(),
        );
        self.send_to_client(
            client_id,
            crate::server::client_commands::success_message_with_result(
                boot_id,
                request_id,
                api::schema::ResponseResult::Ok {},
            ),
        );
    }

    fn browser_placement_facts(&self, client_id: u64) -> crate::browser::placements::ClientFacts {
        let client = self.clients.get(&client_id);
        crate::browser::placements::ClientFacts {
            focused: client.is_some_and(|client| client.outer_terminal_focus == Some(true)),
            foreground: self.foreground_client_id == Some(client_id),
            surface_active: client.is_some_and(|client| client.shell_surface_active),
        }
    }

    /// A client's focus, its surface or the foreground client changed:
    /// refresh the facts of the clients that have a placement (none,
    /// usually).
    pub(super) fn sync_browser_placement_facts(&self) {
        let board = crate::browser::placements::board();
        let ids = board.client_ids();
        if ids.is_empty() {
            return;
        }
        let facts: Vec<_> = ids
            .into_iter()
            .map(|client_id| (client_id, self.browser_placement_facts(client_id)))
            .collect();
        board.set_facts(&facts);
    }

    /// A client left: its placement goes with it.
    pub(super) fn drop_browser_placement(&self, client_id: u64) {
        crate::browser::placements::board().remove(client_id);
    }
}
