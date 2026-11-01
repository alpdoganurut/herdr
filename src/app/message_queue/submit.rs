//! Fork (lost-message fix, `.local/prd/lost-message/decision.md` PR2 and
//! PR3): verify before Enter, and confirm after it.
//!
//! Verify before Enter. A message for a Claude Code or Codex target (the
//! agents whose input box the typing guard reads, src/app/typing_guard.rs)
//! goes in as two writes: its text first (the pointer line or the paste,
//! no Enter), then, once the target's input box shows that text, the Enter.
//! The queue looks at the box from [`SUBMIT_FIRST_LOOK`] after the text,
//! every [`SUBMIT_POLL`], up to [`SUBMIT_WINDOW`]: one read of the screen's
//! bottom per look, through the queue's own deadlines, never per frame.
//! While a batch is in flight its target waits (`submitting`), so nothing
//! else is typed in after it. Enter goes in only when the box shows the text
//! (whitespace ignored, so a wrapped pointer line matches; a paste may show
//! as the agent's placeholder) and the user typed nothing into the pane
//! since the text was written. Only then is the message `delivered` (and a
//! pointer line's envelope kept for `agents.read_messages`). At the deadline
//! a box proven empty (the marker and at most a dim suggestion) means the
//! text never landed: no Enter, the batch stays at the front of its queue
//! with the same ids and one more attempt, and after
//! [`MAX_SUBMIT_ATTEMPTS`] it is held (no more typing) and its senders are
//! told. Anything else at the deadline (other text in the box, no box, the
//! user typing) is `unconfirmed`: no Enter, no retry, its senders and the
//! user are told. A late render (the box shows exactly the text herdr typed
//! within [`LATE_RENDER_WINDOW`] of an unconfirmed batch, nothing typed by
//! the user) gets the Enter alone and is `delivered` after all. A message
//! is never typed in twice for one delivery.
//!
//! Confirm after Enter (every agent). A delivered message is `confirmed`
//! once its target reads it by id (a pointer line) within
//! [`CONFIRM_READ_WITHIN`], or starts a turn (its hooks' turn start or a
//! working edge) within [`CONFIRM_TURN_WITHIN`] of a paste; otherwise it is
//! `unconfirmed`, its sender gets a herdr notice and the user an agent card.
//! Nothing is redelivered: the sender sends again (a new id). A pointer
//! message read after its window still turns `confirmed`.
//!
//! All of it is runtime state: a restart or a live handoff during the 2.5 s
//! window forgets the batch in flight (its messages stay queued).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{minutes, notice_target_name, App, MessageQueue, QueuedMessage, MAX_QUEUED, SETTLE};
use crate::api::schema::AgentPromptParams;
use crate::app::api::responses::encode_error;
use crate::app::message_pointer::Outgoing;
use crate::app::typing_guard::{self, PromptBox};
use crate::coordinator::messages::{
    AgentMessage, OUTCOME_CONFIRMED, OUTCOME_DELIVERED, OUTCOME_UNCONFIRMED,
};

/// The first look at the input box after the text was written.
pub(crate) const SUBMIT_FIRST_LOOK: Duration = Duration::from_millis(150);
/// How often the input box is looked at until the text shows.
pub(crate) const SUBMIT_POLL: Duration = Duration::from_millis(100);
/// How long after the text the input box may take to show it.
pub(crate) const SUBMIT_WINDOW: Duration = Duration::from_millis(2500);
/// Typing attempts into a box that stayed empty before a batch is held.
pub(crate) const MAX_SUBMIT_ATTEMPTS: u32 = 2;
/// How long an unconfirmed batch's text may still show up (a late render)
/// and get its Enter.
pub(crate) const LATE_RENDER_WINDOW: Duration = Duration::from_secs(60);
/// How often the box of an unconfirmed batch is looked at for a late render.
pub(crate) const LATE_RENDER_POLL: Duration = Duration::from_secs(1);
/// How long a pointer line's target has to read the message by id.
pub(crate) const CONFIRM_READ_WITHIN: Duration = Duration::from_secs(60);
/// How long a paste's target has to start a turn.
pub(crate) const CONFIRM_TURN_WITHIN: Duration = Duration::from_secs(30);
/// The wait reason of a target whose first message was held after
/// [`MAX_SUBMIT_ATTEMPTS`].
pub(crate) const SUBMIT_HELD_REASON: &str =
    "typed in twice but it never showed in its input box; held";
