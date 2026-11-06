//! Fork (page API): which companion pages an agent is driving right now.
//! An agent's click is trusted input and its eval runs in the page, so a
//! page message that lands while an agent op is in flight on that page,
//! or within [`AGENT_DRIVEN_GRACE`] after it, is not the user's click:
//! `browser.page_message` drops it (`agent_driving`). Runtime only, keyed
//! by CDP target id; pure data (the hub holds it behind a mutex).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long after an agent op ends its page still counts as driven: above
/// Chromium's transient user activation (about 5 s), so a page script an
/// agent's click or eval started (a `setTimeout` send) is still dropped.
pub const AGENT_DRIVEN_GRACE: Duration = Duration::from_millis(6_000);
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

/// The most panes and terminals an [`Openers`] book keeps (it starts over
/// past it: every page shown before then fails closed).
const MAX_OPENERS: usize = 4_096;

/// Fork (page API): who opened a page, as an identity that changes when
/// the agent does. A token is `<boot>:<terminal id>:<run>`: `boot` is new
/// with every server process (a restart or live handoff), the terminal id
/// changes when the pane gets a new terminal, and `run` grows whenever
/// detection sees the terminal's agent go or change (a restart, another
/// agent in the pane). A page stores its opener's token at show; a page
/// message is refused unless the pane's current token is the same, so it
/// fails closed.
#[derive(Debug)]
pub struct Openers {
    boot: String,
    runs: HashMap<String, u64>,
    /// pane id -> its terminal id, as the last browser call resolved it.
    panes: HashMap<String, String>,
}

impl Openers {
    pub fn new(boot: String) -> Self {
        Self {
            boot,
            runs: HashMap::new(),
            panes: HashMap::new(),
        }
    }

    /// The current opener token of `terminal_id`.
    pub fn token(&self, terminal_id: &str) -> String {
        let run = self.runs.get(terminal_id).copied().unwrap_or(0);
        format!("{}:{terminal_id}:{run}", self.boot)
    }

    /// The terminal's agent went or changed: every page its agent showed
    /// stops sending.
    pub fn bump(&mut self, terminal_id: &str) {
        if self.runs.len() >= MAX_OPENERS && !self.runs.contains_key(terminal_id) {
            // starting over must not hand a stale page a matching token
            self.boot = format!("{}+", self.boot);
            self.runs.clear();
        }
        *self.runs.entry(terminal_id.to_string()).or_insert(0) += 1;
    }

    /// `pane_id` runs in `terminal_id` (an agent's browser call resolved it).
    pub fn note_pane(&mut self, pane_id: &str, terminal_id: &str) {
        if self.panes.len() >= MAX_OPENERS && !self.panes.contains_key(pane_id) {
            self.panes.clear();
        }
        self.panes
            .insert(pane_id.to_string(), terminal_id.to_string());
    }

    /// The opener token of the agent in `pane_id` now, when known.
    pub fn pane_token(&self, pane_id: &str) -> Option<String> {
        self.panes.get(pane_id).map(|terminal| self.token(terminal))
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
        assert!(
            ops.driving("T1", end + Duration::from_millis(1_500)),
            "an eval's setTimeout(1500) send is still the agent's"
        );
        assert!(ops.driving("T1", end + Duration::from_millis(5_900)));
        assert!(!ops.driving("T1", end + AGENT_DRIVEN_GRACE));
        assert!(!ops.driving("T2", end), "another page is not driven");
        ops.touched("T3", end);
        assert!(ops.driving("T3", end + Duration::from_millis(10)));
    }

    #[test]
    fn an_opener_token_changes_with_the_agent_terminal_and_server() {
        let mut book = Openers::new("boot1".into());
        book.note_pane("w1:p2", "t-7");
        let shown = book.pane_token("w1:p2").unwrap();
        assert_eq!(shown, book.token("t-7"), "the same agent");
        book.bump("t-7");
        assert_ne!(book.token("t-7"), shown, "its agent restarted or changed");
        assert_ne!(book.token("t-8"), shown, "another terminal");
        assert_ne!(
            Openers::new("boot2".into()).token("t-7"),
            shown,
            "another server"
        );
        assert!(book.pane_token("w9:p9").is_none());
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
