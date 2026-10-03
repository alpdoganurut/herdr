//! Fork: record the native thread id of Codex panes the Codex integration
//! hook does not report (see [`crate::codex_sessions`] for the evidence).
//!
//! A slow server tick lists the panes whose foreground agent is Codex and
//! that have no session yet (or one this probe reported, so `/new` is
//! followed), then a background thread reads the pane processes and Codex's
//! rollout files. A match is applied exactly as a `herdr:codex` hook report
//! would be, so resume, restore, suspend and the agents model all use it.
//! Sessions reported by the hook or carried by `codex resume <id>` launch
//! arguments are never replaced.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::App;
use crate::codex_sessions::{CodexProcess, RolloutCache};
use crate::terminal::TerminalId;

/// How often the probe looks for Codex panes and unresolved threads (a
/// rollout file only appears with the first prompt).
const CODEX_SESSION_PROBE_INTERVAL: Duration = Duration::from_secs(5);
/// How often panes that already have a probe-reported thread are rechecked
/// for a newer one (`/new`, `/clear`).
const CODEX_SESSION_RECHECK_INTERVAL: Duration = Duration::from_secs(30);
/// A pane the probe could not resolve waits twice as long each time, up to
/// this (a `codex resume` thread, a Codex running in another directory, or
/// a platform without process start times never resolves).
const CODEX_SESSION_MAX_BACKOFF: Duration = Duration::from_secs(120);

const CODEX_SOURCE: &str = "herdr:codex";
const CODEX_AGENT: &str = "codex";

#[derive(Default)]
pub(crate) struct CodexSessionProbe {
    deadline: Option<Instant>,
    last_recheck: Option<Instant>,
    thread: Option<std::thread::JoinHandle<CodexProbeOutcome>>,
    /// Parsed rollout first lines, handed to and back from the thread.
    cache: Option<RolloutCache>,
    /// Thread ids this probe applied, by terminal. Only these may be
    /// replaced by a later probe.
    reported: HashMap<TerminalId, String>,
    /// Panes the probe found nothing for: how many times in a row, and when
    /// they may be probed again.
    misses: HashMap<TerminalId, (u32, Instant)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexProbeCandidate {
    pub(crate) terminal_id: TerminalId,
    /// The pane already shows a thread this probe reported.
    pub(crate) resolved: bool,
}

struct CodexProbeRequest {
    terminal_id: TerminalId,
    shell_pid: u32,
}

struct CodexProbeOutcome {
    cache: RolloutCache,
    /// Every terminal the probe looked at.
    probed: Vec<TerminalId>,
    resolved: Vec<(TerminalId, String)>,
}

impl App {
    pub(crate) fn next_codex_session_probe_deadline(&self) -> Option<Instant> {
        self.codex_sessions.deadline
    }

    /// The tick: apply a finished probe, then start the next one when a
    /// Codex pane needs it. Returns whether pane state changed.
    pub(crate) fn handle_codex_session_probe(&mut self, now: Instant) -> bool {
        let changed = self.reap_codex_session_probe(now);
        if self.codex_sessions.deadline.is_none() {
            self.codex_sessions.deadline = Some(now + CODEX_SESSION_PROBE_INTERVAL);
            return changed;
        }
        if self
            .codex_sessions
            .deadline
            .is_some_and(|deadline| now < deadline)
        {
            return changed;
        }
        self.codex_sessions.deadline = Some(now + CODEX_SESSION_PROBE_INTERVAL);
        if self.codex_sessions.thread.is_some() {
            return changed;
        }
        let candidates = self.codex_session_candidates();
        self.forget_codex_session_reports(&candidates);
        let recheck_due = self
            .codex_sessions
            .last_recheck
            .is_none_or(|last| now.duration_since(last) >= CODEX_SESSION_RECHECK_INTERVAL);
        if recheck_due {
            self.codex_sessions.last_recheck = Some(now);
        }
        let misses = &self.codex_sessions.misses;
        let requests: Vec<CodexProbeRequest> = candidates
            .into_iter()
            .filter(|candidate| !candidate.resolved || recheck_due)
            .filter(|candidate| {
                misses
                    .get(&candidate.terminal_id)
                    .is_none_or(|(_, next)| now >= *next)
            })
            .filter_map(|candidate| {
                let shell_pid = self
                    .terminal_runtimes
                    .get(&candidate.terminal_id)
                    .and_then(|runtime| runtime.child_pid())?;
                Some(CodexProbeRequest {
                    terminal_id: candidate.terminal_id,
                    shell_pid,
                })
            })
            .collect();
        if requests.is_empty() {
            return changed;
        }
        let Ok(codex_home) = crate::integration::codex_dir() else {
            return changed;
        };
        let sessions_root = codex_home.join("sessions");
        let cache = self.codex_sessions.cache.take().unwrap_or_default();
        match std::thread::Builder::new()
            .name("herdr-codex-sessions".into())
            .spawn(move || run_codex_probe(&sessions_root, requests, cache))
        {
            Ok(thread) => self.codex_sessions.thread = Some(thread),
            Err(err) => {
                tracing::warn!(err = %err, "failed to spawn codex session probe thread");
            }
        }
        changed
    }

