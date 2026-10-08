//! The agents model's limits (U7): fixed built-in values, deliberately
//! loose. They catch runaway loops, never normal use (a lead driving fifteen
//! members, a fan-out of ten workers). No config keys.
//!
//! Tests that pin a number import it from here.

/// One delivered message per sender→target pair per this many seconds (a
/// reply to a message sent to the replier is exempt).
pub const PAIR_GAP_S: u64 = 2;
/// Delivered messages per sender per hour.
pub const SENDER_PER_HOUR: usize = 200;
/// More than `LOOP_MAX` delivered messages between one pair (both
/// directions) inside the window is a loop.
pub const LOOP_WINDOW_S: u64 = 600;
pub const LOOP_MAX: usize = 30;
/// Agent-opened tabs and reopens outside a user turn, per caller per hour.
pub const SPAWNS_PER_HOUR: usize = 20;
/// Live agent-opened panes (`opened_by: Agent`) per team.
pub const TEAM_AGENT_SPAWNED_MAX: usize = 40;
/// Soft edits (rename, meta, notes, cosmetic, ...) per caller per hour.
pub const SOFT_EDITS_PER_HOUR: usize = 300;
/// One urgent message (it may interrupt the target's turn) per
/// sender→target pair per this many seconds.
pub const URGENT_GAP_S: u64 = 5 * 60;

use std::collections::{HashMap, VecDeque};

use crate::terminal::TerminalId;

/// Why a message was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitRefusal {
    /// `retry_s` seconds until the pair gap passes.
    PairGap {
        retry_s: u64,
    },
    SenderHourly,
    LoopGuard,
    /// `retry_s` seconds until another urgent message to the same agent.
    UrgentGap {
        retry_s: u64,
    },
}

impl LimitRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::PairGap { .. } | Self::SenderHourly | Self::UrgentGap { .. } => {
                crate::api::schema::agents_model::error_code::RATE_LIMITED
            }
            Self::LoopGuard => crate::api::schema::agents_model::error_code::LOOP_GUARD,
        }
    }

    pub fn hint(&self) -> String {
        match self {
            Self::PairGap { retry_s } => format!(
                "one message per {PAIR_GAP_S}s to the same agent; retry in {retry_s}s"
            ),
            Self::SenderHourly => format!("at most {SENDER_PER_HOUR} messages per hour"),
            Self::UrgentGap { retry_s } => format!(
                "one urgent message per {} minutes to the same agent; send it without urgent, or retry in {retry_s}s",
                URGENT_GAP_S / 60
            ),
            Self::LoopGuard => format!(
                "more than {LOOP_MAX} messages with this agent in {} minutes; this looks like a loop; stop and ask your user",
                LOOP_WINDOW_S / 60
            ),
        }
    }
}

/// The in-memory budgets, keyed by terminal (stable across moves; reset by
/// a restart or a handoff).
#[derive(Debug, Default)]
pub struct Limiter {
    /// Delivered messages: (sender, target, unix), oldest first, the last
    /// hour.
    messages: VecDeque<(TerminalId, TerminalId, u64)>,
    /// Non-user-turn spawns per caller (unix times, the last hour).
    spawns: HashMap<TerminalId, VecDeque<u64>>,
    /// Soft edits per caller (unix times, the last hour).
    soft_edits: HashMap<TerminalId, VecDeque<u64>>,
    /// The last urgent message per sender→target pair (unix).
    urgent: HashMap<(TerminalId, TerminalId), u64>,
}

const HOUR_S: u64 = 3600;

fn prune_hour(times: &mut VecDeque<u64>, now: u64) {
    while times
        .front()
        .is_some_and(|at| now.saturating_sub(*at) >= HOUR_S)
    {
        times.pop_front();
    }
}

impl Limiter {
    fn prune_messages(&mut self, now: u64) {
        while self
            .messages
            .front()
            .is_some_and(|(_, _, at)| now.saturating_sub(*at) >= HOUR_S.max(LOOP_WINDOW_S))
        {
            self.messages.pop_front();
        }
    }