/// Pointer messages marked unconfirmed whose late read still confirms them.
const UNCONFIRMED_KEEP: usize = 64;
/// Delivered messages waiting for their confirmation, at most.
const CONFIRM_KEEP: usize = MAX_QUEUED;

/// A batch whose text was typed into its target, waiting for the input box
/// to show it before its Enter.
#[derive(Debug, Clone)]
pub(crate) struct PendingSubmit {
    ids: Vec<String>,
    items: Vec<Outgoing>,
    /// What was typed: the pointer line, or the envelopes' paste text.
    expected: String,
    /// A (bracketed) paste, not a typed pointer line.
    paste: bool,
    agent: crate::detect::Agent,
    /// When the text was written, on the clock user input is stamped with.
    written_at: Instant,
    next_poll: Instant,
    deadline: Instant,
    enter: Vec<u8>,
    from: Option<crate::terminal::TerminalId>,
    /// The coordinator's turn marker, cleared when no Enter goes in.
    marker: Option<crate::coordinator::turn::Turn>,
}

/// An unconfirmed batch whose text may still show up in its box.
#[derive(Debug, Clone)]
struct LateSubmit {
    submit: PendingSubmit,
    entries: Vec<QueuedMessage>,
    until: Instant,
    next_poll: Instant,
}

/// Delivered messages waiting for their target to take them in.
#[derive(Debug, Clone)]
struct ConfirmWatch {
    terminal_id: String,
    entries: Vec<QueuedMessage>,
    paste: bool,
    deadline: Instant,
}

/// The queue's delivery checks (runtime only; on [`MessageQueue`]).
#[derive(Debug, Default)]
pub(crate) struct DeliveryWatch {
    /// Batches typed in, waiting for their Enter, by target terminal.
    pending: HashMap<String, PendingSubmit>,
    /// Unconfirmed batches whose text may still render, by target terminal.
    late: HashMap<String, LateSubmit>,
    confirm: Vec<ConfirmWatch>,
    /// Pointer messages marked unconfirmed (their log lines), newest last.
    unconfirmed: Vec<AgentMessage>,
    /// Tests: the checks run (off by default, so the older tests keep their
    /// one-write deliveries).
    #[cfg(test)]
    pub(crate) enabled_for_test: bool,
}

impl DeliveryWatch {
    /// The earliest look or confirmation deadline.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let pending = self.pending.values().map(|submit| submit.next_poll);
        let late = self
            .late
            .values()
            .map(|late| late.next_poll.min(late.until));
        let confirm = self.confirm.iter().map(|watch| watch.deadline);
        pending.chain(late).chain(confirm).min()
    }

    /// Whether anything waits for a look or a confirmation.
    pub(super) fn is_active(&self) -> bool {
        !self.pending.is_empty() || !self.late.is_empty() || !self.confirm.is_empty()
    }

    /// Whether a batch is in flight into `terminal_id`.
    pub(super) fn submitting(&self, terminal_id: &str) -> bool {
        self.pending.contains_key(terminal_id)
    }

    /// Whether message `id` is in a batch in flight.
    pub(super) fn submitting_id(&self, id: &str) -> bool {
        self.pending
            .values()
            .any(|submit| submit.ids.iter().any(|pending| pending == id))
    }

    /// Tests: whether `id` waits for its confirmation.
    #[cfg(test)]
    pub(crate) fn awaiting_confirmation(&self, id: &str) -> bool {
        self.confirm
            .iter()
            .any(|watch| watch.entries.iter().any(|entry| entry.id() == id))
    }

    /// Tests: every batch in flight names messages that are queued.
    #[cfg(test)]
    pub(crate) fn assert_invariants_for_test(&self, entries: &[QueuedMessage]) {
        for (terminal_id, submit) in &self.pending {
            for id in &submit.ids {
                assert!(
                    entries
                        .iter()
                        .any(|entry| entry.id() == id && &entry.terminal_id == terminal_id),
                    "a batch in flight into {terminal_id} names {id}, which is not queued for it"
                );
            }
        }
    }
}

