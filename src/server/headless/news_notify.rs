//! News notifications (fork): delivery of what the news desk queued.
//!
//! The app queues ([`crate::app::news`]: one per run, the text sanitized,
//! quiet hours deferred to their end, a failure alert once per streak); the
//! server delivers, since it owns the clients, and applies the daily policy
//! here, at delivery (two run notifications per local day, a high-urgency
//! one past the cap once a day), so a queued notification that was replaced
//! before it went out never used a slot. Delivery goes the way of
//! `notification.show` ([`super::notifications`]'s
//! `handle_notification_show_api`): a `SemanticNotification` of kind
//! `Custom` to every client shell, only while a client shell is connected,
//! under the same one-per-second rate limit. Without a client shell the
//! notification stays queued in `news.json` and goes out when one attaches
//! (the flush at the end of the client-connected block). The card carries
//! the News pane, so clicking it focuses the tab.

use super::*;
use crate::app::news::{notify_decision, NotifyDecision, PENDING_RUN};

impl HeadlessServer {
    /// Deliver the first due news notification, if any. Called on every
    /// scheduler pass and when a client shell attaches. Returns whether one
    /// was delivered.
    pub(super) fn flush_news_notifications(&mut self, now: Instant) -> bool {
        let now_unix = crate::app::news::unix_now();
        // A retry deadline is only for a client that is there: without one
        // (or once past) it would wake the loop for nothing.
        self.app.news.notify_retry_at = None;
        if !self.clients.values().any(ClientConnection::is_shell_client) {
            return false;
        }
        loop {
            let Some(index) = self.app.news.due_notification_index(now_unix) else {
                return false;
            };
            if self.app.api_notification_rate_limited(now) {
                self.app.news.notify_retry_at = Some(now + Duration::from_secs(1));
                return false;
            }
            let Some(pending) = self.app.news.notify.pending.get(index).cloned() else {
                return false;
            };
            // The daily cap, charged only when the notification goes out.
            let mut ledger = self.app.news.notify.clone();
            if pending.kind == PENDING_RUN {
                let (_, day) = self.app.news.local_now();
                if let NotifyDecision::Drop(reason) =
                    notify_decision(&mut ledger, &day, pending.high)
                {
                    tracing::info!(
                        event = "news.notify",
                        outcome = "dropped",
                        reason,
                        title = %pending.title,
                        "news notification dropped by policy"
                    );
                    self.app.news.notify.pending.remove(index);
                    self.app.news.persist();
                    continue;
                }
            }
            let target = self.app.news_notification_target();
            let shown = self.send_to_client_shells(ServerMessage::SemanticNotification(
                protocol::SemanticNotification {
                    kind: protocol::SemanticNotificationKind::Custom,
                    title: pending.title.clone(),
                    body: pending.body.clone(),
                    sound: pending
                        .high
                        .then_some(protocol::SemanticNotificationSound::Done),
                    agent: None,
                    workspace_id: target.as_ref().map(|target| target.0.clone()),
                    tab_id: target.as_ref().map(|target| target.1.clone()),
                    pane_id: target.map(|target| target.2),
                    position: None,
                },
            ));
            if !shown {
                return false;
            }
            self.app.mark_api_notification_shown(now);
            ledger.pending.remove(index);
            self.app.news.notify = ledger;
            self.app.news.persist();
            tracing::info!(
                event = "news.notify",
                outcome = "delivered",
                kind = %pending.kind,
                title = %pending.title,
                "news notification delivered"
            );
            return true;
        }
    }
}