    /// Whether `from` may message `to` now. A reply is exempt from the pair
    /// gap but counts for the loop guard and the hourly cap.
    pub fn check_message(
        &mut self,
        from: &TerminalId,
        to: &TerminalId,
        reply: bool,
        now: u64,
    ) -> Result<(), LimitRefusal> {
        self.prune_messages(now);
        if !reply {
            let last = self
                .messages
                .iter()
                .rev()
                .find(|(sender, target, _)| sender == from && target == to)
                .map(|(_, _, at)| *at);
            if let Some(at) = last {
                let since = now.saturating_sub(at);
                if since < PAIR_GAP_S {
                    return Err(LimitRefusal::PairGap {
                        retry_s: PAIR_GAP_S - since,
                    });
                }
            }
        }
        let hourly = self
            .messages
            .iter()
            .filter(|(sender, _, at)| sender == from && now.saturating_sub(*at) < HOUR_S)
            .count();
        if hourly >= SENDER_PER_HOUR {
            return Err(LimitRefusal::SenderHourly);
        }
        let pair = self
            .messages
            .iter()
            .filter(|(sender, target, at)| {
                now.saturating_sub(*at) < LOOP_WINDOW_S
                    && ((sender == from && target == to) || (sender == to && target == from))
            })
            .count();
        if pair >= LOOP_MAX {
            return Err(LimitRefusal::LoopGuard);
        }
        Ok(())
    }

    /// Count a delivered message.
    pub fn record_message(&mut self, from: &TerminalId, to: &TerminalId, now: u64) {
        self.messages.push_back((from.clone(), to.clone(), now));
    }

    /// Whether `from` may send `to` an urgent message now.
    pub fn check_urgent(
        &self,
        from: &TerminalId,
        to: &TerminalId,
        now: u64,
    ) -> Result<(), LimitRefusal> {
        match self.urgent.get(&(from.clone(), to.clone())) {
            Some(at) if now.saturating_sub(*at) < URGENT_GAP_S => Err(LimitRefusal::UrgentGap {
                retry_s: URGENT_GAP_S - now.saturating_sub(*at),
            }),
            _ => Ok(()),
        }
    }

    /// Count an accepted urgent message.
    pub fn record_urgent(&mut self, from: &TerminalId, to: &TerminalId, now: u64) {
        self.urgent
            .retain(|_, at| now.saturating_sub(*at) < URGENT_GAP_S);
        self.urgent.insert((from.clone(), to.clone()), now);
    }

    /// Whether `caller` may spawn once more outside a user turn.
    pub fn check_spawn(&mut self, caller: &TerminalId, now: u64) -> bool {
        let times = self.spawns.entry(caller.clone()).or_default();
        prune_hour(times, now);
        times.len() < SPAWNS_PER_HOUR
    }

    pub fn record_spawn(&mut self, caller: &TerminalId, now: u64) {
        self.spawns
            .entry(caller.clone())
            .or_default()
            .push_back(now);
    }

    /// Release a spawn that did not happen (a failed open).
    pub fn release_spawn(&mut self, caller: &TerminalId) {
        if let Some(times) = self.spawns.get_mut(caller) {
            times.pop_back();
        }
    }

