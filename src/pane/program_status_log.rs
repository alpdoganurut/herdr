//! OSC 7501 disagreement log (fork, stage 1: observe only).
//!
//! Each detector loop ticks a [`DisagreementTracker`]. When the root Program
//! Status record's mapped state disagrees with the state screen detection
//! published for longer than [`DEBOUNCE`], the episode is logged once (one
//! more line when a long episode is still open at [`OPEN_EMIT`], and a
//! closing line). At each agent change away a session summary is logged,
//! with the agreement and short-episode counts that give the lines a
//! denominator. Lines go to `tracing` and to
//! `<state dir>/program-status-disagreements.jsonl` (rotated), through one
//! global rate limit. Only lengths of the untrusted message and title are
//! logged, never their text.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::program_status::{self, unpack_root, ProgramState};
use super::terminal::PaneTerminal;
use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;

/// More than the pending-idle cap (0.7 s) plus a 0.3 s tick, with slack.
pub(super) const DEBOUNCE: Duration = Duration::from_secs(2);
/// A still-open episode logs one progress line at this age.
pub(super) const OPEN_EMIT: Duration = Duration::from_secs(120);
/// Lines admitted per rolling hour, across panes.
const LINES_PER_HOUR: usize = 600;
const RATE_WINDOW: Duration = Duration::from_secs(3600);
/// The log rotates (one kept generation) past this size.
#[cfg_attr(test, allow(dead_code))] // tests write through append_with_cap
const ROTATE_BYTES: u64 = 1024 * 1024;
const LOG_FILE: &str = "program-status-disagreements.jsonl";
const ROTATED_FILE: &str = "program-status-disagreements.1.jsonl";

