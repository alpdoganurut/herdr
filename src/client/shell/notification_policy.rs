use super::*;

const MAX_QUEUED_NOTIFICATIONS: usize = 8;
pub(super) const COMPLETION_EVIDENCE_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
pub(super) const COMPLETION_RECHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationValidation {
    Current,
    AwaitingSnapshot,
    Stale,
}

fn notification_duration(kind: SemanticNotificationKind) -> std::time::Duration {
    std::time::Duration::from_secs(match kind {
        SemanticNotificationKind::NeedsAttention => 8,
        SemanticNotificationKind::Finished => 5,
        SemanticNotificationKind::UpdateInstalled => 3,
        SemanticNotificationKind::Custom => 5,
    })
}

impl ClientShellState {
    pub(super) fn retire_endpoint_notifications(&mut self, endpoint_id: &ClientEndpointId) {
        self.pending_notifications
            .retain(|pending| &pending.endpoint_id != endpoint_id);
        self.queued_notifications
            .retain(|queued| &queued.endpoint_id != endpoint_id);
        if self
            .visible_notifications
            .iter()
            .any(|visible| &visible.endpoint_id == endpoint_id)
        {
            self.visible_notifications
                .retain(|visible| &visible.endpoint_id != endpoint_id);
            if self.visible_notifications.is_empty() {
                self.promote_queued_notification(std::time::Instant::now());
            }
        }
    }

    fn queue_visible_notification(
        &mut self,
        mut notification: ClientVisibleNotification,
        now: std::time::Instant,
    ) {
        if self.config.toast_sticky {
            // Sticky cards never expire; the deadline is unread until a live reload
            // switches back to timed mode.
            notification.deadline = now + notification_duration(notification.event.kind);
            self.visible_notifications.push_back(notification);
            return;
        }
        if self.visible_notifications.is_empty() {
            notification.deadline = now + notification_duration(notification.event.kind);
            self.visible_notifications.push_back(notification);
            return;
        }
        if self.queued_notifications.len() == MAX_QUEUED_NOTIFICATIONS {
            self.queued_notifications.pop_front();
        }
        self.queued_notifications.push_back(notification);
    }

    fn promote_queued_notification(&mut self, now: std::time::Instant) -> bool {
        let Some(mut notification) = self.queued_notifications.pop_front() else {
            return false;
        };
        notification.deadline = now + notification_duration(notification.event.kind);
        self.visible_notifications.push_back(notification);
        true
    }

    /// Index of the card `open_notification_target` acts on: the single timed toast, or
    /// the newest sticky card.
    pub(super) fn primary_notification_index(&self) -> Option<usize> {
        if self.config.toast_sticky {
            self.visible_notifications.len().checked_sub(1)
        } else {
            (!self.visible_notifications.is_empty()).then_some(0)
        }
    }

    pub(super) fn focus_visible_notification(&mut self, outcome: &mut ClientShellInput) {
        if let Some(index) = self.primary_notification_index() {
            self.focus_notification_at(index, outcome);
        }
    }

    pub(super) fn dismiss_notification_at(&mut self, index: usize, outcome: &mut ClientShellInput) {
        if self.visible_notifications.remove(index).is_none() {
            return;
        }
        if self.visible_notifications.is_empty() {
            self.promote_queued_notification(std::time::Instant::now());
        }
        outcome.repaint = true;
    }

    pub(super) fn focus_notification_at(&mut self, index: usize, outcome: &mut ClientShellInput) {
        let Some(notification) = self.visible_notifications.get(index) else {
            return;
        };
        if notification.event.pane_id.is_some()
            && !self.endpoint_is_online(&notification.endpoint_id)
        {
            let label = self.endpoint_label(&notification.endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is unavailable"));
            outcome.repaint = true;
            return;
        }
        let notification = self
            .visible_notifications
            .remove(index)
            .expect("checked visible notification");
        if self.visible_notifications.is_empty() {
            self.promote_queued_notification(std::time::Instant::now());
        }
        outcome.repaint = true;
        let Some(pane_id) = notification.event.pane_id else {
            return;
        };
        if notification.endpoint_id == self.active_endpoint_id {
            self.push_endpoint_method(
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
        } else if self.endpoint_is_online(&notification.endpoint_id) {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id: notification.endpoint_id,
                target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
            });
        }
    }

