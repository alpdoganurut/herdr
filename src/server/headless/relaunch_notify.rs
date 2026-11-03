//! Relaunch failure toasts (fork, restart screen): delivery of what the
//! app's restart scheduler queued.
//!
//! A restart or wake typed into a pane's shell whose agent never starts
//! (the shell is back at an available prompt, no agent detected) ends its
//! restarting / waking mark early, and the app queues one notice
//! ([`crate::app::RelaunchFailureNotice`]; none for a muted tab). The
//! server delivers it the way of `notification.show`: a
//! `SemanticNotification` of kind `Custom` to every client shell, carrying
//! the pane so a click focuses it. Without a client shell the notice is
//! dropped: the mark is runtime only and a late toast would be noise. Not
//! charged to the API notification rate limit; one per failed relaunch.

use super::*;

impl HeadlessServer {
    /// Send every queued relaunch failure toast. Called on every scheduler
    /// pass, directly after the restart scheduler. Returns how many went out.
    pub(super) fn flush_relaunch_failure_notices(&mut self) -> usize {
        if self.app.relaunch_failure_notices.is_empty() {
            return 0;
        }
        let notices = std::mem::take(&mut self.app.relaunch_failure_notices);
        let mut sent = 0;
        for notice in notices {
            let shown = self.send_to_client_shells(ServerMessage::SemanticNotification(
                protocol::SemanticNotification {
                    kind: protocol::SemanticNotificationKind::Custom,
                    title: notice.title.clone(),
                    body: notice.body,
                    sound: None,
                    agent: None,
                    workspace_id: Some(notice.workspace_id),
                    tab_id: notice.tab_id,
                    pane_id: notice.pane_id,
                    position: None,
                },
            ));
            if shown {
                sent += 1;
            } else {
                tracing::info!(
                    event = "agent.resume.undetected.notice",
                    outcome = "dropped",
                    title = %notice.title,
                    "no client shell for the relaunch failure toast"
                );
            }
        }
        sent
    }
}
