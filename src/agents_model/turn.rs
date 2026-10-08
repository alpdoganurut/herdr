//! A pane's turn origin (design §3): did the current turn start from the
//! pane's user, another agent's message, a herdr wake-up, a scripted prompt,
//! or the agent itself? Runtime only, never persisted: a restored pane
//! starts `Unknown` (fail closed).
//!
//! Inputs are recorded next to every write (`App::note_input`) in O(1) with
//! no allocation for client input; status edges come from the pane state
//! update path, also O(1).

use std::time::{Duration, Instant};

use super::{InputSource, Programmatic, TurnOrigin};
use crate::api::schema::agents_model::{AgentsOriginDetail, AgentsTurnInfo, AgentsTurnOrigin};

/// A screen-detected idle shorter than this between two working phases is
/// a flap between tool calls: the turn carries over (`bridged`).
pub const BRIDGE: Duration = Duration::from_secs(5);
/// How long after a hook-reported turn start (or the idle edge after it)
/// message delivery trusts the hooks over a screen that reads idle
/// ([`TurnState::hook_turn_live_until`]).
pub const HOOK_LIVE_HOLD: Duration = Duration::from_secs(15);
/// A user submit counts for the next turn only this long.
pub const USER_SUBMIT_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The coarse agent status a status edge is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStatus {
    /// No agent (a shell), or not known yet.
    Unknown,
    Idle,
    Working,
    Blocked,
}

impl From<crate::detect::AgentState> for EdgeStatus {
    fn from(state: crate::detect::AgentState) -> Self {
        match state {
            crate::detect::AgentState::Idle => Self::Idle,
            crate::detect::AgentState::Working => Self::Working,
            crate::detect::AgentState::Blocked => Self::Blocked,
            crate::detect::AgentState::Unknown => Self::Unknown,
        }
    }
}

/// What a status edge did to the turn ([`TurnState::on_status_edge`]); the
/// automatic checkpoints (`crate::notes::auto`) read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEdge {
    /// Nothing for the turn.
    None,
    /// A new turn started; `user`: from the pane's own user.
    Started { user: bool },
    /// A short idle flap carried the turn over.
    Bridged,
    /// The turn ended (working or blocked to idle or unknown).
    Ended,
}

/// What landed in the pane since the last idle edge.
#[derive(Debug, Clone, Default)]
struct InputProvenance {
    last_client_input: Option<Instant>,
    last_client_submit: Option<Instant>,
    /// Any client keystroke since the last idle edge.
    client_input_since_idle: bool,
    /// A client submit since the last idle edge (user evidence).
    client_submit_since_idle: bool,
    /// The submit came through a terminal-attach session.
    client_attach_since_idle: bool,
    /// The first programmatic write since the last idle edge.
    programmatic_since_idle: Option<Programmatic>,
    /// The last working→idle edge.
    last_idle_at: Option<Instant>,
    /// That edge came from a hook-authoritative status.
    last_idle_hook: bool,
    /// The last programmatic write (a herdr message, a wake-up, a script),
    /// whatever the status did since.
    last_programmatic: Option<Instant>,
}

/// A turn start or end the agent's own hooks reported (`pane.report_turn`:
/// Claude's UserPromptSubmit and Stop).
#[derive(Debug, Clone)]
struct HookMark {
    /// The agent's prompt id (Claude's `prompt_id`), when it sent one.
    prompt: Option<String>,
    /// The hook's own clock (ns), to order a start and an end whose reports
    /// crossed on the socket.
    seq: u64,
    at: Instant,
}

/// The current turn.
#[derive(Debug, Clone, Default)]
struct TurnRecord {
    origin: TurnOrigin,
    started_unix: u64,
    /// The programmatic write that landed in a user turn (§3.4).
    poison: Option<TurnOrigin>,
    bridged: bool,
    attach: bool,
}

/// The effective turn, as the policy and the MCP header see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveTurn {
    pub origin: TurnOrigin,
    pub poisoned: bool,
    pub bridged: bool,
    pub attach: bool,
    pub started_unix: u64,
}