#[derive(Clone, Copy, Debug)]
pub(super) struct DisagreementInput {
    pub now: Instant,
    pub enabled: bool,
    pub agent: Option<Agent>,
    /// The loop is not reading the screen for this agent this tick (startup
    /// grace, background job, lifecycle hook authority, exit, agent change).
    pub gated: bool,
    pub screen_state: AgentState,
    pub root_packed: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum DisagreementAction {
    None,
    /// The episode crossed the debounce: read its details once.
    Capture,
    Emit(Box<LogLine>),
}

/// What the detail read at the debounce crossing found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Captured {
    pub rule_id: Option<String>,
    pub rule_state: Option<&'static str>,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub screen_text_len: usize,
    pub kind: Option<&'static str>,
    pub msg_len: usize,
    pub title_len: usize,
    pub app: Option<String>,
    /// The root record's age at the capture.
    pub record_age_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Episode {
    agent: Agent,
    since: Instant,
    since_wall_ms: u64,
    osc_mapped: AgentState,
    screen: AgentState,
    record_state: ProgramState,
    start_revision: u64,
    last_revision: u64,
    revisions_seen: u32,
    rejects_start: u8,
    children: u8,
    captured: Option<Captured>,
    open_emitted: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SessionTicks {
    agree_ticks: u64,
    disagree_ticks: u64,
    episodes: u32,
    logged_episodes: u32,
    short_episodes: u32,
    short_max_ms: u64,
    children_max: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct DisagreementLine {
    pub kind: &'static str,
    pub ts_ms: u64,
    pub pane: u64,
    pub agent: &'static str,
    pub screen_state: &'static str,
    pub osc_mapped: &'static str,
    pub record_state: &'static str,
    pub record_kind: Option<&'static str>,
    pub msg_len: usize,
    pub title_len: usize,
    pub app: Option<String>,
    pub rule_id: Option<String>,
    pub rule_state: Option<&'static str>,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub screen_text_len: usize,
    /// The episode's start (wall clock); the open line and the closing line
    /// of one episode share it.
    pub since_ms: u64,
    pub duration_ms: u64,
    pub end_reason: &'static str,
    pub open_emitted: bool,
    pub record_revisions: u32,
    pub rejects_during_episode: u32,
    /// The most child records seen during the episode.
    pub osc_children: u8,
    /// The root record's age when the episode started; null when the record
    /// changed during the episode.
    pub osc_age_ms_at_start: Option<u64>,
    pub dropped_before: u64,
    pub herdr_version: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(super) struct SessionSummaryLine {
    pub kind: &'static str,
    pub ts_ms: u64,
    pub pane: u64,
    pub agent: &'static str,
    pub probe_answered: bool,
    pub reports: u32,
    pub rejects: u32,
    pub root_transitions: u32,
    pub agree_ticks: u64,
    pub disagree_ticks: u64,
    pub episodes: u32,
    pub logged_episodes: u32,
    pub short_episodes: u32,
    pub short_max_ms: u64,
    pub osc_children_max: u8,
    pub dropped_before: u64,
    pub herdr_version: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum LogLine {
    Disagreement(DisagreementLine),
    Summary(SessionSummaryLine),
}

impl LogLine {
    fn set_dropped_before(&mut self, dropped: u64) {
        match self {
            Self::Disagreement(line) => line.dropped_before = dropped,
            Self::Summary(line) => line.dropped_before = dropped,
        }
    }

    fn to_json(&self) -> String {
        match self {
            Self::Disagreement(line) => serde_json::to_string(line),
            Self::Summary(line) => serde_json::to_string(line),
        }
        .unwrap_or_default()
    }
}

/// One per pane, owned by the pane's detector loop. Pure: it reads only
/// what the loop passes in.
#[derive(Debug)]
pub(super) struct DisagreementTracker {
    pane: u64,
    episode: Option<Episode>,
    session: SessionTicks,
    last_packed: u64,
    was_disabled: bool,
    /// After the switch comes back on, the record the mirror still holds
    /// predates the off period until its revision moves.
    stale_revision: Option<u64>,
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn state_label(state: AgentState) -> &'static str {
    crate::detect::manifest::agent_state_label(state)
}

impl DisagreementTracker {
    pub(super) fn new(pane_id: PaneId) -> Self {
        Self {
            pane: u64::from(pane_id.raw()),
            episode: None,
            session: SessionTicks::default(),
            last_packed: 0,
            was_disabled: false,
            stale_revision: None,
        }
    }

    /// The loop's detect reset: forget everything without logging.
    pub(super) fn reset(&mut self) {
        self.episode = None;
        self.session = SessionTicks::default();
        self.stale_revision = None;
    }

    pub(super) fn tick(&mut self, input: DisagreementInput) -> DisagreementAction {
        if !input.enabled {
            self.was_disabled = true;
            return self.end("disabled", input.now);
        }
        let mut root = unpack_root(input.root_packed);
        self.last_packed = input.root_packed;
        if std::mem::take(&mut self.was_disabled) {
            self.stale_revision = Some(root.revision);
        }
        match self.stale_revision {
            Some(revision) if revision == root.revision => root.state = None,
            Some(_) => self.stale_revision = None,
            None => {}
        }
        let Some(agent) = input.agent else {
            return self.end("no_agent", input.now);
        };
        if input.gated {
            return self.end("gated", input.now);
        }
        if input.screen_state == AgentState::Unknown {
            return self.end("screen_unknown", input.now);
        }
        let Some(record_state) = root.state else {
            return self.end("no_record", input.now);
        };
        self.session.children_max = self.session.children_max.max(root.children);
        let mapped = program_status::mapped_agent_state(record_state);
        if mapped == input.screen_state {
            self.session.agree_ticks = self.session.agree_ticks.saturating_add(1);
            return self.end("agreed", input.now);
        }
        self.session.disagree_ticks = self.session.disagree_ticks.saturating_add(1);

        let mut action = DisagreementAction::None;
        let same_pair = self.episode.as_ref().is_some_and(|episode| {
            episode.osc_mapped == mapped
                && episode.screen == input.screen_state
                && episode.agent == agent
        });
        if !same_pair {
            action = self.end("changed", input.now);
            self.episode = Some(Episode {
                agent,
                since: input.now,
                since_wall_ms: wall_ms(),
                osc_mapped: mapped,
                screen: input.screen_state,
                record_state,
                start_revision: root.revision,
                last_revision: root.revision,
                revisions_seen: 1,
                rejects_start: root.rejected,
                children: root.children,
                captured: None,
                open_emitted: false,
            });
            return action;
        }
        let Some(episode) = self.episode.as_mut() else {
            return action;
        };
        if episode.last_revision != root.revision {
            episode.last_revision = root.revision;
            episode.revisions_seen = episode.revisions_seen.saturating_add(1);
        }
        episode.record_state = record_state;
        episode.children = episode.children.max(root.children);
        let age = input.now.saturating_duration_since(episode.since);
        if age >= DEBOUNCE && episode.captured.is_none() {
            return DisagreementAction::Capture;
        }
        if age >= OPEN_EMIT && episode.captured.is_some() && !episode.open_emitted {
            episode.open_emitted = true;
            let line = self.line("open", input.now, false);
            return line.map_or(DisagreementAction::None, |line| {
                DisagreementAction::Emit(Box::new(LogLine::Disagreement(line)))
            });
        }
        DisagreementAction::None
    }

    pub(super) fn capture(&mut self, captured: Captured) {
        if let Some(episode) = self.episode.as_mut() {
            episode.captured = Some(captured);
        }
    }

    fn line(
        &self,
        end_reason: &'static str,
        now: Instant,
        closing: bool,
    ) -> Option<DisagreementLine> {
        let episode = self.episode.as_ref()?;
        let captured = episode.captured.clone().unwrap_or_default();
        let duration = now.saturating_duration_since(episode.since);
        let root = unpack_root(self.last_packed);
        // The record unchanged since the start: its age then is its age at
        // the capture less the episode's age at the capture (approximated
        // by the debounce, which is when the capture runs).
        let osc_age_ms_at_start = (episode.last_revision == episode.start_revision)
            .then(|| {
                captured
                    .record_age_ms
                    .map(|age| age.saturating_sub(duration_ms(DEBOUNCE)))
            })
            .flatten();
        Some(DisagreementLine {
            kind: "disagreement",
            ts_ms: wall_ms(),
            pane: self.pane,
            agent: crate::detect::agent_label(episode.agent),
            screen_state: state_label(episode.screen),
            osc_mapped: state_label(episode.osc_mapped),
            record_state: episode.record_state.label(),
            record_kind: captured.kind,
            msg_len: captured.msg_len,
            title_len: captured.title_len,
            app: captured.app,
            rule_id: captured.rule_id,
            rule_state: captured.rule_state,
            visible_idle: captured.visible_idle,
            visible_blocker: captured.visible_blocker,
            visible_working: captured.visible_working,
            screen_text_len: captured.screen_text_len,
            since_ms: episode.since_wall_ms,
            duration_ms: duration_ms(duration),
            end_reason,
            open_emitted: closing && episode.open_emitted,
            record_revisions: episode.revisions_seen,
            rejects_during_episode: u32::from(root.rejected.wrapping_sub(episode.rejects_start)),
            osc_children: episode.children,
            osc_age_ms_at_start,
            dropped_before: 0,
            herdr_version: env!("CARGO_PKG_VERSION"),
        })
    }

    /// Ends the open episode. Logs it only when it outlived the debounce.
    fn end(&mut self, reason: &'static str, now: Instant) -> DisagreementAction {
        let Some(episode) = self.episode.as_ref() else {
            return DisagreementAction::None;
        };
        self.session.episodes = self.session.episodes.saturating_add(1);
        let action = if episode.captured.is_some() {
            self.session.logged_episodes = self.session.logged_episodes.saturating_add(1);
            self.line(reason, now, true)
                .map_or(DisagreementAction::None, |line| {
                    DisagreementAction::Emit(Box::new(LogLine::Disagreement(line)))
                })
        } else {
            let age = duration_ms(now.saturating_duration_since(episode.since));
            self.session.short_episodes = self.session.short_episodes.saturating_add(1);
            self.session.short_max_ms = self.session.short_max_ms.max(age);
            DisagreementAction::None
        };
        self.episode = None;
        action
    }

    /// The agent left the pane: close the episode and summarize the session.
    /// `counters` come from the store before its agent-change clear.
    pub(super) fn end_agent_session(
        &mut self,
        agent: Agent,
        counters: program_status::ProgramStatusSessionCounters,
        now: Instant,
    ) -> Vec<LogLine> {
        let mut lines = Vec::new();
        if let DisagreementAction::Emit(line) = self.end("agent_changed", now) {
            lines.push(*line);
        }
        let session = std::mem::take(&mut self.session);
        if counters.probe_answered || counters.reports > 0 {
            lines.push(LogLine::Summary(SessionSummaryLine {
                kind: "session_summary",
                ts_ms: wall_ms(),
                pane: self.pane,
                agent: crate::detect::agent_label(agent),
                probe_answered: counters.probe_answered,
                reports: counters.reports,
                rejects: counters.rejects,
                root_transitions: counters.root_transitions,
                agree_ticks: session.agree_ticks,
                disagree_ticks: session.disagree_ticks,
                episodes: session.episodes,
                logged_episodes: session.logged_episodes,
                short_episodes: session.short_episodes,
                short_max_ms: session.short_max_ms,
                osc_children_max: session.children_max,
                dropped_before: 0,
                herdr_version: env!("CARGO_PKG_VERSION"),
            }));
        }
        lines
    }
}

/// One detector-loop tick: everything the loops share, so each loop adds a
/// single call. Runs before the loop's skips, every tick.
pub(super) fn tick_detector(
    tracker: &mut DisagreementTracker,
    terminal: &PaneTerminal,
    agent: Option<Agent>,
    screen_state: AgentState,
    gated: bool,
    screen_text: &str,
    now: Instant,
) {
    let action = tracker.tick(DisagreementInput {
        now,
        enabled: program_status::enabled(),
        agent,
        gated,
        screen_state,
        root_packed: terminal.program_status_root_packed(),
    });
    match action {
        DisagreementAction::None => {}
        DisagreementAction::Capture => {
            if let Some(agent) = agent {
                tracker.capture(capture_details(terminal, agent, screen_text));
            }
        }
        DisagreementAction::Emit(line) => emit(*line),
    }
}

/// Called by the loops right before the agent-change clear of the store.
pub(super) fn end_agent_session(
    tracker: &mut DisagreementTracker,
    terminal: &PaneTerminal,
    previous_agent: Option<Agent>,
    now: Instant,
) {
    let Some(agent) = previous_agent else {
        return;
    };
    let counters = terminal.program_status_session_counters();
    for line in tracker.end_agent_session(agent, counters, now) {
        emit(line);
    }
}

/// Once per episode: one store snapshot and one manifest evaluation over
/// the loop's cached screen text (as last refreshed; a tick stale at most).
fn capture_details(terminal: &PaneTerminal, agent: Agent, screen_text: &str) -> Captured {
    let snapshot = terminal.program_status_snapshot();
    let osc_title = terminal.agent_osc_title();
    let osc_progress = terminal.agent_osc_progress();
    let explain = crate::detect::manifest::explain_with_input(
        agent,
        crate::detect::manifest::DetectionInput {
            screen: screen_text,
            osc_title: &osc_title,
            osc_progress: &osc_progress,
        },
    );
    let root = snapshot.root.as_ref();
    Captured {
        rule_id: explain.matched_rule.as_ref().map(|rule| rule.id.clone()),
        rule_state: Some(state_label(explain.state)),
        visible_idle: explain.visible_idle,
        visible_blocker: explain.visible_blocker,
        visible_working: explain.visible_working,
        screen_text_len: screen_text.len(),
        kind: root.and_then(|root| root.record.kind.map(|kind| kind.label())),
        msg_len: root
            .and_then(|root| root.record.msg.as_ref())
            .map_or(0, String::len),
        title_len: root
            .and_then(|root| root.record.title.as_ref())
            .map_or(0, String::len),
        app: root.and_then(|root| root.app.clone()),
        record_age_ms: root.map(|root| root.age_ms),
    }
}

/// The global limit: lines admitted in the last hour, and the count
/// dropped since the last admitted line.
#[derive(Debug, Default)]
pub(super) struct RateLimiter {
    admitted: VecDeque<Instant>,
    dropped: u64,
}

impl RateLimiter {
    /// `Some(dropped_before)` when the line may be written.
    pub(super) fn admit(&mut self, now: Instant, cap: usize) -> Option<u64> {
        while self
            .admitted
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= RATE_WINDOW)
        {
            self.admitted.pop_front();
        }
        if self.admitted.len() >= cap {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        }
        self.admitted.push_back(now);
        Some(std::mem::take(&mut self.dropped))
    }
}

static LIMITER: Mutex<RateLimiter> = Mutex::new(RateLimiter {
    admitted: VecDeque::new(),
    dropped: 0,
});
static APPEND_LOCK: Mutex<()> = Mutex::new(());

fn emit(mut line: LogLine) {
    let admitted = match LIMITER.lock() {
        Ok(mut limiter) => limiter.admit(Instant::now(), LINES_PER_HOUR),
        Err(poisoned) => poisoned.into_inner().admit(Instant::now(), LINES_PER_HOUR),
    };
    let Some(dropped_before) = admitted else {
        tracing::debug!(target: "herdr::program_status", "program status log line dropped by the rate limit");
        return;
    };
    line.set_dropped_before(dropped_before);
    let json = line.to_json();
    match &line {
        LogLine::Disagreement(_) => {
            tracing::info!(target: "herdr::program_status", line = %json, "status disagreement")
        }
        LogLine::Summary(_) => {
            tracing::info!(target: "herdr::program_status", line = %json, "status session summary")
        }
    }
    write_line(json);
}

/// Tests never touch the real state dir; they call [`append_with_cap`].
#[cfg(test)]
fn write_line(_json: String) {}

#[cfg(not(test))]
fn write_line(json: String) {
    let dir = crate::config::state_dir();
    let write = move || {
        if let Err(err) = append_with_cap(&dir, &json, ROTATE_BYTES) {
            tracing::warn!(target: "herdr::program_status", err = %err, "program status log write failed");
        }
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn_blocking(write);
        }
        Err(_) => write(),
    }
}

pub(super) fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE)
}

/// Appends one line, first rotating a log past `cap` bytes.
pub(super) fn append_with_cap(dir: &Path, json: &str, cap: u64) -> io::Result<()> {
    let _guard = match APPEND_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let path = log_path(dir);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > cap) {
        std::fs::rename(&path, dir.join(ROTATED_FILE))?;
    }
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = String::with_capacity(json.len() + 1);
    line.push_str(json);
    line.push('\n');
    file.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::program_status::{pack_root, PackedRoot};