    fn reap_codex_session_probe(&mut self, now: Instant) -> bool {
        if !self
            .codex_sessions
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return false;
        }
        let Some(thread) = self.codex_sessions.thread.take() else {
            return false;
        };
        match thread.join() {
            Ok(outcome) => {
                self.codex_sessions.cache = Some(outcome.cache);
                self.note_codex_probe_misses(&outcome.probed, &outcome.resolved, now);
                self.apply_codex_session_results(outcome.resolved)
            }
            Err(_) => {
                tracing::warn!("codex session probe thread panicked");
                false
            }
        }
    }

    /// Back off panes the probe found nothing for; a found thread clears it.
    fn note_codex_probe_misses(
        &mut self,
        probed: &[TerminalId],
        resolved: &[(TerminalId, String)],
        now: Instant,
    ) {
        for terminal_id in probed {
            if resolved.iter().any(|(found, _)| found == terminal_id) {
                self.codex_sessions.misses.remove(terminal_id);
                continue;
            }
            let entry = self
                .codex_sessions
                .misses
                .entry(terminal_id.clone())
                .or_insert((0, now));
            entry.0 = entry.0.saturating_add(1);
            entry.1 = now + codex_probe_backoff(entry.0);
        }
    }

    /// Codex panes the probe may fill: no session at all, or the thread the
    /// probe itself reported last.
    pub(crate) fn codex_session_candidates(&self) -> Vec<CodexProbeCandidate> {
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for workspace in &self.state.workspaces {
            for tab in &workspace.tabs {
                for pane_id in tab.panes.keys() {
                    let Some(terminal_id) = tab.terminal_id(*pane_id) else {
                        continue;
                    };
                    if !seen.insert(terminal_id.clone()) {
                        continue;
                    }
                    if let Some(resolved) = self.codex_session_probe_state(terminal_id) {
                        candidates.push(CodexProbeCandidate {
                            terminal_id: terminal_id.clone(),
                            resolved,
                        });
                    }
                }
            }
        }
        candidates
    }

    /// `Some(resolved)` when the terminal runs Codex and its session may be
    /// set by the probe.
    fn codex_session_probe_state(&self, terminal_id: &TerminalId) -> Option<bool> {
        let terminal = self.state.terminals.get(terminal_id)?;
        if terminal.detected_agent != Some(crate::detect::Agent::Codex)
            || terminal.suspended_agent.is_some()
        {
            return None;
        }
        match terminal.persistable_agent_session() {
            None => Some(false),
            Some(session)
                if session.source == CODEX_SOURCE
                    && session.agent == CODEX_AGENT
                    && self.codex_sessions.reported.get(terminal_id)
                        == Some(&session.session_ref.value) =>
            {
                Some(true)
            }
            Some(_) => None,
        }
    }