impl EffectiveTurn {
    /// The origin is the user's and nothing programmatic landed since.
    pub fn user_turn(&self) -> bool {
        self.origin == TurnOrigin::User && !self.poisoned
    }

    /// "a teammate's message", "a herdr wake-up", ... for refusal hints.
    pub fn describe(&self) -> &'static str {
        match self.origin {
            TurnOrigin::User if self.poisoned => "a scripted prompt",
            TurnOrigin::User => "your user",
            TurnOrigin::AgentMessage { .. } => "another agent's message",
            TurnOrigin::HerdrWake { .. } => "a herdr wake-up",
            TurnOrigin::Programmatic => "a scripted prompt",
            TurnOrigin::SelfStarted => "no input (the agent started it)",
            TurnOrigin::Unknown => "an unknown source (no turn seen since herdr started, restarted or reopened the tab)",
        }
    }

    /// The wire form; `pane_of` names a sender terminal's public pane.
    pub fn info(
        &self,
        pane_of: impl Fn(&crate::terminal::TerminalId) -> Option<String>,
    ) -> AgentsTurnInfo {
        let mut info = AgentsTurnInfo {
            origin: wire_origin(&self.origin),
            user_turn: self.user_turn(),
            poisoned: self.poisoned,
            started_unix: (self.started_unix > 0).then_some(self.started_unix),
            ..AgentsTurnInfo::default()
        };
        info.detail = if self.bridged {
            Some(AgentsOriginDetail::Bridged)
        } else if self.attach {
            Some(AgentsOriginDetail::ClientAttach)
        } else {
            None
        };
        match &self.origin {
            TurnOrigin::AgentMessage { id, from } => {
                info.message_id = Some(id.clone());
                info.from_pane = pane_of(from);
            }
            TurnOrigin::HerdrWake { seq } => info.wake_seq = Some(*seq),
            _ => {}
        }
        info
    }
}

/// The wire origin of an internal one.
pub fn wire_origin(origin: &TurnOrigin) -> AgentsTurnOrigin {
    match origin {
        TurnOrigin::User => AgentsTurnOrigin::User,
        TurnOrigin::AgentMessage { .. } => AgentsTurnOrigin::AgentMessage,
        TurnOrigin::HerdrWake { .. } => AgentsTurnOrigin::HerdrWake,
        TurnOrigin::Programmatic => AgentsTurnOrigin::Programmatic,
        TurnOrigin::SelfStarted => AgentsTurnOrigin::SelfStarted,
        TurnOrigin::Unknown => AgentsTurnOrigin::Unknown,
    }
}

/// A pane's turn tracker: on `TerminalState`, runtime only.
#[derive(Debug, Clone, Default)]
pub struct TurnState {
    prov: InputProvenance,
    record: TurnRecord,
    /// The pane came from a restore or a live handoff: its first move out
    /// of `unknown` does not capture. Cleared at its first idle edge.
    restored: bool,
    /// Between a capturing edge and the next idle edge.
    in_turn: bool,
    /// The last turn start and end the agent's hooks reported (message
    /// delivery only; the displayed status never reads them). `None` until
    /// a report arrives, so after a restart or a handoff.
    hook_start: Option<HookMark>,
    hook_end: Option<HookMark>,
}

impl TurnState {
    /// After a restore, a handoff or a reopen: `Unknown`, and the first edge
    /// out of `unknown` does not capture.
    pub fn restored() -> Self {
        Self {
            restored: true,
            ..Self::default()
        }
    }

    /// Record input written into the pane (§3.1).
    pub fn note_input(&mut self, source: &InputSource, now: Instant) {
        match source {
            InputSource::Client { submit, attach } => {
                self.prov.last_client_input = Some(now);
                self.prov.client_input_since_idle = true;
                if *submit {
                    self.prov.last_client_submit = Some(now);
                    self.prov.client_submit_since_idle = true;
                    if *attach {
                        self.prov.client_attach_since_idle = true;
                    }
                }
            }
            InputSource::Programmatic(programmatic) => {
                self.prov.last_programmatic = Some(now);
                if self.prov.programmatic_since_idle.is_none() {
                    self.prov.programmatic_since_idle = Some(programmatic.clone());
                }
                // §3.4: a programmatic write into a running user turn
                // poisons it for the rest of the turn.
                if self.in_turn
                    && self.record.origin == TurnOrigin::User
                    && self.record.poison.is_none()
                {
                    self.record.poison = Some(TurnOrigin::from(programmatic));
                }
            }
            InputSource::Internal => {}
        }
    }

