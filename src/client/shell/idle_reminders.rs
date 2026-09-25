//! Idle reminders for tabs marked "Remind me" (`tab.set_remind`).
//!
//! The mark lives on the tab server-side; the timing and the reminder itself
//! are client-only. For every marked tab whose aggregate agent status is
//! `Done` (finished, unseen) or `Blocked`, the client remembers when it first
//! saw that state. After `ui.idle_reminder_minutes` it raises a reminder, then
//! again every as many minutes. Focusing the tab, the status leaving
//! Done/Blocked (or the agent changing state), removing the mark, or the tab
//! disappearing starts over. A client restart starts every clock fresh.
//!
//! A reminder is a `SemanticNotification` for the tab's agent pane that goes
//! through the normal pending-notification path: it replaces that pane's
//! previous card (so the tab shows one card), honours `ui.toast.delivery`, and
//! carries the done / request sound unless sounds are off. The pass runs at the
//! top of `tick_notifications`, so the client loop's timer drives it.

use super::notification_policy::COMPLETION_EVIDENCE_GRACE;
use super::*;
use crate::api::schema::AgentStatus;

/// One marked tab waiting in Done/Blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientIdleReminder {
    status: AgentStatus,
    /// The waiting agent's `state_change_seq`: a new state episode (even back
    /// to the same status) restarts the clock.
    state_change_seq: u64,
    /// When this client first saw the tab waiting.
    since: std::time::Instant,
    /// The last reminder, or `since`; the next one is due an interval later.
    anchor: std::time::Instant,
}

/// A marked, waiting, unfocused tab seen in this pass.
struct WaitingTab {
    key: (ClientEndpointId, String),
    status: AgentStatus,
    state_change_seq: u64,
    label: String,
    event: SemanticNotification,
}

fn reminder_interval(minutes: u32) -> std::time::Duration {
    std::time::Duration::from_secs(u64::from(minutes) * 60)
}

/// `tab.set_remind` flipping the mark of `tab_id` (`keys.toggle_tab_remind`).
pub(super) fn tab_remind_toggle_method(
    snapshot: &ClientShellSnapshot,
    tab_id: &str,
) -> Option<crate::api::schema::Method> {
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
    Some(crate::api::schema::Method::TabSetRemind(
        crate::api::schema::TabSetRemindParams {
            tab_id: tab.tab_id.clone(),
            remind: !tab.remind,
        },
    ))
}

impl ClientShellState {
    /// Marked tabs that are waiting and unfocused, across every endpoint.
    fn waiting_marked_tabs(&self) -> Vec<WaitingTab> {
        let mut waiting = Vec::new();
        for endpoint in &self.endpoints {
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            for tab in snapshot.tabs.iter().filter(|tab| tab.remind) {
                let (kind, sound) = match tab.agent_status {
                    AgentStatus::Done => (
                        SemanticNotificationKind::Finished,
                        SemanticNotificationSound::Done,
                    ),
                    AgentStatus::Blocked => (
                        SemanticNotificationKind::NeedsAttention,
                        SemanticNotificationSound::Request,
                    ),
                    _ => continue,
                };
                // The pane whose agent holds the tab's status: validation and
                // replace-by-pane both key on it.
                let Some(agent) = snapshot.agents.iter().find(|agent| {
                    agent.tab_id == tab.tab_id && agent.agent_status == tab.agent_status
                }) else {
                    continue;
                };
                let body = super::notification_format::agent_notification_body(
                    snapshot,
                    agent.agent.as_deref(),
                    Some(agent.pane_id.as_str()),
                    Some(tab.workspace_id.as_str()),
                );
                let event = SemanticNotification {
                    kind,
                    title: String::new(),
                    body,
                    sound: self.config.sound_enabled.then_some(sound),
                    agent: agent.agent.clone(),
                    workspace_id: Some(tab.workspace_id.clone()),
                    tab_id: Some(tab.tab_id.clone()),
                    pane_id: Some(agent.pane_id.clone()),
                    position: None,
                };
                if self.notification_target_is_active(&endpoint.endpoint_id, &event) {
                    continue;
                }
                waiting.push(WaitingTab {
                    key: (endpoint.endpoint_id.clone(), tab.tab_id.clone()),
                    status: tab.agent_status,
                    state_change_seq: agent.state_change_seq,
                    label: tab.label.clone(),
                    event,
                });
            }
        }
        waiting
    }