    /// Drop probe reports for terminals that are gone, stopped running
    /// Codex, or now show a session from another source, and the backoff of
    /// terminals that are no candidates any more.
    fn forget_codex_session_reports(&mut self, candidates: &[CodexProbeCandidate]) {
        let all: HashSet<&TerminalId> = candidates
            .iter()
            .map(|candidate| &candidate.terminal_id)
            .collect();
        self.codex_sessions
            .misses
            .retain(|terminal_id, _| all.contains(terminal_id));
        let live: HashSet<&TerminalId> = candidates
            .iter()
            .filter(|candidate| candidate.resolved)
            .map(|candidate| &candidate.terminal_id)
            .collect();
        self.codex_sessions
            .reported
            .retain(|terminal_id, _| live.contains(terminal_id));
    }

    /// Apply probe matches as `herdr:codex` session reports. A result is
    /// dropped when its pane no longer qualifies (the agent exited, a hook
    /// reported meanwhile) or already shows that thread.
    pub(crate) fn apply_codex_session_results(
        &mut self,
        resolved: Vec<(TerminalId, String)>,
    ) -> bool {
        let mut changed = false;
        for (terminal_id, thread_id) in resolved {
            if self.codex_session_probe_state(&terminal_id).is_none() {
                continue;
            }
            let current = self
                .state
                .terminals
                .get(&terminal_id)
                .and_then(|terminal| terminal.persistable_agent_session())
                .map(|session| session.session_ref.value);
            if current.as_deref() == Some(thread_id.as_str()) {
                continue;
            }
            let Some(session_ref) = crate::agent_resume::AgentSessionRef::id(thread_id.clone())
            else {
                continue;
            };
            let Some(pane_id) = self.pane_id_for_terminal(&terminal_id) else {
                continue;
            };
            let seq = u64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0),
            )
            .unwrap_or(u64::MAX);
            self.handle_internal_event(crate::events::AppEvent::AgentSessionReported {
                pane_id,
                source: CODEX_SOURCE.into(),
                agent_label: CODEX_AGENT.into(),
                seq: Some(seq),
                session_ref: Some(session_ref),
                session_start_source: Some("startup".into()),
                transcript_path: None,
            });
            let applied = self
                .state
                .terminals
                .get(&terminal_id)
                .and_then(|terminal| terminal.persistable_agent_session())
                .is_some_and(|session| {
                    session.source == CODEX_SOURCE && session.session_ref.value == thread_id
                });
            if applied {
                tracing::info!(
                    event = "agent.session.codex_rollout",
                    terminal_id = %terminal_id,
                    thread_id = %thread_id,
                    replaced = current.is_some(),
                    "codex thread id recorded from its rollout file"
                );
                self.codex_sessions.reported.insert(terminal_id, thread_id);
                changed = true;
            }
        }
        changed
    }

    fn pane_id_for_terminal(&self, terminal_id: &TerminalId) -> Option<crate::layout::PaneId> {
        self.state.workspaces.iter().find_map(|workspace| {
            workspace.tabs.iter().find_map(|tab| {
                tab.panes
                    .keys()
                    .find(|pane_id| tab.terminal_id(**pane_id) == Some(terminal_id))
                    .copied()
            })
        })
    }
}

/// The wait before probing a pane again after `misses` probes in a row
/// found nothing: the probe interval, doubled each time, capped.
fn codex_probe_backoff(misses: u32) -> Duration {
    let doublings = misses.saturating_sub(1).min(8);
    CODEX_SESSION_PROBE_INTERVAL
        .saturating_mul(1 << doublings)
        .min(CODEX_SESSION_MAX_BACKOFF)
}

/// The background half: find each pane's Codex process, then match threads.
fn run_codex_probe(
    sessions_root: &std::path::Path,
    requests: Vec<CodexProbeRequest>,
    mut cache: RolloutCache,
) -> CodexProbeOutcome {
    let probed = requests
        .iter()
        .map(|request| request.terminal_id.clone())
        .collect();
    let processes: Vec<CodexProcess<TerminalId>> = requests
        .into_iter()
        .filter_map(|request| codex_process(request.terminal_id, request.shell_pid))
        .collect();
    let resolved = crate::codex_sessions::resolve_threads(
        sessions_root,
        &processes,
        crate::codex_sessions::now_unix_ms(),
        &mut cache,
    );
    CodexProbeOutcome {
        cache,
        probed,
        resolved: resolved.into_iter().collect(),
    }
}