    /// A status edge (§3.3). `hook`: the new status is hook-authoritative.
    pub fn on_status_edge(
        &mut self,
        previous: EdgeStatus,
        next: EdgeStatus,
        hook: bool,
        now: Instant,
        now_unix: u64,
    ) -> TurnEdge {
        if previous == next {
            return TurnEdge::None;
        }
        match (previous, next) {
            (EdgeStatus::Idle | EdgeStatus::Unknown, EdgeStatus::Working) if !self.restored => {
                self.capture(now, now_unix);
                if self.record.bridged {
                    TurnEdge::Bridged
                } else {
                    TurnEdge::Started {
                        user: self.record.origin == TurnOrigin::User,
                    }
                }
            }
            (EdgeStatus::Idle | EdgeStatus::Unknown, EdgeStatus::Working) => {
                // A restored, handed-off or reopened pane (it may start out
                // idle): its first turn is not captured (12b) until the
                // agent is next seen idle.
                self.record = TurnRecord::default();
                self.in_turn = true;
                self.clear_since_idle();
                TurnEdge::Started { user: false }
            }
            // blocked→working does not re-capture.
            (_, EdgeStatus::Working) => TurnEdge::None,
            (EdgeStatus::Working | EdgeStatus::Blocked, EdgeStatus::Idle | EdgeStatus::Unknown) => {
                self.idle_edge(next == EdgeStatus::Idle && hook, now);
                TurnEdge::Ended
            }
            (EdgeStatus::Unknown, EdgeStatus::Idle) => {
                // An agent came up idle: no turn ran.
                self.restored = false;
                self.in_turn = false;
                self.prov.last_idle_at = None;
                self.clear_since_idle();
                TurnEdge::None
            }
            _ => TurnEdge::None,
        }
    }

    fn idle_edge(&mut self, hook: bool, now: Instant) {
        self.prov.last_idle_at = Some(now);
        self.prov.last_idle_hook = hook;
        self.in_turn = false;
        self.restored = false;
        self.clear_since_idle();
    }

    fn clear_since_idle(&mut self) {
        self.prov.client_input_since_idle = false;
        self.prov.client_submit_since_idle = false;
        self.prov.client_attach_since_idle = false;
        self.prov.programmatic_since_idle = None;
    }

    fn capture(&mut self, now: Instant, now_unix: u64) {
        let nothing_since =
            !self.prov.client_input_since_idle && self.prov.programmatic_since_idle.is_none();
        let bridge = self
            .prov
            .last_idle_at
            .is_some_and(|at| now.saturating_duration_since(at) < BRIDGE)
            && !self.prov.last_idle_hook
            && nothing_since
            && self.record.origin != TurnOrigin::Unknown;
        if bridge {
            self.record.bridged = true;
        } else {
            let (origin, attach) = self.rule(now);
            self.record = TurnRecord {
                origin,
                started_unix: now_unix,
                poison: None,
                bridged: false,
                attach,
            };
        }
        self.in_turn = true;
        self.clear_since_idle();
    }

    /// §3.3 steps 2-4: the origin of a turn starting now.
    fn rule(&self, now: Instant) -> (TurnOrigin, bool) {
        if let Some(programmatic) = &self.prov.programmatic_since_idle {
            return (TurnOrigin::from(programmatic), false);
        }
        let recent_submit = self
            .prov
            .last_client_submit
            .is_some_and(|at| now.saturating_duration_since(at) < USER_SUBMIT_WINDOW);
        if self.prov.client_submit_since_idle && recent_submit {
            return (TurnOrigin::User, self.prov.client_attach_since_idle);
        }
        (TurnOrigin::SelfStarted, false)
    }