impl MessageQueue {
    /// Whether the first message queued for `terminal_id` is held after
    /// [`MAX_SUBMIT_ATTEMPTS`] typing attempts into an empty box.
    pub(super) fn submit_held(&self, terminal_id: &str) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.terminal_id == terminal_id)
            .is_some_and(|entry| entry.attempts >= MAX_SUBMIT_ATTEMPTS)
    }

    pub(super) fn entries_with(&self, ids: &[String]) -> Vec<QueuedMessage> {
        self.entries
            .iter()
            .filter(|entry| ids.iter().any(|id| id == entry.id()))
            .cloned()
            .collect()
    }
}

/// The input attribution of herdr's writes for a message.
fn message_source(
    from: Option<&crate::terminal::TerminalId>,
    message_id: &str,
) -> crate::agents_model::InputSource {
    crate::agents_model::InputSource::Programmatic(match from {
        Some(from) => crate::agents_model::Programmatic::AgentMessage {
            id: message_id.to_string(),
            from: from.clone(),
        },
        None => crate::agents_model::Programmatic::Api,
    })
}

/// Whether a queued message has a sender herdr can tell (not a notice from
/// herdr itself, not an older sender).
fn has_sender(entry: &QueuedMessage) -> bool {
    !entry.legacy && (entry.from_terminal.is_some() || entry.message.from_pane.is_some())
}

/// What a look at a target's input box found.
struct BoxLook {
    /// The marker-led box's content; `None` when it is not on screen.
    prompt: Option<PromptBox>,
    /// The user typed into the pane since the text was written.
    user_typed: bool,
}

impl App {
    /// The agent of `terminal_id` when a message for it goes in as two
    /// writes (its input box is readable: Claude Code, Codex).
    pub(super) fn submit_agent(&self, terminal_id: &str) -> Option<crate::detect::Agent> {
        #[cfg(test)]
        if !self.message_queue.delivery.enabled_for_test {
            return None;
        }
        let agent = self
            .state
            .terminals
            .values()
            .find(|terminal| terminal.id.as_str() == terminal_id)?
            .effective_known_agent()?;
        typing_guard::prompt_box_supported(agent).then_some(agent)
    }

