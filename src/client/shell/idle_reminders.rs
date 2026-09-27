//! Client-side tab reminders. Two independent reminders per tab, both set
//! server-side (`tab.set_reminder`) and timed here:
//!
//! - **important** (`ClientShellTab.important`): while the tab's aggregate
//!   agent status is `Done` (finished, unseen) or `Blocked`, remind after
//!   `ui.idle_reminder_minutes` and again every as many minutes. Focusing the
//!   tab, the agent changing state (or the status changing), removing the
//!   mark, or the tab disappearing starts over.
//! - **remind** (`ClientShellTab.remind_every`): remind on a schedule whatever
//!   the agent does: every 5, 10 or 30 minutes or 1 or 6 hours from when the
//!   client first saw the interval (or the last firing), or daily at
//!   `ui.daily_reminder_time` local time. A firing due while the tab is
//!   focused is skipped and the interval starts again from then. A daily
//!   reminder missed while this client was not attached fires once when the
//!   client first sees the tab that day; interval reminders resume without
//!   catching up.
//!
//! A client restart starts every clock fresh. A reminder is a
//! `SemanticNotification` for the tab's pane that goes through the normal
//! pending-notification path: it replaces that pane's previous card (so the
//! tab shows one card), honours `ui.toast.delivery`, and plays
//! `Sound::Reminder` unless sounds are off. Once one has fired, the tab's
//! marker (`★` important, `◷` remind) is lit in the reminder's color until it
//! clears. The pass runs at the top of `tick_notifications`, so the client
//! loop's timer drives it.

use super::notification_policy::COMPLETION_EVIDENCE_GRACE;
use super::*;
use crate::api::schema::{AgentStatus, TabRemindInterval};

/// After an endpoint's snapshot first appears, tabs seen within this long
/// count as first sightings (a daily reminder missed today fires once) rather
/// than an interval being set just now.
const REMINDER_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Which reminder raised a notification: its card shows that reminder's
/// marker (`★` / `◷`) in place of the dot, and it plays `Sound::Reminder`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientReminderKind {
    Important,
    Scheduled,
}

impl ClientReminderKind {
    pub(super) fn glyph(self) -> &'static str {
        match self {
            ClientReminderKind::Important => super::tab_sidebar::TAB_IMPORTANT_MARKER,
            ClientReminderKind::Scheduled => super::tab_sidebar::TAB_REMIND_MARKER,
        }
    }
}

/// What a fired reminder was about; the lit marker's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientReminderLit {
    /// Important: the agent finished and nobody has looked.
    Finished,
    /// Important: the agent needs input.
    Blocked,
    /// A scheduled reminder.
    Scheduled,
}

impl ClientReminderLit {
    pub(super) fn color(self, palette: &Palette) -> ratatui::style::Color {
        match self {
            ClientReminderLit::Finished => status_color(AgentStatus::Done, palette),
            ClientReminderLit::Blocked => status_color(AgentStatus::Blocked, palette),
            ClientReminderLit::Scheduled => palette.accent,
        }
    }
}

/// One important tab waiting in Done/Blocked.
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
    /// Set once a reminder fired; lights the tab's `★`.
    pub(super) lit: Option<ClientReminderLit>,
}

/// One tab with a scheduled reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClientScheduledReminder {
    every: TabRemindInterval,
    /// The pane reminders are about, for clearing its card.
    pane_id: String,
    /// Interval reminders: the last firing (or skip), or when the interval
    /// was first seen.
    anchor: std::time::Instant,
    /// Daily reminders: the local day the last one fired (or was skipped).
    last_daily: Option<time::Date>,
    /// Set once a reminder fired, until the tab is focused; lights its `◷`.
    pub(super) lit: bool,
}

/// An important, waiting, unfocused tab seen in this pass.
struct WaitingTab {
    key: (ClientEndpointId, String),
    status: AgentStatus,
    state_change_seq: u64,
    label: String,
    event: SemanticNotification,
}

/// A tab with a scheduled reminder seen in this pass.
struct ScheduledTab {
    key: (ClientEndpointId, String),
    every: TabRemindInterval,
    focused: bool,
    first_sight: bool,
    label: String,
    event: SemanticNotification,
}

fn reminder_interval(minutes: u32) -> std::time::Duration {
    std::time::Duration::from_secs(u64::from(minutes) * 60)
}

/// A scheduled interval this build can act on.
fn known_interval(every: Option<TabRemindInterval>) -> Option<TabRemindInterval> {
    every.filter(|every| *every != TabRemindInterval::Unknown)
}

/// `tab.set_reminder` flipping `important` on `tab_id`
/// (`keys.toggle_tab_important`).
pub(super) fn tab_important_toggle_method(
    snapshot: &ClientShellSnapshot,
    tab_id: &str,
) -> Option<crate::api::schema::Method> {
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id)?;
    Some(crate::api::schema::Method::TabSetReminder(
        crate::api::schema::TabSetReminderParams {
            tab_id: tab.tab_id.clone(),
            important: Some(!tab.important),
            every: None,
        },
    ))
}

