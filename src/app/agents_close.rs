//! `agents.close_tab` and `agents.reopen_tab` (fork, agents v2): the tab's
//! agents exit gracefully (suspend), the tab closes, and every agent stays
//! resumable from the closed sessions.
//!
//! Nothing is partially closed: any refusal refuses the whole close. A close
//! that needs exits replies `closing` and finishes on the scheduler tick
//! ([`App::drive_pending_agent_closes`]); client input or a working edge on
//! a target aborts it, and an aborted close never signals anything. The
//! caller's own tab is deferred until the caller is stably idle.

use std::time::{Duration, Instant};

use crate::agents_model::policy::{self, Action};
use crate::api::schema::agents_model::{
    error_code, AgentsActionOutcome, AgentsCloseOutcome, AgentsCloseResult, AgentsCloseTabParams,
    AgentsClosedInfo, AgentsReopenResult, AgentsReopenTabParams, AgentsResumableInfo,
    AgentsUnresumableInfo, AgentsWho,
};
use crate::api::schema::{Method, ResponseResult};
use crate::terminal::TerminalId;

use super::agent_suspend::{AgentSuspendError, NotSuspendableReason};
use super::agents_model::{now_unix, LogLine, ModelCaller, ModelError, ModelResult, TabTarget};
use super::App;

/// An idle (or done) agent counts as stably idle after this long, unless a
/// hook reported the idle.
pub(crate) const STABLE_IDLE: Duration = Duration::from_secs(3);
/// A closing tab waits this long for its agents' exits.
pub(crate) const CLOSE_DEADLINE: Duration = Duration::from_secs(15);
/// A self deferral expires after this long.
pub(crate) const DEFERRAL_TTL: Duration = Duration::from_secs(10 * 60);
/// How often the tick looks at pending closes and deferrals.
const CLOSE_POLL: Duration = Duration::from_millis(500);

/// The closes in flight.
#[derive(Debug, Default)]
pub(crate) struct AgentCloses {
    pending: Vec<PendingAgentClose>,
    deferrals: Vec<CloseDeferral>,
}

impl AgentCloses {
    pub(crate) fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.deferrals.is_empty()
    }
}

/// A close waiting for its agents' exits.
#[derive(Debug, Clone)]
struct PendingAgentClose {
    caller: ModelCaller,
    /// Every terminal of the tab (a tab is found again by them).
    tab_terminals: Vec<TerminalId>,
    /// The terminals whose agents were asked to exit.
    exits: Vec<TerminalId>,
    requested_at: Instant,
    deadline: Instant,
}

/// The caller's own tab, closed once the caller is stably idle.
#[derive(Debug, Clone)]
struct CloseDeferral {
    caller: ModelCaller,
    params: AgentsCloseTabParams,
    tab_terminals: Vec<TerminalId>,
    requested_at: Instant,
    requested_unix: u64,
    expires: Instant,
}

/// What a close needs, after every check passed.
struct ClosePlan {
    tab_terminals: Vec<TerminalId>,
    /// Agents to ask to exit (by public pane id) and their terminals.
    exits: Vec<(String, TerminalId)>,
    resumable: Vec<AgentsResumableInfo>,
    unresumable: Vec<AgentsUnresumableInfo>,
}

fn busy(name: &str, why: &str) -> ModelError {
    ModelError::new(error_code::TARGET_BUSY, format!("{name} {why}"))
}

impl App {
    pub(super) fn handle_agents_close_tab(
        &mut self,
        id: String,
        params: AgentsCloseTabParams,
    ) -> String {
        let result = self.agents_close_tab(&params);
        Self::model_reply(
            id,
            result.map(|close| ResponseResult::AgentsCloseTab { close }),
        )
    }