    /// Moves cards between the sticky list and the timed queue after a live reload
    /// flips `ui.toast.herdr.sticky`.
    pub(super) fn rebalance_notification_cards(&mut self, now: std::time::Instant) {
        if self.config.toast_sticky {
            self.visible_notifications
                .extend(self.queued_notifications.drain(..));
            return;
        }
        while self.visible_notifications.len() > 1 {
            let Some(card) = self.visible_notifications.pop_back() else {
                break;
            };
            self.queued_notifications.push_front(card);
        }
        while self.queued_notifications.len() > MAX_QUEUED_NOTIFICATIONS {
            self.queued_notifications.pop_front();
        }
        if let Some(front) = self.visible_notifications.front_mut() {
            front.deadline = front
                .deadline
                .min(now + notification_duration(front.event.kind));
        }
    }

    /// Sticky clearing on every endpoint snapshot: a card whose target tab became
    /// focused, whose pane is gone, or whose agent status contradicts it is removed.
    pub(super) fn prune_sticky_notifications(&mut self, endpoint_id: &ClientEndpointId) {
        if !self.config.toast_sticky || self.visible_notifications.is_empty() {
            return;
        }
        let cards = std::mem::take(&mut self.visible_notifications);
        self.visible_notifications = cards
            .into_iter()
            .filter(|card| {
                &card.endpoint_id != endpoint_id
                    || !(self.notification_target_is_active(&card.endpoint_id, &card.event)
                        || self.sticky_notification_is_stale(&card.endpoint_id, &card.event))
            })
            .collect();
    }

