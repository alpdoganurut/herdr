//! Coordinator notifications (fork): delivery of what the coordinator queued.
//!
//! The app queues ([`crate::app::coordinator`]): new board suggestions
//! batched into one `suggestions` notification with sanitized text, and one
//! `down` / `blocked` / `locked` alert per streak, cleared when the
//! coordinator recovers. The server delivers, since it owns the clients, and
//! applies the delivery policy here (the `[coordinator]` `notify` switch,
//! `notify_daily_cap` for suggestions, `quiet_hours`), so a queued
//! notification that was replaced before it went out never used a slot.
//! The alerts are once per streak already and are not capped: a dropped
//! one would not come back while its condition lasts. The policy is the pure
//! [`delivery_decision`]; the flush below only feeds it and acts on it.
//!
//! Delivery goes the way of news ([`super::news_notify`]) and
//! `notification.show`: a `SemanticNotification` of kind `Custom` to every
//! client shell, only while a client shell is connected, under the same
//! one-per-second rate limit. Without a client shell the notification stays
//! queued in `coordinator.json` and goes out when one attaches (the flush at
//! the end of the client-connected block). The card carries the coordinator
//! pane, so clicking it focuses the coordinator tab.
//!
//! These replace the generic Finished / NeedsAttention toasts, which the
//! server suppresses for the coordinator's own terminal
//! (the `AppState::coordinator_terminal_id` check in the `suppress_completion` arm).
//!
//! App-side interface this file relies on (T1, mirroring News):
//! - `App.coordinator.notify: CoordinatorNotifyCfg` with `enabled: bool`,
//!   `daily_cap: u32` and `quiet: Option<QuietHours>` (the config keys);
//! - `App.coordinator.notify_ledger` (persisted in `coordinator.json`):
//!   `pending: Vec<PendingCoordinatorNotify>` with `kind`, `title`,
//!   `body: Option<String>`, `deliver_after: u64` (unix s), plus the daily
//!   ledger `day: String` and `delivered: u32` charged here;
//! - `App.coordinator.notify_retry_at: Option<Instant>`, folded into
//!   `next_coordinator_deadline` with the earliest `deliver_after`;
//! - `App.coordinator.local_now() -> (Option<u16>, String)` (local minute of
//!   day and `YYYY-MM-DD`, overridable in tests),
//!   `App.coordinator.due_notification_index(now_unix)` (first pending with
//!   `deliver_after <= now_unix`) and `App.coordinator.persist()`;
//! - `App::coordinator_notification_target() -> Option<(workspace, tab,
//!   pane)>` (public ids), like `news_notification_target`.

use super::*;
use crate::config::QuietHours;

// The notification kinds are queued by `app::coordinator`:
// suggestions (batched), down (relaunch cap, start failure, name taken),
// blocked (the coordinator's own NeedsAttention) and locked (another server
// holds the coordinator lock).
#[cfg(test)]
use crate::app::coordinator::KIND_LOCKED;
use crate::app::coordinator::{KIND_BLOCKED, KIND_DOWN, KIND_SUGGESTIONS};

/// The `[coordinator]` keys that govern delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotifyPolicy {
    /// `notify`: off drops everything queued.
    pub(crate) enabled: bool,
    /// `notify_daily_cap`.
    pub(crate) daily_cap: u32,
    /// `quiet_hours`, parsed with `config::parse_quiet_hours`.
    pub(crate) quiet: Option<QuietHours>,
}

/// What to do with the first due notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Deliver,
    /// Inside quiet hours: wait this many minutes, until the window ends.
    Hold(u16),
    Drop(&'static str),
}

/// Whether `kind` is the one alert that ignores quiet hours and the daily
/// cap: a down coordinator needs the user, and it is raised once per streak.
fn urgent(kind: &str) -> bool {
    kind == KIND_DOWN
}

/// Notifications already delivered on `today`, given the ledger's day and
/// count (a ledger from another day counts as none).
pub(crate) fn delivered_on(ledger_day: &str, delivered: u32, today: &str) -> u32 {
    if ledger_day == today {
        delivered
    } else {
        0
    }
}

/// Charge one delivery on `today` to the ledger.
pub(crate) fn charge(ledger_day: &mut String, delivered: &mut u32, today: &str) {
    if ledger_day != today {
        today.clone_into(ledger_day);
        *delivered = 0;
    }
    *delivered = delivered.saturating_add(1);
}