    /// The effective turn now (§3.5): while the agent shows idle and input
    /// arrived since the idle edge, the turn that input is about to start.
    pub fn effective(&self, status: EdgeStatus, now: Instant, now_unix: u64) -> EffectiveTurn {
        let pending =
            self.prov.client_submit_since_idle || self.prov.programmatic_since_idle.is_some();
        if status == EdgeStatus::Idle && pending && !self.restored {
            let (origin, attach) = self.rule(now);
            return EffectiveTurn {
                origin,
                poisoned: false,
                bridged: false,
                attach,
                started_unix: now_unix,
            };
        }
        match &self.record.poison {
            Some(poison) => EffectiveTurn {
                origin: poison.clone(),
                poisoned: true,
                bridged: self.record.bridged,
                attach: false,
                started_unix: self.record.started_unix,
            },
            None => EffectiveTurn {
                origin: self.record.origin.clone(),
                poisoned: false,
                bridged: self.record.bridged,
                attach: self.record.attach,
                started_unix: self.record.started_unix,
            },
        }
    }

    /// A turn start (`start`) or end the agent's own hooks reported.
    /// `seq` is the hook's clock; 0 (not sent) orders by arrival.
    pub fn note_hook_turn(&mut self, start: bool, prompt: Option<String>, seq: u64, now: Instant) {
        let seq = if seq == 0 {
            // Past every reported mark: the report arrived last.
            [&self.hook_start, &self.hook_end]
                .into_iter()
                .flatten()
                .map(|mark| mark.seq.saturating_add(1))
                .max()
                .unwrap_or(1)
        } else {
            seq
        };
        let mark = HookMark {
            prompt: prompt.filter(|prompt| !prompt.is_empty()),
            seq,
            at: now,
        };
        if start {
            self.hook_start = Some(mark);
        } else {
            self.hook_end = Some(mark);
        }
    }

    /// When the agent's own hooks say its last turn ended: the reported end
    /// belongs to the newest reported start (same prompt id, or a later hook
    /// clock), and nothing was submitted or typed in by herdr since. Claude
    /// keeps the title spinner after its turn while background agents run;
    /// this tells message delivery the turn is over whatever the screen
    /// shows. `None` when no end was reported, a newer start was, or input
    /// landed after the end (the turn it starts reports itself).
    pub fn hook_turn_ended(&self) -> Option<Instant> {
        let end = self.hook_end.as_ref()?;
        if self.hook_start_is_newest() {
            return None;
        }
        let input_since = |at: Option<Instant>| at.is_some_and(|at| at >= end.at);
        if input_since(self.prov.last_client_submit) || input_since(self.prov.last_programmatic) {
            return None;
        }
        Some(end.at)
    }

    /// Whether the newest reported mark is a start: no end was reported, or
    /// the reported end belongs to an older start (another prompt id and an
    /// earlier hook clock).
    fn hook_start_is_newest(&self) -> bool {
        let Some(start) = &self.hook_start else {
            return false;
        };
        let Some(end) = &self.hook_end else {
            return true;
        };
        let same_prompt = matches!(
            (&start.prompt, &end.prompt),
            (Some(started), Some(ended)) if started == ended
        );
        !same_prompt && end.seq <= start.seq
    }

    /// Until when the agent's own hooks say a turn is live although its
    /// screen may read idle: the newest reported mark is a start, nothing
    /// was submitted or typed in by herdr after it, and `now` is within
    /// [`HOOK_LIVE_HOLD`] of that start or of the last idle edge after it.
    /// Message delivery waits on it (Claude can hide its spinner while it
    /// streams under a background agent's notice). Bounded, because a turn
    /// ended with Esc reports no Stop. `None` otherwise.
    pub fn hook_turn_live_until(&self, now: Instant) -> Option<Instant> {
        if !self.hook_start_is_newest() {
            return None;
        }
        let start = self.hook_start.as_ref()?;
        let input_after = |at: Option<Instant>| at.is_some_and(|at| at > start.at);
        if input_after(self.prov.last_client_submit) || input_after(self.prov.last_programmatic) {
            return None;
        }
        let edge = self
            .prov
            .last_idle_at
            .filter(|idle| *idle > start.at)
            .unwrap_or(start.at);
        let until = edge + HOOK_LIVE_HOLD;
        (now < until).then_some(until)
    }