    /// Track marked waiting tabs and queue the reminders that are due. Returns
    /// whether a visible card was replaced (the caller repaints).
    pub(super) fn tick_idle_reminders(&mut self, now: std::time::Instant) -> bool {
        let minutes = self.config.idle_reminder_minutes;
        if minutes == 0 {
            self.idle_reminders.clear();
            return false;
        }
        if self.idle_reminders.is_empty()
            && !self.endpoints.iter().any(|endpoint| {
                endpoint
                    .snapshot
                    .as_deref()
                    .is_some_and(|snapshot| snapshot.tabs.iter().any(|tab| tab.remind))
            })
        {
            return false;
        }
        let interval = reminder_interval(minutes);
        let waiting = self.waiting_marked_tabs();
        self.idle_reminders
            .retain(|key, _| waiting.iter().any(|tab| &tab.key == key));
        let mut due = Vec::new();
        for tab in waiting {
            let fresh = ClientIdleReminder {
                status: tab.status,
                state_change_seq: tab.state_change_seq,
                since: now,
                anchor: now,
            };
            let reminder = self
                .idle_reminders
                .entry(tab.key.clone())
                .or_insert_with(|| fresh.clone());
            if reminder.status != tab.status || reminder.state_change_seq != tab.state_change_seq {
                *reminder = fresh;
                continue;
            }
            if now < reminder.anchor + interval {
                continue;
            }
            // From now, not anchor + interval: a shorter interval after a live
            // reload must not fire a burst of catch-up reminders.
            reminder.anchor = now;
            let waited = now.saturating_duration_since(reminder.since).as_secs() / 60;
            let mut event = tab.event;
            event.title = match tab.status {
                AgentStatus::Blocked => format!("{} still waiting", tab.label),
                _ => format!("{} finished {waited} min ago", tab.label),
            };
            due.push((tab.key.0, event));
        }
        let mut repaint = false;
        for (endpoint_id, event) in due {
            if let Some(pane_id) = event.pane_id.as_deref() {
                repaint |= self.replace_pane_notifications(&endpoint_id, pane_id, now);
            }
            self.pending_notifications.push(ClientPendingNotification {
                endpoint_id,
                event,
                deadline: now,
                expires_at: now.checked_add(COMPLETION_EVIDENCE_GRACE).unwrap_or(now),
                validate_state: true,
            });
        }
        repaint
    }

    /// When the next idle reminder is due, for the client loop's timer.
    pub(crate) fn next_idle_reminder_deadline(&self) -> Option<std::time::Instant> {
        let minutes = self.config.idle_reminder_minutes;
        if minutes == 0 {
            return None;
        }
        let interval = reminder_interval(minutes);
        self.idle_reminders
            .values()
            .map(|reminder| reminder.anchor + interval)
            .min()
    }
}

/// The settings overlay's reminder choices, in minutes (0 = off).
const REMINDER_CHOICES: [u32; 6] = [0, 5, 10, 15, 30, 60];

/// The reminder section's rows: a configured value outside the list comes
/// first as "custom: N min", then the fixed choices.
pub(super) fn reminder_choices(configured: u32) -> Vec<(String, u32)> {
    let label = |minutes: u32| match minutes {
        0 => "off".to_string(),
        minutes => format!("{minutes} min"),
    };
    let custom = (!REMINDER_CHOICES.contains(&configured))
        .then(|| (format!("custom: {configured} min"), configured));
    custom
        .into_iter()
        .chain(
            REMINDER_CHOICES
                .into_iter()
                .map(|minutes| (label(minutes), minutes)),
        )
        .collect()
}

/// The row of the configured value in `reminder_choices(configured)`.
pub(super) fn reminder_choice_index(configured: u32) -> usize {
    reminder_choices(configured)
        .iter()
        .position(|(_, minutes)| *minutes == configured)
        .unwrap_or(0)
}