/// The delivery policy for one due notification of `kind`, at local minute
/// `local_minute` (None when the local clock is unknown: quiet hours cannot
/// apply), with `delivered_today` already out.
pub(crate) fn delivery_decision(
    policy: &NotifyPolicy,
    kind: &str,
    local_minute: Option<u16>,
    delivered_today: u32,
) -> Delivery {
    if !policy.enabled {
        return Delivery::Drop("notifications off");
    }
    if urgent(kind) {
        return Delivery::Deliver;
    }
    if let (Some(quiet), Some(minute)) = (policy.quiet, local_minute) {
        if quiet.contains(minute) {
            return Delivery::Hold(quiet.minutes_until_end(minute).max(1));
        }
    }
    if capped(kind) && delivered_today >= policy.daily_cap {
        return Delivery::Drop("daily cap");
    }
    Delivery::Deliver
}

/// Whether `kind` counts against (and is charged to) `notify_daily_cap`:
/// suggestions only.
pub(crate) fn capped(kind: &str) -> bool {
    kind == KIND_SUGGESTIONS
}

/// The sound a delivered notification carries: the alerts ring, suggestions
/// stay silent (they are batched and capped, never urgent).
fn sound_for(kind: &str) -> Option<protocol::SemanticNotificationSound> {
    match kind {
        KIND_DOWN | KIND_BLOCKED => Some(protocol::SemanticNotificationSound::Request),
        _ => None,
    }
}