    /// Whether herdr wrote into the pane (a message, a wake-up, an urgent
    /// interrupt, a script) within `window` before `now`.
    pub fn programmatic_within(&self, window: Duration, now: Instant) -> bool {
        self.prov
            .last_programmatic
            .is_some_and(|at| now.saturating_duration_since(at) < window)
    }

    /// Forget the hook marks (the agent in the pane was replaced, went away
    /// or was suspended: its hooks no longer describe what runs there).
    pub fn clear_hook_marks(&mut self) {
        self.hook_start = None;
        self.hook_end = None;
    }

    /// The last client keystroke.
    pub fn last_client_input(&self) -> Option<Instant> {
        self.prov.last_client_input
    }

    /// The last idle edge and whether a hook reported it.
    pub fn last_idle(&self) -> Option<(Instant, bool)> {
        self.prov
            .last_idle_at
            .map(|at| (at, self.prov.last_idle_hook))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TerminalId;
    use EdgeStatus::{Blocked, Idle, Unknown, Working};

    const SUBMIT: InputSource = InputSource::Client {
        submit: true,
        attach: false,
    };
    const KEY: InputSource = InputSource::Client {
        submit: false,
        attach: false,
    };

    fn api() -> InputSource {
        InputSource::Programmatic(Programmatic::Api)
    }

    /// A pane whose agent is up and idle.
    fn idle(t0: Instant) -> TurnState {
        let mut turn = TurnState::default();
        turn.on_status_edge(Unknown, Idle, false, t0, 1);
        turn
    }

    fn secs(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    fn origin(turn: &TurnState, status: EdgeStatus, now: Instant) -> TurnOrigin {
        turn.effective(status, now, 1).origin
    }

    #[test]
    fn a_client_submit_then_a_working_edge_is_the_users() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        let effective = turn.effective(Working, secs(t0, 3), 3);
        assert_eq!(effective.origin, TurnOrigin::User);
        assert!(effective.user_turn());
        assert_eq!(effective.started_unix, 2);
    }

    #[test]
    fn an_agent_message_carries_its_id() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        let from = TerminalId::alloc();
        turn.note_input(
            &InputSource::Programmatic(Programmatic::AgentMessage {
                id: "m1".into(),
                from: from.clone(),
            }),
            secs(t0, 1),
        );
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        assert_eq!(
            origin(&turn, Working, secs(t0, 3)),
            TurnOrigin::AgentMessage {
                id: "m1".into(),
                from
            }
        );
    }

    #[test]
    fn a_programmatic_write_beats_a_user_submit_since_idle() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.note_input(&api(), secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        assert_eq!(
            origin(&turn, Working, secs(t0, 3)),
            TurnOrigin::Programmatic
        );
    }