    fn sticky_notification_is_stale(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> bool {
        let Some(pane_id) = event.pane_id.as_deref() else {
            return false;
        };
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return false;
        };
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id);
        if agent.is_none() && !snapshot.panes.iter().any(|pane| pane.pane_id == pane_id) {
            return true;
        }
        if event.kind == SemanticNotificationKind::Finished
            && agent
                .is_some_and(|agent| agent.agent_status == crate::api::schema::AgentStatus::Working)
        {
            return true;
        }
        matches!(
            self.notification_validation(endpoint_id, event),
            NotificationValidation::Stale
        )
    }

    pub(crate) fn receive_notification(
        &mut self,
        endpoint_id: &ClientEndpointId,
        event: SemanticNotification,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        let delay = if event.kind == SemanticNotificationKind::Custom {
            0
        } else {
            self.config.toast_delay_seconds
        };
        let deadline = now
            .checked_add(std::time::Duration::from_secs(delay))
            .unwrap_or(now);
        let cleared_visible = event
            .pane_id
            .as_deref()
            .is_some_and(|pane_id| self.replace_pane_notifications(endpoint_id, pane_id, now));
        // Completion evidence is advisory. A Finished effect is valid only while the
        // client-projected pane remains Done, even when delivery is immediate.
        let validate_state = delay > 0 || event.kind == SemanticNotificationKind::Finished;
        self.pending_notifications.push(ClientPendingNotification {
            endpoint_id: endpoint_id.clone(),
            event,
            deadline,
            expires_at: now.checked_add(COMPLETION_EVIDENCE_GRACE).unwrap_or(now),
            validate_state,
        });
        let (effects, repaint) = self.tick_notifications(now);
        (effects, repaint || cleared_visible)
    }

    /// A newer notification for a pane replaces that pane's pending, queued and
    /// visible ones. Returns whether a visible card was removed.
    pub(super) fn replace_pane_notifications(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        now: std::time::Instant,
    ) -> bool {
        let cleared_visible = self.visible_notifications.iter().any(|visible| {
            visible.endpoint_id == *endpoint_id && visible.event.pane_id.as_deref() == Some(pane_id)
        });
        self.pending_notifications.retain(|pending| {
            pending.endpoint_id != *endpoint_id || pending.event.pane_id.as_deref() != Some(pane_id)
        });
        self.queued_notifications.retain(|queued| {
            queued.endpoint_id != *endpoint_id || queued.event.pane_id.as_deref() != Some(pane_id)
        });
        if cleared_visible {
            self.visible_notifications.retain(|visible| {
                visible.endpoint_id != *endpoint_id
                    || visible.event.pane_id.as_deref() != Some(pane_id)
            });
            if self.visible_notifications.is_empty() {
                self.promote_queued_notification(now);
            }
        }
        cleared_visible
    }

    pub(crate) fn tick_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        // Due idle reminders join the pending list first, so this pass delivers them.
        let mut repaint = self.tick_idle_reminders(now);
        if !self.config.toast_sticky
            && self
                .visible_notifications
                .front()
                .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_notifications.pop_front();
            self.promote_queued_notification(now);
            repaint = true;
        }
        if self
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_endpoint_notice = None;
            repaint = true;
        }

        let pending = std::mem::take(&mut self.pending_notifications);
        let mut effects = Vec::new();
        for pending in pending {
            if pending.deadline > now {
                self.pending_notifications.push(pending);
                continue;
            }
            if pending.validate_state {
                match self.notification_validation(&pending.endpoint_id, &pending.event) {
                    NotificationValidation::Current => {}
                    NotificationValidation::AwaitingSnapshot if now < pending.expires_at => {
                        let mut pending = pending;
                        pending.deadline = now
                            .checked_add(COMPLETION_RECHECK_INTERVAL)
                            .unwrap_or(now)
                            .min(pending.expires_at);
                        self.pending_notifications.push(pending);
                        continue;
                    }
                    NotificationValidation::AwaitingSnapshot | NotificationValidation::Stale => {
                        continue;
                    }
                }
            }
            // Fork: final text for every delivery path (notification_format.rs).
            let mut pending = pending;
            pending.event = self.format_agent_notification(&pending.endpoint_id, pending.event);
            let target_active =
                self.notification_target_is_active(&pending.endpoint_id, &pending.event);
            let suppress_external = target_active && self.outer_focused != Some(false);
            if let Some(sound) = pending.event.sound {
                let suppress_sound =
                    pending.event.kind == SemanticNotificationKind::Finished && suppress_external;
                if !suppress_sound {
                    effects.push(ClientShellNotificationEffect::Sound {
                        sound: match sound {
                            SemanticNotificationSound::Done => crate::sound::Sound::Done,
                            SemanticNotificationSound::Request => crate::sound::Sound::Request,
                        },
                        agent: pending.event.agent.clone(),
                    });
                }
            }

            match self.config.toast_delivery {
                crate::config::ToastDelivery::Off => {}
                crate::config::ToastDelivery::Herdr if !target_active => {
                    self.queue_visible_notification(
                        ClientVisibleNotification {
                            endpoint_id: pending.endpoint_id,
                            event: pending.event,
                            deadline: now,
                        },
                        now,
                    );
                    repaint = true;
                }
                crate::config::ToastDelivery::Herdr => {}
                crate::config::ToastDelivery::Terminal if !suppress_external => {
                    effects.push(ClientShellNotificationEffect::Terminal {
                        title: pending.event.title,
                        body: pending.event.body,
                    });
                }
                crate::config::ToastDelivery::System if !suppress_external => {
                    effects.push(ClientShellNotificationEffect::System {
                        title: pending.event.title,
                        body: pending.event.body,
                    });
                }
                crate::config::ToastDelivery::Terminal | crate::config::ToastDelivery::System => {}
            }
        }
        (effects, repaint)
    }

    pub(super) fn notification_target_is_active(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> bool {
        if endpoint_id != &self.active_endpoint_id {
            return false;
        }
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return false;
        };
        if let Some(tab_id) = event.tab_id.as_deref() {
            return snapshot.focused_tab_id.as_deref() == Some(tab_id);
        }
        event.workspace_id.as_deref().is_some_and(|workspace_id| {
            snapshot.focused_workspace_id.as_deref() == Some(workspace_id)
        })
    }

    fn notification_validation(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> NotificationValidation {
        let Some(pane_id) = event.pane_id.as_deref() else {
            // Finished notifications carry no independently trustworthy completion state. Without
            // a projected pane to verify as Done, do not emit completion chrome or sound.
            return if event.kind == SemanticNotificationKind::Finished {
                NotificationValidation::Stale
            } else {
                NotificationValidation::Current
            };
        };
        let Some(agent) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane_id)
            })
        else {
            return NotificationValidation::AwaitingSnapshot;
        };
        match event.kind {
            SemanticNotificationKind::NeedsAttention
                if agent.agent_status == crate::api::schema::AgentStatus::Blocked =>
            {
                NotificationValidation::Current
            }
            SemanticNotificationKind::Finished
                if agent.agent_status == crate::api::schema::AgentStatus::Done =>
            {
                NotificationValidation::Current
            }
            SemanticNotificationKind::Finished
                if agent.agent_status == crate::api::schema::AgentStatus::Working =>
            {
                NotificationValidation::AwaitingSnapshot
            }
            SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => {
                NotificationValidation::Current
            }
            SemanticNotificationKind::NeedsAttention | SemanticNotificationKind::Finished => {
                NotificationValidation::Stale
            }
        }
    }
}