impl ClientShellState {
    /// The local wall-clock time daily reminders go by (overridable in tests).
    fn reminder_local_now(&self) -> Option<time::PrimitiveDateTime> {
        self.reminder_local_time
            .or_else(crate::platform::local_datetime)
    }

    /// Important tabs that are waiting and unfocused, across every endpoint.
    fn waiting_important_tabs(&self) -> Vec<WaitingTab> {
        let mut waiting = Vec::new();
        for endpoint in &self.endpoints {
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            for tab in snapshot.tabs.iter().filter(|tab| tab.important) {
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

    /// Tabs with a scheduled reminder, across every endpoint.
    fn scheduled_tabs(&self, now: std::time::Instant) -> Vec<ScheduledTab> {
        let mut scheduled = Vec::new();
        for endpoint in &self.endpoints {
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            let first_sight = self
                .reminder_epochs
                .get(&endpoint.endpoint_id)
                .is_none_or(|epoch| now.saturating_duration_since(*epoch) < REMINDER_STARTUP_GRACE);
            for tab in &snapshot.tabs {
                let Some(every) = known_interval(tab.remind_every) else {
                    continue;
                };
                // The tab's agent pane, else its first pane (a plain shell).
                let agent = snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.tab_id == tab.tab_id);
                let Some(pane_id) = agent.map(|agent| agent.pane_id.clone()).or_else(|| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.tab_id == tab.tab_id)
                        .map(|pane| pane.pane_id.clone())
                }) else {
                    continue;
                };
                let agent_name = agent.and_then(|agent| agent.agent.clone());
                let body = super::notification_format::agent_notification_body(
                    snapshot,
                    agent_name.as_deref(),
                    Some(pane_id.as_str()),
                    Some(tab.workspace_id.as_str()),
                );
                let event = SemanticNotification {
                    kind: SemanticNotificationKind::Custom,
                    title: String::new(),
                    body,
                    sound: self
                        .config
                        .sound_enabled
                        .then_some(SemanticNotificationSound::Done),
                    agent: agent_name,
                    workspace_id: Some(tab.workspace_id.clone()),
                    tab_id: Some(tab.tab_id.clone()),
                    pane_id: Some(pane_id),
                    position: None,
                };
                let focused = self.notification_target_is_active(&endpoint.endpoint_id, &event);
                scheduled.push(ScheduledTab {
                    key: (endpoint.endpoint_id.clone(), tab.tab_id.clone()),
                    every,
                    focused,
                    first_sight,
                    label: tab.label.clone(),
                    event,
                });
            }
        }
        scheduled
    }

    /// Track reminding tabs and queue the reminders that are due. Returns
    /// whether a visible card was replaced or removed (the caller repaints).
    pub(super) fn tick_idle_reminders(&mut self, now: std::time::Instant) -> bool {
        self.track_reminder_epochs(now);
        let repaint = self.tick_important_reminders(now);
        self.tick_scheduled_reminders(now) || repaint
    }