/// The Codex process in a pane's foreground job. An npm install runs a node
/// launcher with the native `codex` as its child; the earliest-started
/// `codex` process is the one that created the thread.
fn codex_process(terminal_id: TerminalId, shell_pid: u32) -> Option<CodexProcess<TerminalId>> {
    let job = crate::detect::foreground_job(shell_pid)?;
    let (agent, _) = crate::detect::identify_agent_in_job(&job)?;
    if agent != crate::detect::Agent::Codex {
        return None;
    }
    let (pid, start_ms, args) = job
        .processes
        .iter()
        .filter(|process| is_codex_process(process))
        .filter_map(|process| {
            let start_ms = crate::platform::process_start_unix_ms(process.pid)?;
            let args = process
                .argv
                .as_ref()
                .map(|argv| argv.iter().skip(1).cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            Some((process.pid, start_ms, args))
        })
        .min_by_key(|(_, start_ms, _)| *start_ms)?;
    let cwd: PathBuf = crate::platform::process_cwd(pid)?;
    Some(CodexProcess {
        key: terminal_id,
        start_ms,
        cwd: crate::codex_sessions::canonical_dir(&cwd),
        exec: crate::codex_sessions::is_exec_invocation(&args),
    })
}

fn is_codex_process(process: &crate::platform::ForegroundProcess) -> bool {
    let is_codex_name = |name: &str| {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        base.eq_ignore_ascii_case("codex") || base.eq_ignore_ascii_case("codex.exe")
    };
    is_codex_name(&process.name) || process.argv0.as_deref().is_some_and(is_codex_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    const THREAD_A: &str = "01a1022f-b35b-7c12-81ea-bafbc3b8c3c1";
    const THREAD_B: &str = "01a10230-fa11-7862-ad37-ec47da81baa3";

    fn test_app() -> App {
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    fn app_with_pane() -> (App, TerminalId) {
        let mut app = test_app();
        let workspace = Workspace::test_new("codex");
        let pane = workspace.tabs[0].root_pane;
        let terminal_id = workspace
            .terminal_id(pane)
            .expect("root pane terminal")
            .clone();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        (app, terminal_id)
    }

    fn run_codex(app: &mut App, terminal_id: &TerminalId) {
        app.state
            .terminals
            .get_mut(terminal_id)
            .expect("terminal")
            .set_detected_agent_process_at(crate::detect::Agent::Codex, Instant::now());
    }

    fn session_value(app: &App, terminal_id: &TerminalId) -> Option<(String, String)> {
        app.state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.persistable_agent_session())
            .map(|session| (session.source, session.session_ref.value))
    }

    #[test]
    fn only_codex_panes_are_probe_candidates() {
        let (mut app, terminal_id) = app_with_pane();
        assert!(app.codex_session_candidates().is_empty());
        run_codex(&mut app, &terminal_id);
        assert_eq!(
            app.codex_session_candidates(),
            vec![CodexProbeCandidate {
                terminal_id,
                resolved: false
            }]
        );
    }

    #[test]
    fn a_matched_thread_becomes_the_pane_codex_session_and_resumes_by_id() {
        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        assert!(app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_A.into())]));
        assert_eq!(
            session_value(&app, &terminal_id),
            Some(("herdr:codex".into(), THREAD_A.into()))
        );
        let session = app.state.terminals[&terminal_id]
            .persistable_agent_session()
            .expect("session");
        let plan = crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
            .expect("codex resume plan");
        assert_eq!(plan.argv, vec!["codex", "resume", THREAD_A]);
        assert_eq!(
            app.codex_session_candidates(),
            vec![CodexProbeCandidate {
                terminal_id,
                resolved: true
            }]
        );
    }

    #[test]
    fn a_newer_thread_replaces_a_probe_reported_one() {
        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_A.into())]);
        assert!(app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_B.into())]));
        assert_eq!(
            session_value(&app, &terminal_id),
            Some(("herdr:codex".into(), THREAD_B.into()))
        );
        // The same thread again is not a change.
        assert!(!app.apply_codex_session_results(vec![(terminal_id, THREAD_B.into())]));
    }

    #[test]
    fn hook_and_launch_sessions_are_never_replaced() {
        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_managed_agent_launch_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("launched-thread")
                    .expect("id"),
                transcript_path: None,
            });
        assert!(app.codex_session_candidates().is_empty());
        assert!(!app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_A.into())]));
        assert_eq!(
            session_value(&app, &terminal_id),
            Some(("herdr:codex".into(), "launched-thread".into()))
        );
    }

    #[test]
    fn results_for_panes_that_stopped_running_codex_are_dropped() {
        let (mut app, terminal_id) = app_with_pane();
        assert!(!app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_A.into())]));
        assert_eq!(session_value(&app, &terminal_id), None);
    }

    #[test]
    fn reports_are_forgotten_once_the_pane_shows_another_session() {
        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        app.apply_codex_session_results(vec![(terminal_id.clone(), THREAD_A.into())]);
        // The integration hook reports a resumed thread.
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("hook-thread").expect("id"),
                transcript_path: None,
            });
        let candidates = app.codex_session_candidates();
        assert!(candidates.is_empty());
        app.forget_codex_session_reports(&candidates);
        assert!(app.codex_sessions.reported.is_empty());
    }

    #[test]
    fn the_tick_arms_its_deadline_and_skips_panes_without_a_process() {
        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        let now = Instant::now();
        assert_eq!(app.next_codex_session_probe_deadline(), None);
        assert!(!app.handle_codex_session_probe(now));
        assert_eq!(
            app.next_codex_session_probe_deadline(),
            Some(now + CODEX_SESSION_PROBE_INTERVAL)
        );
        let later = now + CODEX_SESSION_PROBE_INTERVAL;
        assert!(!app.handle_codex_session_probe(later));
        // Test terminals have no runtime, so nothing is probed.
        assert!(app.codex_sessions.thread.is_none());
        assert_eq!(
            app.next_codex_session_probe_deadline(),
            Some(later + CODEX_SESSION_PROBE_INTERVAL)
        );
    }

    #[test]
    fn panes_that_never_resolve_are_probed_less_and_less_often() {
        assert_eq!(codex_probe_backoff(1), CODEX_SESSION_PROBE_INTERVAL);
        assert_eq!(codex_probe_backoff(2), CODEX_SESSION_PROBE_INTERVAL * 2);
        assert_eq!(codex_probe_backoff(3), CODEX_SESSION_PROBE_INTERVAL * 4);
        assert_eq!(codex_probe_backoff(40), CODEX_SESSION_MAX_BACKOFF);

        let (mut app, terminal_id) = app_with_pane();
        run_codex(&mut app, &terminal_id);
        let now = Instant::now();
        let probed = [terminal_id.clone()];
        app.note_codex_probe_misses(&probed, &[], now);
        app.note_codex_probe_misses(&probed, &[], now);
        assert_eq!(
            app.codex_sessions.misses.get(&terminal_id),
            Some(&(2, now + CODEX_SESSION_PROBE_INTERVAL * 2))
        );
        // a found thread clears the backoff
        app.note_codex_probe_misses(&probed, &[(terminal_id.clone(), THREAD_A.into())], now);
        assert!(app.codex_sessions.misses.is_empty());
        // and a pane that stops running Codex forgets it
        app.note_codex_probe_misses(&probed, &[], now);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_detected_state(None, crate::detect::AgentState::Unknown);
        let candidates = app.codex_session_candidates();
        app.forget_codex_session_reports(&candidates);
        assert!(app.codex_sessions.misses.is_empty());
    }

    #[test]
    fn codex_processes_are_recognised_by_name_or_argv0() {
        let process = |name: &str, argv0: Option<&str>| crate::platform::ForegroundProcess {
            pid: 1,
            name: name.into(),
            argv0: argv0.map(str::to_string),
            argv: None,
            cmdline: None,
        };
        assert!(is_codex_process(&process("codex", None)));
        assert!(is_codex_process(&process(
            "node",
            Some("/opt/homebrew/lib/node_modules/@openai/codex/vendor/codex")
        )));
        assert!(!is_codex_process(&process("node", Some("node"))));
        assert!(!is_codex_process(&process("codex-code-mode-host", None)));
    }
}