impl HeadlessServer {
    /// Deliver the first due coordinator notification, if any. Called on
    /// every scheduler pass and when a client shell attaches. Returns
    /// whether one was delivered.
    pub(super) fn flush_coordinator_notifications(&mut self, now: Instant) -> bool {
        let now_unix = crate::app::news::unix_now();
        // A retry deadline is only for a client that is there: without one
        // (or once past) it would wake the loop for nothing.
        self.app.coordinator.notify_retry_at = None;
        if !self.clients.values().any(ClientConnection::is_shell_client) {
            return false;
        }
        loop {
            let Some(index) = self.app.coordinator.due_notification_index(now_unix) else {
                return false;
            };
            if self.app.api_notification_rate_limited(now) {
                self.app.coordinator.notify_retry_at = Some(now + Duration::from_secs(1));
                return false;
            }
            let Some(pending) = self
                .app
                .coordinator
                .notify_ledger
                .pending
                .get(index)
                .cloned()
            else {
                return false;
            };
            let cfg = &self.app.coordinator.notify;
            let policy = NotifyPolicy {
                enabled: cfg.enabled,
                daily_cap: cfg.daily_cap,
                quiet: cfg.quiet,
            };
            let (local_minute, today) = self.app.coordinator.local_now();
            let ledger = &self.app.coordinator.notify_ledger;
            let delivered_today = delivered_on(&ledger.day, ledger.delivered, &today);
            match delivery_decision(&policy, &pending.kind, local_minute, delivered_today) {
                Delivery::Deliver => {}
                Delivery::Hold(minutes) => {
                    let until = now_unix + u64::from(minutes) * 60;
                    if let Some(queued) = self.app.coordinator.notify_ledger.pending.get_mut(index)
                    {
                        queued.deliver_after = until;
                    }
                    self.app.coordinator.persist();
                    tracing::info!(
                        event = "coordinator.notify",
                        outcome = "held",
                        kind = %pending.kind,
                        until,
                        "coordinator notification held for quiet hours"
                    );
                    // Another queued one (a down alert) may still be due.
                    continue;
                }
                Delivery::Drop(reason) => {
                    tracing::info!(
                        event = "coordinator.notify",
                        outcome = "dropped",
                        reason,
                        kind = %pending.kind,
                        title = %pending.title,
                        "coordinator notification dropped by policy"
                    );
                    self.app.coordinator.notify_ledger.pending.remove(index);
                    self.app.coordinator.persist();
                    continue;
                }
            }
            let target = self.app.coordinator_notification_target();
            let shown = self.send_to_client_shells(ServerMessage::SemanticNotification(
                protocol::SemanticNotification {
                    kind: protocol::SemanticNotificationKind::Custom,
                    title: pending.title.clone(),
                    body: pending.body.clone(),
                    sound: sound_for(&pending.kind),
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
            let ledger = &mut self.app.coordinator.notify_ledger;
            ledger.pending.remove(index);
            if capped(&pending.kind) {
                charge(&mut ledger.day, &mut ledger.delivered, &today);
            }
            self.app.coordinator.persist();
            tracing::info!(
                event = "coordinator.notify",
                outcome = "delivered",
                kind = %pending.kind,
                title = %pending.title,
                "coordinator notification delivered"
            );
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: &str = "2026-10-02";

    fn policy(quiet: &str) -> NotifyPolicy {
        NotifyPolicy {
            enabled: true,
            daily_cap: 2,
            quiet: crate::config::parse_quiet_hours(quiet).unwrap(),
        }
    }

    #[test]
    fn delivers_within_the_cap_and_drops_past_it() {
        let policy = policy("");
        assert_eq!(
            delivery_decision(&policy, KIND_SUGGESTIONS, Some(600), 0),
            Delivery::Deliver
        );
        assert_eq!(
            delivery_decision(&policy, KIND_BLOCKED, Some(600), 1),
            Delivery::Deliver
        );
        assert_eq!(
            delivery_decision(&policy, KIND_SUGGESTIONS, Some(600), 2),
            Delivery::Drop("daily cap")
        );
        for alert in [KIND_BLOCKED, KIND_LOCKED] {
            assert_eq!(
                delivery_decision(&policy, alert, Some(600), 2),
                Delivery::Deliver,
                "{alert}: an alert is once per streak, never capped"
            );
            assert!(!capped(alert), "{alert} is not charged");
        }
        assert!(capped(KIND_SUGGESTIONS));
        assert_eq!(
            delivery_decision(&policy, KIND_DOWN, Some(600), 9),
            Delivery::Deliver,
            "a down alert ignores the cap"
        );
    }

    #[test]
    fn quiet_hours_hold_everything_but_down() {
        let policy = policy("22:00-07:00");
        // 23:30: an hour and a half after the start, 7.5 hours to go.
        assert_eq!(
            delivery_decision(&policy, KIND_SUGGESTIONS, Some(23 * 60 + 30), 0),
            Delivery::Hold(450)
        );
        assert_eq!(
            delivery_decision(&policy, KIND_BLOCKED, Some(60), 0),
            Delivery::Hold(360)
        );
        assert_eq!(
            delivery_decision(&policy, KIND_DOWN, Some(60), 0),
            Delivery::Deliver
        );
        assert_eq!(
            delivery_decision(&policy, KIND_SUGGESTIONS, Some(12 * 60), 0),
            Delivery::Deliver,
            "outside the window"
        );
        assert_eq!(
            delivery_decision(&policy, KIND_SUGGESTIONS, None, 0),
            Delivery::Deliver,
            "an unknown local clock cannot be inside quiet hours"
        );
    }

    #[test]
    fn quiet_hours_hold_before_the_cap_drops() {
        // Held, not dropped: the cap is judged when the window ends, on
        // that day's count.
        let policy = policy("22:00-07:00");
        assert!(matches!(
            delivery_decision(&policy, KIND_SUGGESTIONS, Some(23 * 60), 5),
            Delivery::Hold(_)
        ));
    }

    #[test]
    fn notifications_off_drops_even_down() {
        let policy = NotifyPolicy {
            enabled: false,
            ..policy("")
        };
        for kind in [KIND_SUGGESTIONS, KIND_DOWN, KIND_BLOCKED, KIND_LOCKED] {
            assert_eq!(
                delivery_decision(&policy, kind, Some(600), 0),
                Delivery::Drop("notifications off"),
                "{kind}"
            );
        }
    }

    #[test]
    fn the_ledger_resets_on_a_new_day() {
        let mut day = String::new();
        let mut delivered = 0;
        assert_eq!(delivered_on(&day, delivered, DAY), 0);
        charge(&mut day, &mut delivered, DAY);
        charge(&mut day, &mut delivered, DAY);
        assert_eq!((day.as_str(), delivered), (DAY, 2));
        assert_eq!(delivered_on(&day, delivered, DAY), 2);
        assert_eq!(delivered_on(&day, delivered, "2026-10-03"), 0);
        charge(&mut day, &mut delivered, "2026-10-03");
        assert_eq!((day.as_str(), delivered), ("2026-10-03", 1));
    }

    #[test]
    fn alerts_ring_and_suggestions_stay_silent() {
        assert_eq!(sound_for(KIND_SUGGESTIONS), None);
        assert_eq!(sound_for(KIND_LOCKED), None);
        assert_eq!(
            sound_for(KIND_DOWN),
            Some(protocol::SemanticNotificationSound::Request)
        );
        assert_eq!(
            sound_for(KIND_BLOCKED),
            Some(protocol::SemanticNotificationSound::Request)
        );
    }
}