    fn packed(state: Option<ProgramState>, revision: u64) -> u64 {
        pack_root(PackedRoot {
            state,
            children: 0,
            rejected: 0,
            revision,
        })
    }

    fn input(now: Instant, screen: AgentState, root_packed: u64) -> DisagreementInput {
        DisagreementInput {
            now,
            enabled: true,
            agent: Some(Agent::Claude),
            gated: false,
            screen_state: screen,
            root_packed,
        }
    }

    fn tracker() -> DisagreementTracker {
        DisagreementTracker::new(PaneId::from_raw(7))
    }

    fn emitted(action: DisagreementAction) -> DisagreementLine {
        match action {
            DisagreementAction::Emit(line) => match *line {
                LogLine::Disagreement(line) => line,
                other => panic!("expected a disagreement line, got {other:?}"),
            },
            other => panic!("expected a disagreement line, got {other:?}"),
        }
    }

    #[test]
    fn agreement_does_nothing() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Done), 1);
        for step in 0..20 {
            let now = start + Duration::from_millis(300 * step);
            assert_eq!(
                tracker.tick(input(now, AgentState::Idle, root)),
                DisagreementAction::None
            );
        }
        assert_eq!(tracker.session.agree_ticks, 20);
    }

    #[test]
    fn short_disagreement_is_counted_not_logged() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Blocked), 1);
        assert_eq!(
            tracker.tick(input(start, AgentState::Working, root)),
            DisagreementAction::None
        );
        assert_eq!(
            tracker.tick(input(
                start + Duration::from_millis(1900),
                AgentState::Working,
                root
            )),
            DisagreementAction::None
        );
        assert_eq!(
            tracker.tick(input(
                start + Duration::from_millis(2100),
                AgentState::Blocked,
                root
            )),
            DisagreementAction::None
        );
        assert_eq!(tracker.session.short_episodes, 1);
        assert!(tracker.session.short_max_ms >= 2000);
    }

    #[test]
    fn long_disagreement_captures_once_and_logs_at_agreement() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Blocked), 3);
        tracker.tick(input(start, AgentState::Idle, root));
        assert_eq!(
            tracker.tick(input(start + DEBOUNCE, AgentState::Idle, root)),
            DisagreementAction::Capture
        );
        tracker.capture(Captured {
            rule_id: Some("turn_ended_idle".into()),
            rule_state: Some("idle"),
            kind: Some("permission"),
            msg_len: 44,
            record_age_ms: Some(2500),
            ..Captured::default()
        });
        assert_eq!(
            tracker.tick(input(
                start + DEBOUNCE + Duration::from_millis(300),
                AgentState::Idle,
                root
            )),
            DisagreementAction::None
        );
        let line = emitted(tracker.tick(input(
            start + Duration::from_millis(5310),
            AgentState::Blocked,
            root,
        )));
        assert_eq!(line.duration_ms, 5310);
        assert_eq!(line.end_reason, "agreed");
        assert_eq!(line.screen_state, "idle");
        assert_eq!(line.osc_mapped, "blocked");
        assert_eq!(line.record_kind, Some("permission"));
        assert_eq!(line.rule_id.as_deref(), Some("turn_ended_idle"));
        assert_eq!(line.osc_age_ms_at_start, Some(500));
        assert_eq!(line.pane, 7);
        assert!(!line.open_emitted);
    }

    #[test]
    fn a_new_pair_restarts_the_episode() {
        let mut tracker = tracker();
        let start = Instant::now();
        tracker.tick(input(
            start,
            AgentState::Idle,
            packed(Some(ProgramState::Working), 1),
        ));
        tracker.tick(input(
            start + DEBOUNCE,
            AgentState::Idle,
            packed(Some(ProgramState::Working), 1),
        ));
        tracker.capture(Captured::default());
        let line = emitted(tracker.tick(input(
            start + DEBOUNCE + Duration::from_millis(300),
            AgentState::Idle,
            packed(Some(ProgramState::Blocked), 2),
        )));
        assert_eq!(line.end_reason, "changed");
        assert_eq!(line.osc_mapped, "working");
        let episode = tracker.episode.as_ref().expect("a new episode");
        assert_eq!(episode.osc_mapped, AgentState::Blocked);
    }

    #[test]
    fn gated_no_agent_and_unknown_screen_end_or_never_start() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Blocked), 1);
        tracker.tick(input(start, AgentState::Unknown, root));
        assert!(tracker.episode.is_none());
        tracker.tick(input(start, AgentState::Idle, root));
        assert!(tracker.episode.is_some());
        let mut gated = input(start + Duration::from_millis(300), AgentState::Idle, root);
        gated.gated = true;
        tracker.tick(gated);
        assert!(tracker.episode.is_none());
        tracker.tick(input(start, AgentState::Idle, root));
        let mut no_agent = input(start + Duration::from_millis(300), AgentState::Idle, root);
        no_agent.agent = None;
        tracker.tick(no_agent);
        assert!(tracker.episode.is_none());
        tracker.tick(input(start, AgentState::Idle, packed(None, 2)));
        assert!(tracker.episode.is_none());
    }

    #[test]
    fn an_open_episode_logs_once_then_closes_with_its_final_duration() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Working), 1);
        tracker.tick(input(start, AgentState::Idle, root));
        tracker.tick(input(start + DEBOUNCE, AgentState::Idle, root));
        tracker.capture(Captured::default());
        let open = emitted(tracker.tick(input(start + OPEN_EMIT, AgentState::Idle, root)));
        assert_eq!(open.end_reason, "open");
        assert!(!open.open_emitted);
        assert_eq!(
            tracker.tick(input(
                start + OPEN_EMIT + Duration::from_secs(1),
                AgentState::Idle,
                root
            )),
            DisagreementAction::None
        );
        let close = emitted(tracker.tick(input(
            start + OPEN_EMIT + Duration::from_secs(5),
            AgentState::Working,
            root,
        )));
        assert_eq!(close.end_reason, "agreed");
        assert!(close.open_emitted);
        assert_eq!(close.since_ms, open.since_ms);
        assert_eq!(close.duration_ms, 125_000);
    }

    #[test]
    fn rejects_and_revisions_during_an_episode_are_counted() {
        let mut tracker = tracker();
        let start = Instant::now();
        let with = |revision, rejected| {
            pack_root(PackedRoot {
                state: Some(ProgramState::Blocked),
                children: 2,
                rejected,
                revision,
            })
        };
        tracker.tick(input(start, AgentState::Idle, with(5, 255)));
        tracker.tick(input(start + DEBOUNCE, AgentState::Idle, with(6, 0)));
        tracker.capture(Captured::default());
        let line = emitted(tracker.tick(input(
            start + DEBOUNCE + Duration::from_millis(300),
            AgentState::Blocked,
            with(6, 0),
        )));
        assert_eq!(line.rejects_during_episode, 1);
        assert_eq!(line.record_revisions, 2);
        assert_eq!(line.osc_children, 2);
        assert_eq!(line.osc_age_ms_at_start, None);
    }

    #[test]
    fn switching_off_ends_and_back_on_ignores_the_stale_record() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Blocked), 4);
        let mut off = input(start, AgentState::Idle, root);
        off.enabled = false;
        tracker.tick(off);
        tracker.tick(input(start, AgentState::Idle, root));
        tracker.tick(input(start + DEBOUNCE * 2, AgentState::Idle, root));
        assert!(tracker.episode.is_none(), "stale record ignored");
        tracker.tick(input(
            start + DEBOUNCE * 3,
            AgentState::Idle,
            packed(Some(ProgramState::Blocked), 5),
        ));
        assert!(tracker.episode.is_some(), "a fresh report counts again");
    }

    #[test]
    fn session_summary_needs_a_reporting_program() {
        let mut tracker = tracker();
        let start = Instant::now();
        tracker.tick(input(
            start,
            AgentState::Idle,
            packed(Some(ProgramState::Idle), 1),
        ));
        assert!(tracker
            .end_agent_session(
                Agent::Claude,
                program_status::ProgramStatusSessionCounters::default(),
                start
            )
            .is_empty());
        tracker.tick(input(
            start,
            AgentState::Idle,
            packed(Some(ProgramState::Idle), 1),
        ));
        tracker.tick(input(
            start + Duration::from_millis(300),
            AgentState::Working,
            packed(Some(ProgramState::Idle), 1),
        ));
        let lines = tracker.end_agent_session(
            Agent::Claude,
            program_status::ProgramStatusSessionCounters {
                probe_answered: true,
                reports: 9,
                rejects: 0,
                root_transitions: 4,
            },
            start + Duration::from_millis(900),
        );
        let [LogLine::Summary(summary)] = lines.as_slice() else {
            panic!("expected one summary, got {lines:?}");
        };
        assert_eq!(summary.reports, 9);
        assert_eq!(summary.agree_ticks, 1);
        assert_eq!(summary.disagree_ticks, 1);
        assert_eq!(summary.short_episodes, 1);
        assert_eq!(summary.short_max_ms, 600);
        assert_eq!(tracker.session, SessionTicks::default());
    }

    #[test]
    fn rate_limiter_caps_and_reports_dropped_lines() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        for _ in 0..3 {
            assert_eq!(limiter.admit(start, 3), Some(0));
        }
        assert_eq!(limiter.admit(start, 3), None);
        assert_eq!(limiter.admit(start, 3), None);
        assert_eq!(limiter.admit(start + RATE_WINDOW, 3), Some(2));
    }

    #[test]
    fn append_rotates_past_the_cap() {
        let dir =
            std::env::temp_dir().join(format!("herdr-program-status-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        append_with_cap(&dir, "{\"a\":1}", 8).expect("first append");
        append_with_cap(&dir, "{\"a\":2}", 8).expect("second append, under the cap");
        append_with_cap(&dir, "{\"a\":3}", 8).expect("third append rotates");
        let current = std::fs::read_to_string(log_path(&dir)).expect("current log");
        let rotated = std::fs::read_to_string(dir.join(ROTATED_FILE)).expect("rotated log");
        assert_eq!(current, "{\"a\":3}\n");
        assert_eq!(rotated, "{\"a\":1}\n{\"a\":2}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_carries_every_field_and_never_the_message_text() {
        let mut tracker = tracker();
        let start = Instant::now();
        let root = packed(Some(ProgramState::Blocked), 1);
        tracker.tick(input(start, AgentState::Idle, root));
        tracker.tick(input(start + DEBOUNCE, AgentState::Idle, root));
        tracker.capture(Captured {
            kind: Some("permission"),
            msg_len: 12,
            app: Some("claude-code".into()),
            ..Captured::default()
        });
        let line = emitted(tracker.tick(input(
            start + DEBOUNCE + Duration::from_millis(1),
            AgentState::Blocked,
            root,
        )));
        let json: serde_json::Value =
            serde_json::from_str(&LogLine::Disagreement(line).to_json()).expect("json line");
        for key in [
            "kind",
            "ts_ms",
            "pane",
            "agent",
            "screen_state",
            "osc_mapped",
            "record_state",
            "record_kind",
            "msg_len",
            "title_len",
            "app",
            "rule_id",
            "rule_state",
            "visible_idle",
            "visible_blocker",
            "visible_working",
            "screen_text_len",
            "since_ms",
            "duration_ms",
            "end_reason",
            "open_emitted",
            "record_revisions",
            "rejects_during_episode",
            "osc_children",
            "osc_age_ms_at_start",
            "dropped_before",
            "herdr_version",
        ] {
            assert!(json.get(key).is_some(), "missing {key}: {json}");
        }
        assert!(json.get("msg").is_none());
        assert_eq!(json["agent"], "claude");
    }
}