    #[test]
    fn a_prompt_into_a_user_turn_poisons_it() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        turn.note_input(&api(), secs(t0, 3));
        let effective = turn.effective(Working, secs(t0, 4), 4);
        assert!(effective.poisoned);
        assert_eq!(effective.origin, TurnOrigin::Programmatic);
        assert!(!effective.user_turn());
    }

    #[test]
    fn no_input_since_idle_is_self_started() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.on_status_edge(Idle, Working, false, secs(t0, 60), 2);
        assert_eq!(
            origin(&turn, Working, secs(t0, 61)),
            TurnOrigin::SelfStarted
        );
    }

    #[test]
    fn blocked_to_working_does_not_recapture() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        turn.on_status_edge(Working, Blocked, false, secs(t0, 3), 3);
        turn.note_input(&KEY, secs(t0, 4));
        turn.on_status_edge(Blocked, Working, false, secs(t0, 5), 5);
        assert_eq!(origin(&turn, Working, secs(t0, 6)), TurnOrigin::User);
    }

    #[test]
    fn a_short_screen_flap_bridges_and_a_long_one_does_not() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        turn.on_status_edge(Working, Idle, false, secs(t0, 10), 10);
        turn.on_status_edge(Idle, Working, false, secs(t0, 12), 12);
        let effective = turn.effective(Working, secs(t0, 13), 13);
        assert_eq!(effective.origin, TurnOrigin::User);
        assert!(effective.bridged);
        turn.on_status_edge(Working, Idle, false, secs(t0, 20), 20);
        turn.on_status_edge(Idle, Working, false, secs(t0, 26), 26);
        assert_eq!(
            origin(&turn, Working, secs(t0, 27)),
            TurnOrigin::SelfStarted
        );
    }

    #[test]
    fn status_edges_report_what_they_did_to_the_turn() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        assert_eq!(
            turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2),
            TurnEdge::Started { user: true }
        );
        assert_eq!(
            turn.on_status_edge(Working, Blocked, false, secs(t0, 3), 3),
            TurnEdge::None
        );
        assert_eq!(
            turn.on_status_edge(Blocked, Working, false, secs(t0, 4), 4),
            TurnEdge::None
        );
        assert_eq!(
            turn.on_status_edge(Working, Idle, false, secs(t0, 10), 10),
            TurnEdge::Ended
        );
        assert_eq!(
            turn.on_status_edge(Idle, Working, false, secs(t0, 12), 12),
            TurnEdge::Bridged
        );
        assert_eq!(
            turn.on_status_edge(Working, Idle, false, secs(t0, 20), 20),
            TurnEdge::Ended
        );
        turn.note_input(&api(), secs(t0, 30));
        assert_eq!(
            turn.on_status_edge(Idle, Working, false, secs(t0, 31), 31),
            TurnEdge::Started { user: false }
        );
        assert_eq!(
            turn.on_status_edge(Working, Working, false, secs(t0, 32), 32),
            TurnEdge::None
        );
        // a restored pane's first turn is not captured but still starts
        let mut restored = TurnState::restored();
        assert_eq!(
            restored.on_status_edge(Unknown, Working, false, t0, 1),
            TurnEdge::Started { user: false }
        );
    }

    #[test]
    fn a_hook_idle_is_never_bridged() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, true, secs(t0, 2), 2);
        turn.on_status_edge(Working, Idle, true, secs(t0, 10), 10);
        turn.on_status_edge(Idle, Working, true, secs(t0, 12), 12);
        assert_eq!(
            origin(&turn, Working, secs(t0, 13)),
            TurnOrigin::SelfStarted
        );
    }

    #[test]
    fn internal_writes_do_not_count() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&InputSource::Internal, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        assert_eq!(origin(&turn, Working, secs(t0, 3)), TurnOrigin::SelfStarted);
    }

    #[test]
    fn an_attach_submit_is_the_users_with_its_detail() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(
            &InputSource::Client {
                submit: true,
                attach: true,
            },
            secs(t0, 1),
        );
        turn.on_status_edge(Idle, Working, false, secs(t0, 2), 2);
        let effective = turn.effective(Working, secs(t0, 3), 3);
        assert!(effective.user_turn() && effective.attach);
        let info = effective.info(|_| None);
        assert_eq!(info.detail, Some(AgentsOriginDetail::ClientAttach));
    }

    #[test]
    fn an_old_submit_is_not_the_users() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Idle, Working, false, secs(t0, 11 * 60), 2);
        assert_eq!(
            origin(&turn, Working, secs(t0, 11 * 60 + 1)),
            TurnOrigin::SelfStarted
        );
    }

    #[test]
    fn the_lag_guard_sees_the_turn_before_detection_does() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&SUBMIT, secs(t0, 1));
        assert!(turn.effective(Idle, secs(t0, 1), 1).user_turn());
        turn.note_input(&api(), secs(t0, 1));
        assert_eq!(origin(&turn, Idle, secs(t0, 1)), TurnOrigin::Programmatic);
    }

    #[test]
    fn a_restored_pane_is_unknown_and_its_first_working_does_not_capture() {
        let t0 = Instant::now();
        let mut turn = TurnState::restored();
        assert_eq!(origin(&turn, Idle, t0), TurnOrigin::Unknown);
        turn.note_input(&SUBMIT, secs(t0, 1));
        turn.on_status_edge(Unknown, Working, false, secs(t0, 2), 2);
        assert_eq!(origin(&turn, Working, secs(t0, 3)), TurnOrigin::Unknown);
        // The next real turn after an idle edge captures again.
        turn.on_status_edge(Working, Idle, true, secs(t0, 10), 10);
        turn.note_input(&SUBMIT, secs(t0, 11));
        turn.on_status_edge(Idle, Working, false, secs(t0, 12), 12);
        assert_eq!(origin(&turn, Working, secs(t0, 13)), TurnOrigin::User);
    }

    #[test]
    fn a_restored_pane_that_starts_idle_does_not_capture_its_first_turn() {
        let t0 = Instant::now();
        let mut turn = TurnState::restored();
        turn.note_input(&SUBMIT, t0);
        turn.on_status_edge(Idle, Working, false, secs(t0, 1), 1);
        assert_eq!(origin(&turn, Working, secs(t0, 2)), TurnOrigin::Unknown);
        assert!(!turn.effective(Working, secs(t0, 2), 2).user_turn());
        turn.on_status_edge(Working, Idle, false, secs(t0, 3), 3);
        turn.note_input(&SUBMIT, secs(t0, 20));
        turn.on_status_edge(Idle, Working, false, secs(t0, 21), 21);
        assert_eq!(origin(&turn, Working, secs(t0, 22)), TurnOrigin::User);
    }

    #[test]
    fn a_fresh_launch_captures_out_of_no_agent() {
        let t0 = Instant::now();
        let mut turn = TurnState::default();
        turn.note_input(&SUBMIT, t0);
        turn.on_status_edge(Unknown, Working, false, secs(t0, 1), 1);
        assert_eq!(origin(&turn, Working, secs(t0, 2)), TurnOrigin::User);
    }

    #[test]
    fn keys_mid_turn_never_reach_the_next_turn() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.on_status_edge(Idle, Working, false, secs(t0, 1), 1);
        // Esc, a permission `y`, steering: no effect on the next turn.
        turn.note_input(&KEY, secs(t0, 2));
        turn.note_input(&SUBMIT, secs(t0, 3));
        turn.on_status_edge(Working, Idle, true, secs(t0, 4), 4);
        turn.on_status_edge(Idle, Working, true, secs(t0, 6), 6);
        assert_eq!(origin(&turn, Working, secs(t0, 7)), TurnOrigin::SelfStarted);
    }

    #[test]
    fn a_write_and_an_edge_in_the_same_tick_take_the_writes_origin() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_input(&api(), t0);
        turn.on_status_edge(Idle, Working, false, t0, 1);
        assert_eq!(origin(&turn, Working, t0), TurnOrigin::Programmatic);
    }

    fn prompt(id: &str) -> Option<String> {
        Some(id.to_string())
    }

    #[test]
    fn a_reported_end_frees_the_turn_until_the_next_start() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        assert_eq!(turn.hook_turn_ended(), None, "nothing reported yet");
        turn.note_hook_turn(true, prompt("p1"), 10, secs(t0, 1));
        assert_eq!(turn.hook_turn_ended(), None, "a live turn");
        turn.note_hook_turn(false, prompt("p1"), 20, secs(t0, 5));
        assert_eq!(turn.hook_turn_ended(), Some(secs(t0, 5)));
        // A task notification starts the next turn on its own.
        turn.note_hook_turn(true, prompt("p2"), 30, secs(t0, 9));
        assert_eq!(turn.hook_turn_ended(), None);
        turn.note_hook_turn(false, prompt("p2"), 40, secs(t0, 12));
        assert_eq!(turn.hook_turn_ended(), Some(secs(t0, 12)));
    }

    #[test]
    fn crossed_reports_order_by_prompt_then_by_the_hook_clock() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_hook_turn(true, prompt("p1"), 10, secs(t0, 1));
        // p1's Stop hook ran before p2's UserPromptSubmit, but its report
        // arrived after it.
        turn.note_hook_turn(true, prompt("p2"), 30, secs(t0, 3));
        turn.note_hook_turn(false, prompt("p1"), 20, secs(t0, 3));
        assert_eq!(turn.hook_turn_ended(), None, "p2 is live");
        // A missed start: the end is newer on the hook clock.
        turn.note_hook_turn(false, prompt("p3"), 50, secs(t0, 8));
        assert_eq!(turn.hook_turn_ended(), Some(secs(t0, 8)));
        // Without prompt ids, the hook clock decides; without that, arrival.
        let mut bare = idle(t0);
        bare.note_hook_turn(true, None, 0, secs(t0, 1));
        bare.note_hook_turn(false, None, 0, secs(t0, 2));
        assert_eq!(bare.hook_turn_ended(), Some(secs(t0, 2)));
        bare.note_hook_turn(true, None, 0, secs(t0, 3));
        assert_eq!(bare.hook_turn_ended(), None);
    }

    #[test]
    fn input_after_a_reported_end_waits_for_the_turn_it_starts() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        turn.note_hook_turn(true, prompt("p1"), 10, secs(t0, 1));
        turn.note_hook_turn(false, prompt("p1"), 20, secs(t0, 4));
        turn.note_input(&KEY, secs(t0, 5));
        assert!(
            turn.hook_turn_ended().is_some(),
            "a keystroke is not a turn"
        );
        turn.note_input(&SUBMIT, secs(t0, 6));
        assert_eq!(turn.hook_turn_ended(), None, "the user submitted");
        let mut typed = idle(t0);
        typed.note_hook_turn(false, prompt("p1"), 20, secs(t0, 4));
        typed.note_input(&api(), secs(t0, 5));
        assert_eq!(typed.hook_turn_ended(), None, "herdr typed into it");
    }

    #[test]
    fn a_reported_start_holds_an_idle_screen_for_a_bounded_time() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        assert_eq!(turn.hook_turn_live_until(t0), None, "nothing reported");
        turn.note_hook_turn(true, prompt("p1"), 10, secs(t0, 1));
        assert_eq!(turn.hook_turn_live_until(secs(t0, 2)), Some(secs(t0, 16)));
        // The screen flaps idle mid-turn: the hold counts from that edge.
        turn.on_status_edge(Idle, Working, false, secs(t0, 3), 2);
        turn.on_status_edge(Working, Idle, false, secs(t0, 10), 3);
        assert_eq!(turn.hook_turn_live_until(secs(t0, 20)), Some(secs(t0, 25)));
        // An Esc-ended turn reports no Stop: the hold runs out.
        assert_eq!(turn.hook_turn_live_until(secs(t0, 25)), None);
        // Its Stop ends it.
        turn.note_hook_turn(false, prompt("p1"), 20, secs(t0, 11));
        assert_eq!(turn.hook_turn_live_until(secs(t0, 12)), None);
        // A start older than the last submit or herdr write is not trusted.
        let mut typed = idle(t0);
        typed.note_hook_turn(true, prompt("p2"), 10, secs(t0, 1));
        typed.note_input(&api(), secs(t0, 2));
        assert_eq!(typed.hook_turn_live_until(secs(t0, 3)), None);
    }

    #[test]
    fn a_recent_programmatic_write_and_cleared_marks_are_seen() {
        let t0 = Instant::now();
        let mut turn = idle(t0);
        let window = Duration::from_secs(30);
        assert!(!turn.programmatic_within(window, t0));
        turn.note_input(&api(), secs(t0, 1));
        assert!(turn.programmatic_within(window, secs(t0, 30)));
        assert!(!turn.programmatic_within(window, secs(t0, 31)));
        turn.note_hook_turn(false, prompt("p1"), 20, secs(t0, 40));
        assert!(turn.hook_turn_ended().is_some());
        turn.clear_hook_marks();
        assert_eq!(turn.hook_turn_ended(), None);
        assert_eq!(turn.hook_turn_live_until(secs(t0, 41)), None);
    }

    #[test]
    fn a_restored_pane_knows_no_hook_turn() {
        let t0 = Instant::now();
        let mut turn = TurnState::restored();
        assert_eq!(turn.hook_turn_ended(), None);
        // The first end after a restart is the last thing known.
        turn.note_hook_turn(false, prompt("p9"), 90, secs(t0, 1));
        assert_eq!(turn.hook_turn_ended(), Some(secs(t0, 1)));
    }
}