    /// Whether delivered messages wait for their confirmation.
    fn confirms_delivery(&self) -> bool {
        #[cfg(test)]
        {
            self.message_queue.delivery.enabled_for_test
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    /// Type the text of `items` into `pane` (guarded, as
    /// [`App::type_message`], no Enter) and wait for it to show in the input
    /// box ([`App::drive_delivery_watch`]). Every id must be queued for
    /// `terminal_id` by the end of the caller's `&mut App` borrow. `Err`
    /// carries the error response; nothing was written then.
    #[allow(clippy::too_many_arguments)] // type_message's parts plus the agent and the clock.
    pub(super) fn start_submit(
        &mut self,
        request_id: String,
        terminal_id: &str,
        pane: &str,
        items: &[Outgoing],
        coordinator: bool,
        from: Option<&crate::terminal::TerminalId>,
        agent: crate::detect::Agent,
        now: Instant,
    ) -> Result<(), String> {
        let message_id = items.first().map(|item| item.id.as_str()).unwrap_or("");
        let input = self.message_input(terminal_id, items);
        let params = AgentPromptParams {
            target: pane.to_string(),
            text: input.text.clone(),
            wait: None,
            guard_user_typing: true,
        };
        // As type_message: the coordinator's marker only once the prompt
        // can go in.
        if coordinator {
            self.validate_agent_prompt(request_id.clone(), &params)?;
        }
        let marker = if coordinator {
            match self.write_coordinator_message_turn(pane, message_id) {
                Ok(marker) => Some(marker),
                Err(reason) => return Err(encode_error(request_id, "busy", reason)),
            }
        } else {
            None
        };
        let written = self.queue_agent_prompt_text(
            request_id,
            params,
            message_source(from, message_id),
            input.typed,
        );
        let enter = match written {
            Ok(enter) => enter,
            Err(response) => {
                if let Some(marker) = marker {
                    crate::coordinator::turn::clear_if(&self.coordinator.dir, &marker);
                }
                return Err(response);
            }
        };
        let written_at = Instant::now();
        self.message_queue.delivery.late.remove(terminal_id);
        self.message_queue.delivery.pending.insert(
            terminal_id.to_string(),
            PendingSubmit {
                ids: items.iter().map(|item| item.id.clone()).collect(),
                items: items.to_vec(),
                expected: input.text,
                paste: !input.typed,
                agent,
                written_at,
                next_poll: now + SUBMIT_FIRST_LOOK,
                deadline: now + SUBMIT_WINDOW,
                enter,
                from: from.cloned(),
                marker,
            },
        );
        Ok(())
    }

    /// The delivery checks' share of a queue pass: batches in flight, late
    /// renders, confirmations past their window. Returns whether anything
    /// was typed in or moved.
    pub(super) fn drive_delivery_watch(
        &mut self,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let mut changed = false;
        let pending: Vec<String> = self
            .message_queue
            .delivery
            .pending
            .keys()
            .cloned()
            .collect();
        for terminal_id in pending {
            changed |= self.poll_pending_submit(&terminal_id, now, now_unix, next);
        }
        let late: Vec<String> = self.message_queue.delivery.late.keys().cloned().collect();
        for terminal_id in late {
            changed |= self.poll_late_submit(&terminal_id, now, now_unix, next);
        }
        changed | self.expire_confirmations(now, now_unix, next)
    }

    /// One look at the box of a batch in flight.
    fn poll_pending_submit(
        &mut self,
        terminal_id: &str,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let Some(submit) = self
            .message_queue
            .delivery
            .pending
            .get(terminal_id)
            .cloned()
        else {
            return false;
        };
        let queued = submit.ids.iter().all(|id| {
            self.message_queue
                .entries
                .iter()
                .any(|entry| entry.id() == id && entry.terminal_id == terminal_id)
        });
        if !queued {
            // Claimed, expired or dropped meanwhile: no Enter for them.
            tracing::info!(
                target = %terminal_id,
                "message queue: a batch in flight left the queue before its Enter"
            );
            self.end_submit(terminal_id, &submit);
            return true;
        }
        if now < submit.next_poll {
            next.push(submit.next_poll);
            return false;
        }
        let Some(look) = self.look_at_box(terminal_id, &submit) else {
            // The target is gone: its messages follow the gone grace.
            self.end_submit(terminal_id, &submit);
            return true;
        };
        let shows = matches!(
            &look.prompt,
            Some(PromptBox::Draft { typed, .. })
                if typing_guard::draft_shows(typed, &submit.expected, submit.paste)
        );
        if look.user_typed {
            self.submit_unconfirmed(
                terminal_id,
                submit,
                "its user typed in it before herdr pressed Enter",
                now,
                now_unix,
            );
            return true;
        }
        if shows {
            return self.press_submit_enter(terminal_id, submit, now, now_unix);
        }
        if now < submit.deadline {
            let poll = (now + SUBMIT_POLL).min(submit.deadline);
            if let Some(pending) = self.message_queue.delivery.pending.get_mut(terminal_id) {
                pending.next_poll = poll;
            }
            next.push(poll);
            return false;
        }
        match look.prompt {
            Some(PromptBox::Empty | PromptBox::Suggestion(_)) => {
                self.submit_retry(terminal_id, submit, now, now_unix, next)
            }
            Some(PromptBox::Draft { .. }) => {
                self.submit_unconfirmed(
                    terminal_id,
                    submit,
                    "its input box shows other text",
                    now,
                    now_unix,
                );
                true
            }
            None => {
                self.submit_unconfirmed(
                    terminal_id,
                    submit,
                    "its input box went off screen (a dialog or a menu?)",
                    now,
                    now_unix,
                );
                true
            }
        }
    }

    /// Read the target's input box once (the screen's bottom with its
    /// styles) and whether its user typed since the text went in. `None`
    /// when the target or its runtime is gone.
    fn look_at_box(&self, terminal_id: &str, submit: &PendingSubmit) -> Option<BoxLook> {
        let target = self.message_target(terminal_id, None)?;
        let runtime = self.lookup_runtime_sender(target.ws_idx, target.pane_id)?;
        let user_typed = runtime
            .last_user_input()
            .is_some_and(|at| at > submit.written_at);
        let rows = usize::from(runtime.current_size().0.max(1));
        let ansi = runtime.recent_ansi_snapshot(rows).text;
        Some(BoxLook {
            prompt: typing_guard::input_box(submit.agent, &ansi),
            user_typed,
        })
    }

    /// A batch in flight ends without an Enter (no outcome of its own).
    fn end_submit(&mut self, terminal_id: &str, submit: &PendingSubmit) {
        self.message_queue.delivery.pending.remove(terminal_id);
        if let Some(marker) = &submit.marker {
            crate::coordinator::turn::clear_if(&self.coordinator.dir, marker);
        }
        if self.message_queue.queued_for(terminal_id) {
            self.message_queue.due = true;
        }
    }

    /// Write the Enter alone into `terminal_id`. `false` when it could not
    /// be written (the target or its runtime is gone, its input closed).
    fn write_submit_enter(&mut self, terminal_id: &str, submit: &PendingSubmit) -> bool {
        let Some(target) = self.message_target(terminal_id, None) else {
            return false;
        };
        let Some(runtime) = self.lookup_runtime_sender(target.ws_idx, target.pane_id) else {
            return false;
        };
        if let Err(err) = runtime.queue_user_input_submission(
            Bytes::new(),
            Bytes::from(submit.enter.clone()),
            Duration::ZERO,
            None,
        ) {
            tracing::warn!(error = %err, "message queue: the Enter could not be written");
            return false;
        }
        let first = submit.ids.first().map(String::as_str).unwrap_or("");
        self.note_pane_input(
            target.ws_idx,
            target.pane_id,
            message_source(submit.from.as_ref(), first),
        );
        true
    }

    /// The box shows the text: press Enter, and the batch is delivered.
    fn press_submit_enter(
        &mut self,
        terminal_id: &str,
        submit: PendingSubmit,
        now: Instant,
        now_unix: u64,
    ) -> bool {
        if !self.write_submit_enter(terminal_id, &submit) {
            self.submit_unconfirmed(
                terminal_id,
                submit,
                "herdr could not press Enter in it",
                now,
                now_unix,
            );
            return true;
        }
        self.message_queue.delivery.pending.remove(terminal_id);
        let entries = self.message_queue.entries_with(&submit.ids);
        if !submit.paste {
            self.remember_delivered(terminal_id, &submit.items);
        }
        self.message_queue.ready_since.remove(terminal_id);
        self.message_queue.settle_screens.remove(terminal_id);
        self.message_queue.interrupted_at.remove(terminal_id);
        self.message_queue.user_blocked.remove(terminal_id);
        self.message_queue
            .finish(&submit.ids, OUTCOME_DELIVERED, now_unix);
        tracing::info!(
            target = %terminal_id,
            messages = submit.ids.len(),
            "message queue: the text showed in the input box; Enter pressed"
        );
        self.watch_confirmation(terminal_id, entries, submit.paste, now);
        if self.message_queue.queued_for(terminal_id) {
            // The rest goes in once the target settled again.
            self.message_queue.due = true;
        }
        true
    }

    /// The box stayed empty: nothing landed. The batch stays at the front
    /// of its queue (same ids) with one more attempt, typed again after a
    /// fresh SETTLE, or held after [`MAX_SUBMIT_ATTEMPTS`].
    fn submit_retry(
        &mut self,
        terminal_id: &str,
        submit: PendingSubmit,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        self.end_submit(terminal_id, &submit);
        let mut held: Vec<QueuedMessage> = Vec::new();
        for entry in &mut self.message_queue.entries {
            if !submit.ids.iter().any(|id| id == entry.id()) {
                continue;
            }
            entry.attempts = entry.attempts.saturating_add(1);
            if entry.attempts == MAX_SUBMIT_ATTEMPTS {
                held.push(entry.clone());
            }
        }
        self.message_queue.save();
        self.message_queue
            .ready_since
            .insert(terminal_id.to_string(), now);
        next.push(now + SETTLE);
        tracing::info!(
            target = %terminal_id,
            messages = submit.ids.len(),
            held = !held.is_empty(),
            "message queue: typed text never showed in an empty input box; no Enter"
        );
        for entry in held {
            let target_name = notice_target_name(&entry);
            let text = format!(
                "herdr: your message {} to {target_name} ({}) was typed in {MAX_SUBMIT_ATTEMPTS} times but never showed in its input box, so herdr pressed no Enter. It stays queued and is not typed in again; it expires {} after you sent it. Send it again later if it still matters; check it with agents_messages.",
                entry.id(),
                entry.message.to_pane,
                minutes(Duration::from_secs(super::MESSAGE_TTL_S)),
            );
            self.send_stuck_notice(&entry, text, now_unix);
        }
        true
    }

    /// No proof the text landed: no Enter, `unconfirmed`, never retried;
    /// its senders and the user are told. Its text may still render.
    fn submit_unconfirmed(
        &mut self,
        terminal_id: &str,
        submit: PendingSubmit,
        why: &str,
        now: Instant,
        now_unix: u64,
    ) {
        self.end_submit(terminal_id, &submit);
        let entries = self.message_queue.entries_with(&submit.ids);
        self.message_queue
            .finish(&submit.ids, OUTCOME_UNCONFIRMED, now_unix);
        self.message_queue.ready_since.remove(terminal_id);
        tracing::info!(
            target = %terminal_id,
            messages = submit.ids.len(),
            why,
            "message queue: typed text not confirmed in the input box; no Enter"
        );
        for entry in entries.iter().filter(|entry| has_sender(entry)) {
            let target_name = notice_target_name(entry);
            let text = format!(
                "herdr: your message {} to {target_name} ({}) was typed in but herdr pressed no Enter: {why}. It may sit unsent in their input box; herdr does not type it again. Send it again (a new message) if it still matters.",
                entry.id(),
                entry.message.to_pane,
            );
            self.send_stuck_notice(entry, text, now_unix);
        }
        self.unconfirmed_card(
            terminal_id,
            &entries,
            &format!("herdr typed it but pressed no Enter: {why}."),
        );
        self.message_queue.delivery.late.insert(
            terminal_id.to_string(),
            LateSubmit {
                submit,
                entries,
                until: now + LATE_RENDER_WINDOW,
                next_poll: now + LATE_RENDER_POLL,
            },
        );
    }

    /// An unconfirmed batch: its box showing exactly what herdr typed (and
    /// nothing the user typed since) gets the Enter alone.
    fn poll_late_submit(
        &mut self,
        terminal_id: &str,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let Some(late) = self.message_queue.delivery.late.get(terminal_id).cloned() else {
            return false;
        };
        if now >= late.until || self.message_queue.delivery.submitting(terminal_id) {
            self.message_queue.delivery.late.remove(terminal_id);
            return false;
        }
        if now < late.next_poll {
            next.push(late.next_poll);
            return false;
        }
        let ours = match self.look_at_box(terminal_id, &late.submit) {
            None => {
                self.message_queue.delivery.late.remove(terminal_id);
                return false;
            }
            Some(BoxLook {
                user_typed: true, ..
            }) => {
                self.message_queue.delivery.late.remove(terminal_id);
                return false;
            }
            Some(BoxLook {
                prompt: Some(PromptBox::Draft { typed, .. }),
                ..
            }) => typing_guard::draft_is(&typed, &late.submit.expected, late.submit.paste),
            Some(_) => false,
        };
        if !ours {
            let poll = now + LATE_RENDER_POLL;
            if let Some(late) = self.message_queue.delivery.late.get_mut(terminal_id) {
                late.next_poll = poll;
            }
            next.push(poll);
            return false;
        }
        self.message_queue.delivery.late.remove(terminal_id);
        if !self.write_submit_enter(terminal_id, &late.submit) {
            return false;
        }
        if !late.submit.paste {
            self.remember_delivered(terminal_id, &late.submit.items);
        }
        for entry in &late.entries {
            self.message_queue.log(AgentMessage::update(
                &entry.message,
                OUTCOME_DELIVERED,
                now_unix,
            ));
        }
        tracing::info!(
            target = %terminal_id,
            messages = late.entries.len(),
            "message queue: an unconfirmed message's text showed late; Enter pressed"
        );
        self.watch_confirmation(terminal_id, late.entries, late.submit.paste, now);
        true
    }

    /// Delivered `entries` wait for their target to take them in.
    pub(super) fn watch_confirmation(
        &mut self,
        terminal_id: &str,
        entries: Vec<QueuedMessage>,
        paste: bool,
        now: Instant,
    ) {
        if entries.is_empty() || !self.confirms_delivery() {
            return;
        }
        let within = if paste {
            CONFIRM_TURN_WITHIN
        } else {
            CONFIRM_READ_WITHIN
        };
        let watches = &mut self.message_queue.delivery.confirm;
        watches.push(ConfirmWatch {
            terminal_id: terminal_id.to_string(),
            entries,
            paste,
            deadline: now + within,
        });
        if watches.len() > CONFIRM_KEEP {
            let excess = watches.len() - CONFIRM_KEEP;
            watches.drain(..excess);
        }
    }

    /// The agent read message `id` by id (`agents.read_messages`, its
    /// first read): a pointer line's confirmation.
    pub(crate) fn note_message_read(&mut self, id: &str) {
        let now_unix = crate::coordinator::now_unix();
        let mut confirmed: Vec<AgentMessage> = Vec::new();
        for watch in &mut self.message_queue.delivery.confirm {
            if watch.paste {
                continue;
            }
            if let Some(at) = watch.entries.iter().position(|entry| entry.id() == id) {
                confirmed.push(watch.entries.remove(at).message);
            }
        }
        self.message_queue
            .delivery
            .confirm
            .retain(|watch| !watch.entries.is_empty());
        let unconfirmed = &mut self.message_queue.delivery.unconfirmed;
        if let Some(at) = unconfirmed
            .iter()
            .position(|message| message.id.as_deref() == Some(id))
        {
            confirmed.push(unconfirmed.remove(at));
        }
        for message in confirmed {
            self.message_queue
                .log(AgentMessage::update(&message, OUTCOME_CONFIRMED, now_unix));
        }
    }

    /// The agent in `terminal_id` started a turn (its hooks' turn start, or
    /// a working edge): a paste's confirmation.
    pub(crate) fn note_submitted_turn(&mut self, terminal_id: &str) {
        if self.message_queue.delivery.confirm.is_empty() {
            return;
        }
        let now_unix = crate::coordinator::now_unix();
        let mut confirmed: Vec<AgentMessage> = Vec::new();
        self.message_queue.delivery.confirm.retain(|watch| {
            if watch.paste && watch.terminal_id == terminal_id {
                confirmed.extend(watch.entries.iter().map(|entry| entry.message.clone()));
                false
            } else {
                true
            }
        });
        for message in confirmed {
            self.message_queue
                .log(AgentMessage::update(&message, OUTCOME_CONFIRMED, now_unix));
        }
    }

    /// Confirmations past their window: `unconfirmed`, a herdr notice to
    /// each sender and one agent card per delivery. Nothing is redelivered.
    fn expire_confirmations(
        &mut self,
        now: Instant,
        now_unix: u64,
        next: &mut Vec<Instant>,
    ) -> bool {
        let mut expired: Vec<ConfirmWatch> = Vec::new();
        self.message_queue.delivery.confirm.retain(|watch| {
            if now >= watch.deadline {
                expired.push(watch.clone());
                false
            } else {
                next.push(watch.deadline);
                true
            }
        });
        if expired.is_empty() {
            return false;
        }
        for watch in expired {
            let what = if watch.paste {
                format!(
                    "no turn started within {}s after it went in",
                    CONFIRM_TURN_WITHIN.as_secs()
                )
            } else {
                format!(
                    "it was not read by id (agents_messages) within {}s after it went in",
                    CONFIRM_READ_WITHIN.as_secs()
                )
            };
            for entry in &watch.entries {
                self.message_queue.log(AgentMessage::update(
                    &entry.message,
                    OUTCOME_UNCONFIRMED,
                    now_unix,
                ));
                if !watch.paste {
                    let unconfirmed = &mut self.message_queue.delivery.unconfirmed;
                    unconfirmed.push(entry.message.clone());
                    if unconfirmed.len() > UNCONFIRMED_KEEP {
                        unconfirmed.remove(0);
                    }
                }
                if has_sender(entry) {
                    let target_name = notice_target_name(entry);
                    let text = format!(
                        "herdr: your message {} to {target_name} ({}) went in, but {what}, so herdr cannot confirm {target_name} got it. herdr does not resend it; send it again (a new message) if it matters.",
                        entry.id(),
                        entry.message.to_pane,
                    );
                    tracing::info!(
                        message = %entry.id(),
                        target = %entry.message.to_pane,
                        paste = watch.paste,
                        "message queue: a delivered message was not confirmed; telling its sender"
                    );
                    self.send_stuck_notice(entry, text, now_unix);
                }
            }
            self.unconfirmed_card(
                &watch.terminal_id,
                &watch.entries,
                &format!("It went in, but {what}."),
            );
        }
        true
    }

    /// One agent card from the target's pane: messages from other agents
    /// may not have reached it. Not for herdr's own notices.
    fn unconfirmed_card(&mut self, terminal_id: &str, entries: &[QueuedMessage], body: &str) {
        let from_agents = entries.iter().filter(|entry| has_sender(entry)).count();
        if from_agents == 0 {
            return;
        }
        let Some(target) = self.message_target(terminal_id, None) else {
            return;
        };
        let Some(pane) = self.public_pane_id(target.ws_idx, target.pane_id) else {
            return;
        };
        let Some(sender) = self.notice_sender(&pane) else {
            return;
        };
        let title = crate::app::agent_notices::sanitize_title(&format!(
            "{from_agents} agent message{} may not have reached it",
            if from_agents == 1 { "" } else { "s" },
        ))
        .unwrap_or_default();
        let body = crate::app::agent_notices::sanitize_body(&format!(
            "{body} Check its input box; the sender was told."
        ));
        match self.agent_notices.notify(
            sender,
            crate::api::schema::AgentNoticeKind::Warning,
            title,
            body,
            crate::coordinator::now_unix(),
        ) {
            Ok((_, outcome)) => {
                if outcome == crate::api::schema::AgentNotifyOutcome::Shown {
                    self.render_dirty.request_generic();
                    self.render_notify.notify_one();
                }
            }
            Err(refused) => {
                tracing::debug!(code = %refused.code, "message queue: the unconfirmed card was not shown");
            }
        }
    }
}

#[cfg(test)]
mod tests;