    fn agents_close_tab(
        &mut self,
        params: &AgentsCloseTabParams,
    ) -> ModelResult<AgentsCloseResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let line = self.tab_line("close_tab", target);
        self.authorize_on_tab(&caller, target, Action::Close, &line)?;
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .unwrap_or_default();
        let own_tab = caller.ws_idx == target.ws_idx && caller.tab_idx == target.tab_idx;
        if own_tab && caller.live {
            // The caller is working (it is calling): close once it is
            // stably idle. A keystroke in its pane cancels.
            let now = Instant::now();
            self.agents_model.closes.deferrals.push(CloseDeferral {
                caller: caller.clone(),
                params: params.clone(),
                tab_terminals: self.tab_terminals(target),
                requested_at: now,
                requested_unix: now_unix(),
                expires: now + DEFERRAL_TTL,
            });
            self.sync_agents_close_deadline(now);
            self.log_model_action(Some(&caller), AgentsActionOutcome::Deferred, line);
            return Ok(AgentsCloseResult {
                tab_id,
                outcome: AgentsCloseOutcome::Deferred,
                resumable: Vec::new(),
                unresumable: Vec::new(),
                closed_ids: Vec::new(),
            });
        }
        self.close_or_start(&caller, target, params, None, line)
    }

    /// Plan the close; close now when no exit is needed, else ask the
    /// agents to exit and finish on the tick.
    fn close_or_start(
        &mut self,
        caller: &ModelCaller,
        target: TabTarget,
        params: &AgentsCloseTabParams,
        since: Option<Instant>,
        line: LogLine,
    ) -> ModelResult<AgentsCloseResult> {
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .unwrap_or_default();
        let plan = match self.close_plan(target, params, since, Some(caller)) {
            Ok(plan) => plan,
            Err(err) => {
                let mut denied = line;
                denied.code = Some(err.code.clone());
                denied.detail = Some(err.message.clone());
                self.log_model_action(Some(caller), AgentsActionOutcome::Denied, denied);
                return Err(err);
            }
        };
        if plan.exits.is_empty() {
            let closed_ids = self.close_tab_as(caller, &tab_id)?;
            let mut done = line;
            done.closed_ids = closed_ids.clone();
            self.log_model_action(Some(caller), AgentsActionOutcome::Ok, done);
            return Ok(AgentsCloseResult {
                tab_id,
                outcome: AgentsCloseOutcome::Closed,
                resumable: plan.resumable,
                unresumable: plan.unresumable,
                closed_ids,
            });
        }
        let by = Some(caller.who());
        let mut asked = Vec::new();
        for (pane, terminal_id) in &plan.exits {
            match self.suspend_agent_by(pane, by.clone()) {
                Ok(_) => asked.push(terminal_id.clone()),
                Err(_) => {
                    // Undo what was asked so far: nothing is partially closed.
                    self.abort_exits(&asked);
                    let err = busy(pane, "could not be asked to exit; retry in a few seconds");
                    let mut failed = line;
                    failed.code = Some(err.code.clone());
                    self.log_model_action(Some(caller), AgentsActionOutcome::Failed, failed);
                    return Err(err);
                }
            }
        }
        let now = Instant::now();
        self.agents_model.closes.pending.push(PendingAgentClose {
            caller: caller.clone(),
            tab_terminals: plan.tab_terminals,
            exits: asked,
            requested_at: now,
            deadline: now + CLOSE_DEADLINE,
        });
        self.sync_agents_close_deadline(now);
        self.log_model_action(Some(caller), AgentsActionOutcome::Closing, line);
        Ok(AgentsCloseResult {
            tab_id,
            outcome: AgentsCloseOutcome::Closing,
            resumable: plan.resumable,
            unresumable: plan.unresumable,
            closed_ids: Vec::new(),
        })
    }

    fn tab_terminals(&self, target: TabTarget) -> Vec<TerminalId> {
        self.state
            .workspaces
            .get(target.ws_idx)
            .and_then(|ws| ws.tabs.get(target.tab_idx))
            .map(|tab| {
                tab.layout
                    .pane_ids()
                    .into_iter()
                    .filter_map(|pane| tab.terminal_id(pane).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The tab holding these terminals now (it may have moved).
    fn tab_of_terminals(&self, terminals: &[TerminalId]) -> Option<TabTarget> {
        let (ws_idx, tab_idx, _) = terminals
            .iter()
            .find_map(|terminal| self.locate_terminal(terminal))?;
        Some(TabTarget {
            ws_idx,
            tab_idx,
            pane_id: None,
        })
    }

    /// Every check of the close, pane by pane (design §5 step 2).
    fn close_plan(
        &self,
        target: TabTarget,
        params: &AgentsCloseTabParams,
        since: Option<Instant>,
        caller: Option<&ModelCaller>,
    ) -> ModelResult<ClosePlan> {
        let now = Instant::now();
        let ws = self
            .state
            .workspaces
            .get(target.ws_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        let tab = ws
            .tabs
            .get(target.tab_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        let mut plan = ClosePlan {
            tab_terminals: Vec::new(),
            exits: Vec::new(),
            resumable: Vec::new(),
            unresumable: Vec::new(),
        };
        for pane in tab.layout.pane_ids() {
            let Some(pane_state) = tab.panes.get(&pane) else {
                continue;
            };
            let terminal_id = pane_state.attached_terminal_id.clone();
            let Some(terminal) = self.state.terminals.get(&terminal_id) else {
                continue;
            };
            plan.tab_terminals.push(terminal_id.clone());
            let public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
            let name = self
                .model_agent_name(target.ws_idx, pane)
                .unwrap_or_else(|| public.clone());
            let self_pane = caller.is_some_and(|caller| caller.terminal_id == terminal_id);
            let typed_since = since.is_some_and(|since| {
                terminal
                    .turn()
                    .last_client_input()
                    .is_some_and(|at| at > since)
            });
            if !self_pane && (self.pane_user_typing(target.ws_idx, pane) || typed_since) {
                return Err(ModelError::new(
                    error_code::USER_TYPING,
                    format!("your user is typing in {name}; ask again later"),
                ));
            }
            if !terminal.is_agent_terminal() {
                if !params.force && self.shell_runs_a_job(target.ws_idx, pane) {
                    return Err(ModelError::new(
                        error_code::SHELL_BUSY,
                        format!("{public} runs a job; pass force to close it anyway"),
                    ));
                }
                continue;
            }
            let agent = self
                .model_agent_kind(target.ws_idx, pane)
                .unwrap_or_default();
            if terminal.suspended_agent.is_some() {
                // Its closed entry is resumable.
                plan.resumable
                    .push(self.resumable_info(&name, &agent, terminal));
                continue;
            }
            if terminal.managed_agent_launch_pending() {
                return Err(busy(&name, "is still starting"));
            }
            if terminal.active_subagent_count() > 0
                || matches!(
                    terminal.state,
                    crate::detect::AgentState::Working | crate::detect::AgentState::Blocked
                )
            {
                return Err(busy(&name, "is working; message it or wait"));
            }
            let hook = terminal.full_lifecycle_hook_authority_active();
            let stable = hook
                || terminal.turn().last_idle().is_none_or(|(at, hook_idle)| {
                    hook_idle || now.saturating_duration_since(at) >= STABLE_IDLE
                });
            if !stable {
                return Err(busy(&name, "may be between steps; retry in a few seconds"));
            }
            match self.check_suspend(&public) {
                Ok(()) => {
                    plan.resumable
                        .push(self.resumable_info(&name, &agent, terminal));
                    plan.exits.push((public, terminal_id));
                }
                Err(AgentSuspendError::NotSuspendable { kind, reason, .. }) => match kind {
                    NotSuspendableReason::PaneProcess => {
                        // The agent is the pane's process: no exit step.
                        if terminal.persistable_agent_session().is_some() {
                            plan.resumable
                                .push(self.resumable_info(&name, &agent, terminal));
                        } else {
                            plan.unresumable.push(AgentsUnresumableInfo {
                                name,
                                agent,
                                reason,
                            });
                        }
                    }
                    NotSuspendableReason::NoSession | NotSuspendableReason::NoResumePlan => {
                        plan.unresumable.push(AgentsUnresumableInfo {
                            name,
                            agent,
                            reason,
                        });
                    }
                    NotSuspendableReason::NoGracefulExit => {
                        if !(params.allow_unresumable || params.force) {
                            return Err(ModelError::new(
                                error_code::NO_GRACEFUL_EXIT,
                                format!("{name}: {reason}; pass allow_unresumable or force to close it anyway"),
                            ));
                        }
                        plan.unresumable.push(AgentsUnresumableInfo {
                            name,
                            agent,
                            reason,
                        });
                    }
                },
                Err(_) => return Err(busy(&name, "is busy; retry in a few seconds")),
            }
        }
        if !plan.unresumable.is_empty() && !params.allow_unresumable {
            let names: Vec<String> = plan
                .unresumable
                .iter()
                .map(|entry| format!("{} ({}: {})", entry.name, entry.agent, entry.reason))
                .collect();
            return Err(ModelError::new(
                error_code::NOT_RESUMABLE,
                format!(
                    "could not be reopened after the close: {}; pass allow_unresumable to close anyway",
                    names.join(", ")
                ),
            ));
        }
        Ok(plan)
    }

    fn resumable_info(
        &self,
        name: &str,
        agent: &str,
        terminal: &crate::terminal::TerminalState,
    ) -> AgentsResumableInfo {
        AgentsResumableInfo {
            name: name.to_string(),
            agent: agent.to_string(),
            session: terminal
                .persistable_agent_session()
                .map(|session| session.session_ref.value)
                .unwrap_or_default(),
        }
    }

    /// Whether a shell pane's foreground job is something other than its
    /// shell (one process read, off any hot path).
    fn shell_runs_a_job(&self, ws_idx: usize, pane: crate::layout::PaneId) -> bool {
        self.lookup_runtime_sender(ws_idx, pane)
            .is_some_and(|runtime| super::agents::available_shell_name(runtime).is_none())
    }

    /// Close a tab as `caller` (the closed sessions record `closed_by`);
    /// the closed-session ids.
    fn close_tab_as(&mut self, caller: &ModelCaller, tab_id: &str) -> ModelResult<Vec<String>> {
        self.agents_model.close_actor = Some(caller.who());
        self.agents_model.last_closed_ids.borrow_mut().clear();
        let closed = self.call_method(Method::TabClose(crate::api::schema::TabTarget {
            tab_id: tab_id.to_string(),
        }));
        self.agents_model.close_actor = None;
        closed?;
        Ok(std::mem::take(
            &mut *self.agents_model.last_closed_ids.borrow_mut(),
        ))
    }

    /// Undo asked exits whose agent has not exited yet: the agent keeps
    /// running as if nothing was asked (no record, no escalation, no
    /// signal). Exited ones stay suspended and resumable.
    fn abort_exits(&mut self, exits: &[TerminalId]) {
        for terminal_id in exits {
            if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
                let exited = terminal
                    .suspended_agent
                    .as_ref()
                    .is_none_or(|record| record.exit_observed());
                if !exited {
                    terminal.take_suspended_agent();
                }
            }
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    /// The scheduler's next look at the closes in flight.
    pub(crate) fn sync_agents_close_deadline(&mut self, now: Instant) {
        self.state.agents_close_deadline =
            (!self.agents_model.closes.is_empty()).then_some(now + CLOSE_POLL);
    }

    /// Drive pending closes and deferrals; runs in the scheduler tick before
    /// the suspend escalation. Whether anything changed.
    pub(crate) fn drive_pending_agent_closes(&mut self, now: Instant) -> bool {
        if self.agents_model.closes.is_empty() {
            self.state.agents_close_deadline = None;
            return false;
        }
        let mut changed = false;
        let pending = std::mem::take(&mut self.agents_model.closes.pending);
        for close in pending {
            match self.drive_close(&close, now) {
                CloseStep::Wait => self.agents_model.closes.pending.push(close),
                CloseStep::Done => changed = true,
            }
        }
        let deferrals = std::mem::take(&mut self.agents_model.closes.deferrals);
        for deferral in deferrals {
            match self.drive_deferral(&deferral, now) {
                CloseStep::Wait => self.agents_model.closes.deferrals.push(deferral),
                CloseStep::Done => changed = true,
            }
        }
        self.sync_agents_close_deadline(now);
        changed
    }

    fn close_log_line(&self, action: &'static str, terminals: &[TerminalId]) -> LogLine {
        match self.tab_of_terminals(terminals) {
            Some(target) => self.tab_line(action, target),
            None => LogLine {
                action,
                ..LogLine::default()
            },
        }
    }

    fn drive_close(&mut self, close: &PendingAgentClose, now: Instant) -> CloseStep {
        let Some(target) = self.tab_of_terminals(&close.tab_terminals) else {
            // The tab went away by other means.
            return CloseStep::Done;
        };
        // Abort on client input or a working edge on any target.
        let interrupted = close.tab_terminals.iter().any(|terminal_id| {
            self.state
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| {
                    terminal
                        .turn()
                        .last_client_input()
                        .is_some_and(|at| at > close.requested_at)
                        || (terminal.suspended_agent.is_none()
                            && terminal.state == crate::detect::AgentState::Working)
                        || terminal.suspended_agent.as_ref().is_some_and(|record| {
                            !record.exit_observed()
                                && terminal.state == crate::detect::AgentState::Working
                                && terminal
                                    .turn()
                                    .last_client_input()
                                    .is_some_and(|at| at > close.requested_at)
                        })
                })
        });
        if interrupted {
            self.abort_exits(&close.exits);
            let mut line = self.close_log_line("close_tab", &close.tab_terminals);
            line.detail = Some("aborted: input or work in a target after the request".into());
            self.log_model_action(Some(&close.caller), AgentsActionOutcome::Aborted, line);
            return CloseStep::Done;
        }
        let all_exited = close.exits.iter().all(|terminal_id| {
            self.state
                .terminals
                .get(terminal_id)
                .is_some_and(|terminal| {
                    terminal
                        .suspended_agent
                        .as_ref()
                        .is_some_and(|record| record.exit_observed())
                })
        });
        if all_exited {
            let tab_id = self
                .public_tab_id(target.ws_idx, target.tab_idx)
                .unwrap_or_default();
            let mut line = self.tab_line("close_tab", target);
            match self.close_tab_as(&close.caller, &tab_id) {
                Ok(closed_ids) => {
                    line.closed_ids = closed_ids;
                    self.log_model_action(Some(&close.caller), AgentsActionOutcome::Ok, line);
                }
                Err(err) => {
                    line.code = Some(err.code);
                    line.detail = Some(err.message);
                    self.log_model_action(Some(&close.caller), AgentsActionOutcome::Failed, line);
                }
            }
            return CloseStep::Done;
        }
        if now >= close.deadline {
            self.abort_exits(&close.exits);
            let mut line = self.close_log_line("close_tab", &close.tab_terminals);
            line.detail = Some("the agents did not exit in time; nothing was closed".into());
            self.log_model_action(Some(&close.caller), AgentsActionOutcome::Failed, line);
            return CloseStep::Done;
        }
        CloseStep::Wait
    }

    fn drive_deferral(&mut self, deferral: &CloseDeferral, now: Instant) -> CloseStep {
        let Some(terminal) = self.state.terminals.get(&deferral.caller.terminal_id) else {
            return CloseStep::Done;
        };
        let log = |app: &mut Self, outcome: AgentsActionOutcome, detail: &str| {
            let mut line = app.close_log_line("close_tab", &deferral.tab_terminals);
            line.detail = Some(detail.to_string());
            app.log_model_action(Some(&deferral.caller), outcome, line);
        };
        if now >= deferral.expires {
            log(self, AgentsActionOutcome::Aborted, "deferred close expired");
            return CloseStep::Done;
        }
        // Any keystroke in the caller's pane may be the user's next prompt.
        if terminal
            .turn()
            .last_client_input()
            .is_some_and(|at| at > deferral.requested_at)
        {
            log(
                self,
                AgentsActionOutcome::Aborted,
                "deferred close cancelled: your user typed in the pane",
            );
            return CloseStep::Done;
        }
        let turn = terminal.turn().effective(
            crate::agents_model::turn::EdgeStatus::from(terminal.state),
            now,
            now_unix(),
        );
        if turn.user_turn() && turn.started_unix > deferral.requested_unix {
            log(
                self,
                AgentsActionOutcome::Aborted,
                "deferred close cancelled: a new user turn started",
            );
            return CloseStep::Done;
        }
        let idle = terminal.state == crate::detect::AgentState::Idle
            && terminal.active_subagent_count() == 0;
        let stable = idle
            && (terminal.full_lifecycle_hook_authority_active()
                || terminal.turn().last_idle().is_some_and(|(at, hook)| {
                    hook || now.saturating_duration_since(at) >= STABLE_IDLE
                }));
        if !stable {
            return CloseStep::Wait;
        }
        let Some(target) = self.tab_of_terminals(&deferral.tab_terminals) else {
            return CloseStep::Done;
        };
        let line = self.tab_line("close_tab", target);
        // Steps 2-4 now that the caller is idle; its own pane exits too.
        // A refusal is logged by `close_or_start`.
        let _ = self.close_or_start(
            &deferral.caller,
            target,
            &deferral.params,
            Some(deferral.requested_at),
            line,
        );
        CloseStep::Done
    }

    // ----- reopen ------------------------------------------------------------

    pub(super) fn handle_agents_reopen_tab(
        &mut self,
        id: String,
        params: AgentsReopenTabParams,
    ) -> String {
        let result = self.agents_reopen_tab(&params);
        Self::model_reply(
            id,
            result.map(|reopen| ResponseResult::AgentsReopenTab { reopen }),
        )
    }

    fn agents_reopen_tab(
        &mut self,
        params: &AgentsReopenTabParams,
    ) -> ModelResult<AgentsReopenResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        if !self.policy.persist_session {
            return Err(ModelError::new(
                error_code::NOT_FOUND,
                "no closed sessions here",
            ));
        }
        let store = crate::persist::closed_sessions::store_path();
        let entry = crate::persist::closed_sessions::load(&store)
            .into_iter()
            .find(|entry| entry.id == params.closed_id)
            .ok_or_else(|| {
                ModelError::new(
                    error_code::NOT_FOUND,
                    format!("closed session {} not found", params.closed_id),
                )
            })?;
        let side = crate::persist::closed_sessions::load_agents(&store)
            .remove(&crate::persist::closed_sessions::session_key(&entry))
            .unwrap_or_default();
        let by_agent = side.closed_by.as_ref().is_some_and(AgentsWho::is_agent);
        let dest_ws = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == entry.space_id);
        let dest = self.model_dest(&caller, dest_ws);
        let line = LogLine {
            action: "reopen_tab",
            target_name: Some(entry.title().to_string()),
            detail: Some(format!("closed session {}", entry.id)),
            ..LogLine::default()
        };
        let facts = self.model_facts(&caller, entry.title().to_string(), dest_ws);
        self.model_authorize(
            &caller,
            policy::Relation::SelfPane,
            Action::SoftEdit(policy::SoftEdit::Reopen { by_agent, dest }),
            &facts,
            &line,
        )?;
        let capped =
            matches!(caller.actor, policy::Actor::Agent { .. }) && !caller.turn.user_turn();
        if capped {
            if !self
                .agents_model
                .limiter
                .check_spawn(&caller.terminal_id, now_unix())
            {
                return Err(ModelError::new(
                    error_code::SPAWN_LIMIT,
                    "spawn limit (tabs per hour); ask your user",
                ));
            }
            self.agents_model
                .limiter
                .record_spawn(&caller.terminal_id, now_unix());
        }
        let result = self.call_method(Method::SessionClosedReopen(
            crate::api::schema::ClosedSessionTarget {
                id: params.closed_id.clone(),
            },
        ));
        let result = match result {
            Ok(result) => result,
            Err(err) => {
                if capped {
                    self.agents_model.limiter.release_spawn(&caller.terminal_id);
                }
                let mut failed = line;
                failed.code = Some(err.code.clone());
                self.log_model_action(Some(&caller), AgentsActionOutcome::Failed, failed);
                return Err(err);
            }
        };
        let ResponseResult::TabCreated { tab, root_pane } = result else {
            return Err(ModelError::new(
                error_code::FAILED,
                "unexpected reopen reply",
            ));
        };
        let mut done = line;
        done.target_tab = Some(tab.tab_id.clone());
        done.target_pane = Some(root_pane.pane_id.clone());
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, done);
        Ok(AgentsReopenResult {
            tab_id: tab.tab_id,
            pane_ids: vec![root_pane.pane_id],
            workspace_id: Some(tab.workspace_id),
        })
    }

    /// The last few closed tabs in spaces the caller sees (opt-in: it reads
    /// the closed-sessions store from disk).
    pub(crate) fn recently_closed_for(
        &self,
        _caller: Option<&ModelCaller>,
    ) -> Vec<AgentsClosedInfo> {
        if !self.policy.persist_session {
            return Vec::new();
        }
        let store = crate::persist::closed_sessions::store_path();
        let side = crate::persist::closed_sessions::load_agents(&store);
        crate::persist::closed_sessions::load(&store)
            .into_iter()
            .take(5)
            .map(|entry| AgentsClosedInfo {
                closed_by: side
                    .get(&crate::persist::closed_sessions::session_key(&entry))
                    .and_then(|record| record.closed_by.clone()),
                closed_id: entry.id.clone(),
                label: entry.title().to_string(),
                workspace_id: entry.space_id.clone(),
                agents: vec![entry.agent.clone()],
                closed_at: entry.closed_at,
            })
            .collect()
    }
}

enum CloseStep {
    Wait,
    Done,
}