    /// When each endpoint's snapshot was first seen; an endpoint that goes
    /// away and comes back starts over (its tabs are first sightings).
    fn track_reminder_epochs(&mut self, now: std::time::Instant) {
        if self.reminder_epochs.len() == self.endpoints.len()
            && self.endpoints.iter().all(|endpoint| {
                endpoint.snapshot.is_some()
                    && self.reminder_epochs.contains_key(&endpoint.endpoint_id)
            })
        {
            return;
        }
        let online: Vec<ClientEndpointId> = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.snapshot.is_some())
            .map(|endpoint| endpoint.endpoint_id.clone())
            .collect();
        self.reminder_epochs
            .retain(|endpoint_id, _| online.contains(endpoint_id));
        for endpoint_id in online {
            self.reminder_epochs.entry(endpoint_id).or_insert(now);
        }
    }

    fn tick_important_reminders(&mut self, now: std::time::Instant) -> bool {
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
                    .is_some_and(|snapshot| snapshot.tabs.iter().any(|tab| tab.important))
            })
        {
            return false;
        }
        let interval = reminder_interval(minutes);
        let waiting = self.waiting_important_tabs();
        self.idle_reminders
            .retain(|key, _| waiting.iter().any(|tab| &tab.key == key));
        let mut due = Vec::new();
        for tab in waiting {
            let fresh = ClientIdleReminder {
                status: tab.status,
                state_change_seq: tab.state_change_seq,
                since: now,
                anchor: now,
                lit: None,
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
            let (title, lit) = match tab.status {
                AgentStatus::Blocked => (
                    format!("{} still waiting", tab.label),
                    ClientReminderLit::Blocked,
                ),
                _ => (
                    format!("{} finished {waited} min ago", tab.label),
                    ClientReminderLit::Finished,
                ),
            };
            event.title = title;
            reminder.lit = Some(lit);
            due.push((tab.key.0, event, ClientReminderKind::Important));
        }
        self.queue_reminders(due, now)
    }

    fn tick_scheduled_reminders(&mut self, now: std::time::Instant) -> bool {
        if self.scheduled_reminders.is_empty()
            && !self.endpoints.iter().any(|endpoint| {
                endpoint.snapshot.as_deref().is_some_and(|snapshot| {
                    snapshot
                        .tabs
                        .iter()
                        .any(|tab| known_interval(tab.remind_every).is_some())
                })
            })
        {
            return false;
        }
        let tabs = self.scheduled_tabs(now);
        // Daily reminders go by the local clock: its date and the time past.
        let daily = tabs
            .iter()
            .any(|tab| tab.every == TabRemindInterval::Daily)
            .then(|| self.reminder_local_now())
            .flatten()
            .map(|local| {
                let target = self.config.daily_reminder_minutes;
                let minutes = u32::from(local.hour()) * 60 + u32::from(local.minute());
                (local.date(), minutes >= target)
            });

        // Tabs that lost the reminder (or went away) take their card along.
        let mut removed_cards = Vec::new();
        self.scheduled_reminders.retain(|key, reminder| {
            let kept = tabs.iter().any(|tab| &tab.key == key);
            if !kept && reminder.lit {
                removed_cards.push((key.0.clone(), reminder.pane_id.clone()));
            }
            kept
        });
        let mut due = Vec::new();
        for tab in tabs {
            let pane_id = tab.event.pane_id.clone().unwrap_or_default();
            // A daily reminder set just now waits for the next time of day;
            // one first seen (client start, reconnect) may still fire today.
            let last_daily = match daily {
                Some((today, true)) if !tab.first_sight => Some(today),
                _ => None,
            };
            let fresh = ClientScheduledReminder {
                every: tab.every,
                pane_id: pane_id.clone(),
                anchor: now,
                last_daily,
                lit: false,
            };
            let reminder = self
                .scheduled_reminders
                .entry(tab.key.clone())
                .or_insert_with(|| fresh.clone());
            if reminder.every != tab.every {
                if reminder.lit {
                    removed_cards.push((tab.key.0.clone(), reminder.pane_id.clone()));
                }
                *reminder = fresh;
                continue;
            }
            reminder.pane_id = pane_id;
            if tab.focused {
                reminder.lit = false;
            }
            let is_due = match tab.every.period() {
                Some(period) => now >= reminder.anchor + period,
                None => {
                    daily.is_some_and(|(today, past)| past && reminder.last_daily != Some(today))
                }
            };
            if !is_due {
                continue;
            }
            // Fired or skipped, the schedule restarts from now.
            reminder.anchor = now;
            if let Some((today, _)) = daily.filter(|_| tab.every == TabRemindInterval::Daily) {
                reminder.last_daily = Some(today);
            }
            if tab.focused {
                continue;
            }
            reminder.lit = true;
            let mut event = tab.event;
            event.title = format!("{} reminder", tab.label);
            due.push((tab.key.0, event, ClientReminderKind::Scheduled));
        }
        let mut repaint = false;
        for (endpoint_id, pane_id) in removed_cards {
            repaint |= self.replace_pane_notifications(&endpoint_id, &pane_id, now);
        }
        self.queue_reminders(due, now) || repaint
    }

    /// Queue due reminders, each replacing its pane's cards. Important ones
    /// check the agent still holds the status they are about.
    fn queue_reminders(
        &mut self,
        due: Vec<(ClientEndpointId, SemanticNotification, ClientReminderKind)>,
        now: std::time::Instant,
    ) -> bool {
        let mut repaint = false;
        for (endpoint_id, event, kind) in due {
            if let Some(pane_id) = event.pane_id.as_deref() {
                repaint |= self.replace_pane_notifications(&endpoint_id, pane_id, now);
            }
            self.pending_notifications.push(ClientPendingNotification {
                endpoint_id,
                event,
                deadline: now,
                expires_at: now.checked_add(COMPLETION_EVIDENCE_GRACE).unwrap_or(now),
                validate_state: kind == ClientReminderKind::Important,
                reminder: Some(kind),
            });
        }
        repaint
    }

    /// When the next timed reminder is due, for the client loop's timer.
    /// Daily reminders are checked on the loop's regular tick.
    pub(crate) fn next_idle_reminder_deadline(&self) -> Option<std::time::Instant> {
        let minutes = self.config.idle_reminder_minutes;
        let important = (minutes > 0)
            .then(|| {
                let interval = reminder_interval(minutes);
                self.idle_reminders
                    .values()
                    .map(move |reminder| reminder.anchor + interval)
            })
            .into_iter()
            .flatten();
        let scheduled = self
            .scheduled_reminders
            .values()
            .filter_map(|reminder| Some(reminder.anchor + reminder.every.period()?));
        important.chain(scheduled).min()
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
