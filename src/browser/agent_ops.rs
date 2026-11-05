//! Fork (page API): which companion pages an agent is driving right now.
//! An agent's click is trusted input and its eval runs in the page, so a
//! page message that lands while an agent op is in flight on that page,
//! or within [`AGENT_DRIVEN_GRACE`] after it, is not the user's click:
//! `browser.page_message` drops it (`agent_driving`). Runtime only, keyed
//! by CDP target id; pure data (the hub holds it behind a mutex).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long after an agent op ends its page still counts as driven.
pub const AGENT_DRIVEN_GRACE: Duration = Duration::from_millis(1_000);
/// The most targets kept (ended marks past the grace go first).
const MAX_TARGETS: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
struct Mark {
    inflight: u32,
    ended: Option<Instant>,
}

/// In-flight agent ops and their end times, per target.
#[derive(Debug, Default)]
pub struct AgentOps {
    marks: HashMap<String, Mark>,
}

impl AgentOps {
    /// An agent op on `target` starts.
    pub fn start(&mut self, target: &str, now: Instant) {
        self.prune(now);
        self.marks.entry(target.to_string()).or_default().inflight += 1;
    }

    /// An agent op on `target` ended (successfully or not).
    pub fn end(&mut self, target: &str, now: Instant) {
        let mark = self.marks.entry(target.to_string()).or_default();
        mark.inflight = mark.inflight.saturating_sub(1);
        mark.ended = Some(now);
    }

    /// An op that produced `target` (show, open) just ended: its page
    /// counts as driven for the grace.
    pub fn touched(&mut self, target: &str, now: Instant) {
        self.prune(now);
        self.marks.entry(target.to_string()).or_default().ended = Some(now);
    }

    /// Whether an agent drives `target`: an op in flight, or one that
    /// ended within the grace.
    pub fn driving(&self, target: &str, now: Instant) -> bool {
        self.marks.get(target).is_some_and(|mark| {
            mark.inflight > 0
                || mark
                    .ended
                    .is_some_and(|ended| now.saturating_duration_since(ended) < AGENT_DRIVEN_GRACE)
        })
    }

    fn prune(&mut self, now: Instant) {
        if self.marks.len() < MAX_TARGETS {
            return;
        }
        self.marks.retain(|_, mark| {
            mark.inflight > 0
                || mark
                    .ended
                    .is_some_and(|ended| now.saturating_duration_since(ended) < AGENT_DRIVEN_GRACE)
        });
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.marks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_driven_while_an_op_runs_and_for_the_grace_after() {
        let mut ops = AgentOps::default();
        let t0 = Instant::now();
        assert!(!ops.driving("T1", t0));
        ops.start("T1", t0);
        ops.start("T1", t0);
        assert!(ops.driving("T1", t0 + Duration::from_secs(60)), "in flight");
        ops.end("T1", t0 + Duration::from_secs(1));
        assert!(
            ops.driving("T1", t0 + Duration::from_secs(5)),
            "one still in flight"
        );
        let end = t0 + Duration::from_secs(6);
        ops.end("T1", end);
        assert!(ops.driving("T1", end + Duration::from_millis(999)));
        assert!(!ops.driving("T1", end + AGENT_DRIVEN_GRACE));
        assert!(!ops.driving("T2", end), "another page is not driven");
        ops.touched("T3", end);
        assert!(ops.driving("T3", end + Duration::from_millis(10)));
    }

    #[test]
    fn ended_marks_are_pruned_once_the_map_is_full() {
        let mut ops = AgentOps::default();
        let t0 = Instant::now();
        for n in 0..MAX_TARGETS {
            ops.touched(&format!("T{n}"), t0);
        }
        ops.start("live", t0);
        let later = t0 + AGENT_DRIVEN_GRACE * 2;
        ops.touched("new", later);
        assert_eq!(ops.len(), 2, "only the live op and the new mark stay");
        assert!(ops.driving("live", later));
    }
}