    /// Count a soft edit; `false` when the hourly cap is reached.
    pub fn take_soft_edit(&mut self, caller: &TerminalId, now: u64) -> bool {
        let times = self.soft_edits.entry(caller.clone()).or_default();
        prune_hour(times, now);
        if times.len() >= SOFT_EDITS_PER_HOUR {
            return false;
        }
        times.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_urgent_message_per_pair_per_five_minutes() {
        let (a, b, c) = (
            TerminalId::alloc(),
            TerminalId::alloc(),
            TerminalId::alloc(),
        );
        let mut limiter = Limiter::default();
        assert!(limiter.check_urgent(&a, &b, 100).is_ok());
        limiter.record_urgent(&a, &b, 100);
        assert_eq!(
            limiter.check_urgent(&a, &b, 160),
            Err(LimitRefusal::UrgentGap {
                retry_s: URGENT_GAP_S - 60
            })
        );
        assert_eq!(
            LimitRefusal::UrgentGap { retry_s: 1 }.code(),
            crate::api::schema::agents_model::error_code::RATE_LIMITED
        );
        assert!(limiter.check_urgent(&a, &c, 160).is_ok(), "another target");
        assert!(limiter.check_urgent(&b, &a, 160).is_ok(), "the other way");
        assert!(limiter.check_urgent(&a, &b, 100 + URGENT_GAP_S).is_ok());
    }

    #[test]
    fn the_pair_gap_and_a_reply_exemption() {
        let (a, b) = (TerminalId::alloc(), TerminalId::alloc());
        let mut limiter = Limiter::default();
        assert!(limiter.check_message(&a, &b, false, 100).is_ok());
        limiter.record_message(&a, &b, 100);
        assert_eq!(
            limiter.check_message(&a, &b, false, 100),
            Err(LimitRefusal::PairGap {
                retry_s: PAIR_GAP_S
            })
        );
        assert!(limiter.check_message(&a, &b, true, 100).is_ok());
        assert!(limiter
            .check_message(&a, &b, false, 100 + PAIR_GAP_S)
            .is_ok());
    }

    #[test]
    fn a_lead_fans_out_to_fifteen_members_twice_without_a_refusal() {
        let lead = TerminalId::alloc();
        let members: Vec<TerminalId> = (0..15).map(|_| TerminalId::alloc()).collect();
        let mut limiter = Limiter::default();
        let mut now = 1_000;
        for _ in 0..2 {
            for member in &members {
                assert!(limiter.check_message(&lead, member, false, now).is_ok());
                limiter.record_message(&lead, member, now);
                // and each member answers
                assert!(limiter.check_message(member, &lead, true, now).is_ok());
                limiter.record_message(member, &lead, now);
            }
            now += PAIR_GAP_S;
        }
    }

    #[test]
    fn the_loop_guard_and_the_hourly_cap() {
        let (a, b) = (TerminalId::alloc(), TerminalId::alloc());
        let mut limiter = Limiter::default();
        let mut now = 1_000;
        for i in 0..LOOP_MAX {
            let (from, to) = if i % 2 == 0 { (&a, &b) } else { (&b, &a) };
            limiter.record_message(from, to, now);
            now += 1;
        }
        assert_eq!(
            limiter.check_message(&a, &b, true, now),
            Err(LimitRefusal::LoopGuard)
        );
        let mut limiter = Limiter::default();
        let sender = TerminalId::alloc();
        for _ in 0..SENDER_PER_HOUR {
            limiter.record_message(&sender, &TerminalId::alloc(), 5_000);
        }
        assert_eq!(
            limiter.check_message(&sender, &a, false, 5_001),
            Err(LimitRefusal::SenderHourly)
        );
        assert!(limiter
            .check_message(&sender, &a, false, 5_000 + 3_600)
            .is_ok());
    }

    #[test]
    fn spawns_and_soft_edits_are_capped_per_hour() {
        let caller = TerminalId::alloc();
        let mut limiter = Limiter::default();
        for _ in 0..SPAWNS_PER_HOUR {
            assert!(limiter.check_spawn(&caller, 10));
            limiter.record_spawn(&caller, 10);
        }
        assert!(!limiter.check_spawn(&caller, 10));
        limiter.release_spawn(&caller);
        assert!(limiter.check_spawn(&caller, 10));
        assert!(limiter.check_spawn(&caller, 10 + 3_600));
        for _ in 0..SOFT_EDITS_PER_HOUR {
            assert!(limiter.take_soft_edit(&caller, 10));
        }
        assert!(!limiter.take_soft_edit(&caller, 10));
    }
}
