//! The agents model's App side (fork, agents v2): input provenance, the
//! turn origin, the actor, the one permission check and the `agents.*`
//! handlers. The close flow lives in `agents_close.rs`, the `managed.json`
//! migration in `agents_migrate.rs`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::agents_model::actions_log::{self, AgentActionEntry};
use crate::agents_model::limits::Limiter;
use crate::agents_model::policy::{self, Action, Actor, Decision, Dest, Facts, Relation};
use crate::agents_model::reply_index::ReplyIndex;
use crate::agents_model::turn::{EdgeStatus, EffectiveTurn};
use crate::agents_model::{InputSource, TurnOrigin};
use crate::api::schema::agents_model::{
    error_code, AgentActorKind, AgentPaneKind, AgentsAccess, AgentsActionOutcome,
    AgentsActionsParams, AgentsActorInfo, AgentsActorParams, AgentsCheckAction, AgentsCheckParams,
    AgentsCheckpointParams, AgentsDirectory, AgentsDirectoryParams, AgentsGroupInfo,
    AgentsGroupTeam, AgentsLifecycleParams, AgentsMessageOutcome, AgentsMessageResult,
    AgentsMoveResult, AgentsMoveTabParams, AgentsNotesAppendParams, AgentsOpenResult,
    AgentsOpenTabParams, AgentsOriginDetail, AgentsPaneInfo, AgentsReadFormat, AgentsReadParams,
    AgentsReadResult, AgentsReadSource, AgentsRenameResult, AgentsRenameTabParams,
    AgentsScreenAccess, AgentsSendMessageParams, AgentsSetMetaParams, AgentsSetMetaResult,
    AgentsTabInfo, AgentsTeamRef, AgentsWho,
};
use crate::api::schema::{ErrorBody, Method, Request, ResponseResult};
use crate::layout::PaneId;
use crate::terminal::TerminalId;

use super::api::responses::{encode_error, encode_success};
use super::App;

/// The most tab rows `agents.directory` returns by default.
pub(crate) const DIRECTORY_DEFAULT_LIMIT: u32 = 40;
/// The longest note (characters, one line).
pub(crate) const NOTE_MAX_CHARS: usize = 200;
/// The longest message text (characters).
pub(crate) const MESSAGE_MAX_CHARS: usize = 4000;
/// The longest tab or group label an agent sets.
const LABEL_MAX_CHARS: usize = 64;

/// The agents model's runtime state on the App (not persisted).
#[derive(Debug, Default)]
pub(crate) struct AgentsModelRuntime {
    /// Where `actions.jsonl` and `messages.jsonl` live (the coordinator
    /// directory); `None` keeps the logs off (tests, unpersisted sessions).
    pub(crate) dir: Option<PathBuf>,
    pub(crate) limiter: Limiter,
    /// Seeded from the message log on first use.
    pub(crate) reply_index: Option<ReplyIndex>,
    pub(crate) closes: super::agents_close::AgentCloses,
    pub(crate) migration: super::agents_migrate::MigrationState,
    /// Who is closing tabs right now (an agents-model close); the closed
    /// session records take it as `closed_by`. `None` is the user.
    pub(crate) close_actor: Option<AgentsWho>,
    /// Side records of closed sessions between their entry and their write
    /// (the record functions take `&self`).
    pub(crate) closing_records:
        RefCell<HashMap<String, crate::persist::closed_sessions::ClosedAgentRecord>>,
    /// The closed-session ids the last record wrote.
    pub(crate) last_closed_ids: RefCell<Vec<String>>,
    /// Depth of in-process delegation ([`App::call_method`]): a delegated
    /// method is not logged again as a user action.
    pub(crate) delegating: u32,
}

impl AgentsModelRuntime {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            ..Self::default()
        }
    }
}

/// A refusal or failure of an `agents.*` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelError {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl ModelError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

pub(crate) type ModelResult<T> = Result<T, ModelError>;

/// Who is calling, from the server's own records.
#[derive(Debug, Clone)]
pub(crate) struct ModelCaller {
    pub(crate) ws_idx: usize,
    pub(crate) tab_idx: usize,
    pub(crate) pane_id: PaneId,
    pub(crate) terminal_id: TerminalId,
    /// The canonical public pane id.
    pub(crate) public: String,
    pub(crate) actor: Actor,
    pub(crate) kind: AgentActorKind,
    /// A live agent runs in the pane.
    pub(crate) live: bool,
    /// The agent name, else the agent kind, else the pane id.
    pub(crate) name: String,
    pub(crate) agent: Option<String>,
    /// The caller's team group, when it is a (not excluded) member.
    pub(crate) team_ws: Option<usize>,
    pub(crate) turn: EffectiveTurn,
}

impl ModelCaller {
    pub(crate) fn who(&self) -> AgentsWho {
        match self.actor {
            Actor::User => AgentsWho::User,
            Actor::Coordinator => AgentsWho::Coordinator,
            Actor::Agent { .. } => AgentsWho::Agent {
                name: self.name.clone(),
            },
        }
    }
}

/// A target tab (and the pane the target named, if any).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TabTarget {
    pub(crate) ws_idx: usize,
    pub(crate) tab_idx: usize,
    pub(crate) pane_id: Option<PaneId>,
}

/// One action-log line in the making.
#[derive(Debug, Clone, Default)]
pub(crate) struct LogLine {
    pub(crate) action: &'static str,
    pub(crate) target_tab: Option<String>,
    pub(crate) target_pane: Option<String>,
    pub(crate) target_name: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) closed_ids: Vec<String>,
}

pub(crate) fn now_unix() -> u64 {
    crate::notes::now_unix()
}

/// One line of plain text, at most `max` characters.
pub(crate) fn one_line(value: &str, max: usize) -> String {
    crate::coordinator::one_line(value, max)
}

impl App {
    // ----- input provenance and the turn ----------------------------------

    /// Record where input written into a terminal came from, next to the
    /// write. No allocation for client input, no terminal-core lock, no
    /// event, no render.
    pub(crate) fn note_input(&mut self, terminal_id: &TerminalId, source: InputSource) {
        if matches!(source, InputSource::Internal) {
            return;
        }
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.turn_mut().note_input(&source, Instant::now());
        }
    }

    /// [`Self::note_input`] for a pane.
    pub(crate) fn note_pane_input(&mut self, ws_idx: usize, pane_id: PaneId, source: InputSource) {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .map(|pane| &pane.attached_terminal_id)
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.turn_mut().note_input(&source, Instant::now());
        }
    }

    /// A pane state update's status edge, for the turn origin. O(tabs).
    /// What it did to the turn (the automatic checkpoints read it).
    pub(crate) fn note_turn_edge(
        &mut self,
        update: &crate::app::actions::PaneStateUpdate,
    ) -> crate::agents_model::turn::TurnEdge {
        use crate::agents_model::turn::TurnEdge;
        if update.previous_state == update.state {
            return TurnEdge::None;
        }
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(update.ws_idx)
            .and_then(|ws| ws.pane_state(update.pane_id))
            .map(|pane| &pane.attached_terminal_id)
        else {
            return TurnEdge::None;
        };
        let Some(terminal) = self.state.terminals.get_mut(terminal_id) else {
            return TurnEdge::None;
        };
        let hook = terminal.full_lifecycle_hook_authority_active();
        terminal.turn_mut().on_status_edge(
            EdgeStatus::from(update.previous_state),
            EdgeStatus::from(update.state),
            hook,
            Instant::now(),
            now_unix(),
        )
    }

    /// The effective turn of a terminal now.
    pub(crate) fn effective_turn(&self, terminal_id: &TerminalId) -> Option<EffectiveTurn> {
        let terminal = self.state.terminals.get(terminal_id)?;
        Some(terminal.turn().effective(
            EdgeStatus::from(terminal.state),
            Instant::now(),
            now_unix(),
        ))
    }

    /// Whether the pane's current turn is an effective user turn (§3.6):
    /// the coordinator's single turn check reads it.
    pub(crate) fn pane_user_turn(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane_id))
            .and_then(|pane| self.effective_turn(&pane.attached_terminal_id))
            .is_some_and(|turn| turn.user_turn())
    }

    /// Whether the pane's user is typing in it or holds an unsent draft in
    /// its agent's input box (the fork's typing guard, app::typing_guard).
    pub(crate) fn pane_user_typing(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return false;
        };
        let agent = self
            .model_terminal(ws_idx, pane_id)
            .and_then(crate::terminal::TerminalState::effective_known_agent);
        crate::app::typing_guard::runtime_typing_block(agent, runtime, Instant::now()).is_some()
    }

    // ----- lookups -----------------------------------------------------------

    pub(crate) fn model_terminal(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<&crate::terminal::TerminalState> {
        let pane = self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
        self.state.terminals.get(&pane.attached_terminal_id)
    }

    /// A live agent runs in the pane: detected, not parked, not starting.
    pub(crate) fn pane_hosts_live_agent(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.model_terminal(ws_idx, pane_id)
            .is_some_and(|terminal| {
                terminal.effective_agent_label().is_some()
                    && terminal.suspended_agent.is_none()
                    && !terminal.managed_agent_launch_pending()
            })
    }

    /// The pane's agent name, else its agent kind.
    pub(crate) fn model_agent_name(&self, ws_idx: usize, pane_id: PaneId) -> Option<String> {
        let terminal = self.model_terminal(ws_idx, pane_id)?;
        terminal
            .agent_name
            .clone()
            .or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .and_then(|record| record.name.clone())
            })
            .filter(|name| !name.trim().is_empty())
            .or_else(|| terminal.effective_agent_label().map(str::to_string))
            .or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .map(|record| record.agent.clone())
            })
    }

    pub(crate) fn model_agent_kind(&self, ws_idx: usize, pane_id: PaneId) -> Option<String> {
        let terminal = self.model_terminal(ws_idx, pane_id)?;
        terminal
            .effective_agent_label()
            .map(str::to_string)
            .or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .map(|record| record.agent.clone())
            })
    }

    /// A group's label as the sidebar shows it.
    pub(crate) fn group_label(&self, ws_idx: usize) -> String {
        self.state
            .workspaces
            .get(ws_idx)
            .map(|ws| {
                ws.custom_name
                    .clone()
                    .unwrap_or_else(|| ws.cached_auto_label.clone())
            })
            .unwrap_or_default()
    }

    pub(crate) fn tab_label(&self, ws_idx: usize, tab_idx: usize) -> String {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tab_display_name(tab_idx))
            .unwrap_or_default()
    }

    /// Whether the pane is a member of its group's team.
    pub(crate) fn pane_team_member(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.team.as_ref())
            .is_some_and(|team| team.is_member(pane_id))
    }

    /// Whether the pane is excluded from its group's team (in place, or by
    /// its meta after a close and reopen).
    pub(crate) fn pane_team_excluded(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return false;
        };
        let Some(team) = ws.team.as_ref() else {
            return false;
        };
        team.is_excluded(pane_id)
            || self
                .model_terminal(ws_idx, pane_id)
                .is_some_and(|terminal| {
                    terminal.agent_meta().team_excluded.as_deref() == Some(ws.id.as_str())
                })
    }

    /// The coordinator's tab: protected from every agent (D3).
    pub(crate) fn tab_is_protected(&self, ws_idx: usize, tab_idx: usize) -> bool {
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return false;
        };
        tab.layout
            .pane_ids()
            .into_iter()
            .any(|pane| self.is_coordinator_pane(ws_idx, pane))
    }

    /// The caller from `caller_pane`; `None` (absent) is the user.
    pub(crate) fn model_caller(
        &self,
        caller_pane: Option<&str>,
    ) -> ModelResult<Option<ModelCaller>> {
        let Some(caller) = caller_pane.map(str::trim).filter(|c| !c.is_empty()) else {
            return Ok(None);
        };
        let unresolved = || {
            ModelError::new(
                error_code::CALLER_UNRESOLVED,
                format!(
                    "your herdr pane id {caller} no longer points at a pane; restart the session"
                ),
            )
        };
        let (ws_idx, pane_id) = self.parse_pane_id(caller).ok_or_else(unresolved)?;
        let ws = &self.state.workspaces[ws_idx];
        let tab_idx = ws.find_tab_index_for_pane(pane_id).ok_or_else(unresolved)?;
        let terminal_id = ws
            .pane_state(pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
            .ok_or_else(unresolved)?;
        let public = self
            .public_pane_id(ws_idx, pane_id)
            .unwrap_or_else(|| caller.to_string());
        let live = self.pane_hosts_live_agent(ws_idx, pane_id);
        let coordinator = live && self.is_coordinator_pane(ws_idx, pane_id);
        let member = !coordinator
            && self.pane_team_member(ws_idx, pane_id)
            && !self.pane_team_excluded(ws_idx, pane_id);
        let team_ws = member.then_some(ws_idx);
        let (actor, kind) = if !live {
            (Actor::User, AgentActorKind::Shell)
        } else if coordinator {
            (Actor::Coordinator, AgentActorKind::Coordinator)
        } else {
            (
                Actor::Agent {
                    team: team_ws.is_some(),
                },
                AgentActorKind::Agent,
            )
        };
        let turn = self.effective_turn(&terminal_id).unwrap_or(EffectiveTurn {
            origin: TurnOrigin::Unknown,
            poisoned: false,
            bridged: false,
            attach: false,
            started_unix: 0,
        });
        Ok(Some(ModelCaller {
            ws_idx,
            tab_idx,
            pane_id,
            terminal_id,
            name: self
                .model_agent_name(ws_idx, pane_id)
                .unwrap_or_else(|| public.clone()),
            public,
            actor,
            kind,
            live,
            agent: self.model_agent_kind(ws_idx, pane_id),
            team_ws,
            turn,
        }))
    }

    /// A required caller (the agent-side methods).
    pub(crate) fn required_caller(&self, caller_pane: &str) -> ModelResult<ModelCaller> {
        self.model_caller(Some(caller_pane))?
            .ok_or_else(|| ModelError::new(error_code::INVALID_PARAMS, "caller_pane is required"))
    }

    /// A target tab: a pane id, a tab id, an agent name or a unique tab
    /// label, in that order.
    pub(crate) fn resolve_model_tab(&self, target: &str) -> ModelResult<TabTarget> {
        let target = target.trim();
        if target.is_empty() {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "target is required",
            ));
        }
        if let Some((ws_idx, pane_id)) = self.parse_pane_id(target) {
            if let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) {
                return Ok(TabTarget {
                    ws_idx,
                    tab_idx,
                    pane_id: Some(pane_id),
                });
            }
        }
        if let Some((ws_idx, tab_idx)) = self.parse_tab_id(target) {
            return Ok(TabTarget {
                ws_idx,
                tab_idx,
                pane_id: None,
            });
        }
        if let Ok(resolved) = self.resolve_agent_target(target) {
            if let Some(tab_idx) =
                self.state.workspaces[resolved.ws_idx].find_tab_index_for_pane(resolved.pane_id)
            {
                return Ok(TabTarget {
                    ws_idx: resolved.ws_idx,
                    tab_idx,
                    pane_id: Some(resolved.pane_id),
                });
            }
        }
        let mut hits = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                if tab.custom_name.as_deref() == Some(target) {
                    hits.push((ws_idx, tab_idx));
                }
            }
        }
        match hits.as_slice() {
            [(ws_idx, tab_idx)] => Ok(TabTarget {
                ws_idx: *ws_idx,
                tab_idx: *tab_idx,
                pane_id: None,
            }),
            [] => Err(ModelError::new(
                error_code::NOT_FOUND,
                format!("no tab, pane or agent {target}"),
            )),
            _ => Err(ModelError::new(
                error_code::INVALID_PARAMS,
                format!("{} tabs are labelled {target}; pass a tab id", hits.len()),
            )),
        }
    }

    /// A message's `role:<role>` target: the one other member of the
    /// caller's team whose role is `role` (ignoring case, and punctuation as
    /// the role slug does). The usual checks still run on the result.
    fn resolve_team_role(&self, caller: &ModelCaller, role: &str) -> ModelResult<TabTarget> {
        let role = crate::coordinator::one_line(role, 32);
        if role.is_empty() {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "role:<role> needs a role",
            ));
        }
        let team = caller.team_ws.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            Some((ws_idx, ws, ws.team.as_ref()?))
        });
        let Some((ws_idx, ws, team)) = team else {
            return Err(ModelError::new(
                error_code::OUTSIDE_TEAM,
                "role:<role> names a teammate, and you are in no team",
            ));
        };
        let slug = crate::agent_wrap::team::role_slug(&role);
        let matches = |other: &str| {
            other.trim().eq_ignore_ascii_case(&role)
                || (slug.is_some() && crate::agent_wrap::team::role_slug(other) == slug)
        };
        let hits: Vec<(usize, PaneId)> = team
            .members
            .iter()
            .filter(|member| member.pane_id != caller.pane_id)
            .filter(|member| member.role.as_deref().is_some_and(matches))
            .filter_map(|member| {
                ws.find_tab_index_for_pane(member.pane_id)
                    .map(|tab_idx| (tab_idx, member.pane_id))
            })
            .collect();
        match hits.as_slice() {
            [(tab_idx, pane_id)] => Ok(TabTarget {
                ws_idx,
                tab_idx: *tab_idx,
                pane_id: Some(*pane_id),
            }),
            [] => Err(ModelError::new(
                error_code::NOT_FOUND,
                format!("no teammate has role {role} (agents_whoami lists your team)"),
            )),
            _ => {
                let names: Vec<String> = hits
                    .iter()
                    .map(|(_, pane_id)| {
                        let public = self.public_pane_id(ws_idx, *pane_id).unwrap_or_default();
                        match self.model_agent_name(ws_idx, *pane_id) {
                            Some(name) => format!("{name} ({public})"),
                            None => public,
                        }
                    })
                    .collect();
                Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    format!(
                        "{} teammates have role {role}: {}; pass a name or pane",
                        hits.len(),
                        names.join(", ")
                    ),
                ))
            }
        }
    }

    /// The pane a target names: the named pane, else the tab's only agent
    /// pane, else its root.
    pub(crate) fn target_pane(&self, target: TabTarget) -> Option<PaneId> {
        if let Some(pane) = target.pane_id {
            return Some(pane);
        }
        let tab = self
            .state
            .workspaces
            .get(target.ws_idx)?
            .tabs
            .get(target.tab_idx)?;
        let agents: Vec<PaneId> = tab
            .layout
            .pane_ids()
            .into_iter()
            .filter(|pane| {
                self.model_terminal(target.ws_idx, *pane)
                    .is_some_and(crate::terminal::TerminalState::is_agent_terminal)
            })
            .collect();
        match agents.as_slice() {
            [only] => Some(*only),
            _ => Some(tab.root_pane),
        }
    }

    /// The caller's relation to a target tab.
    pub(crate) fn model_relation(
        &self,
        caller: &ModelCaller,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Relation {
        if self.tab_is_protected(ws_idx, tab_idx) {
            return Relation::Protected;
        }
        if caller.ws_idx == ws_idx && caller.tab_idx == tab_idx {
            return Relation::SelfPane;
        }
        if caller.team_ws == Some(ws_idx) {
            let excluded = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.tabs.get(tab_idx))
                .is_some_and(|tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .any(|pane| self.pane_team_excluded(ws_idx, pane))
                });
            if !excluded {
                return Relation::Teammate;
            }
        }
        Relation::Other
    }

    /// The destination kind of a group for a caller.
    pub(crate) fn model_dest(&self, caller: &ModelCaller, ws_idx: Option<usize>) -> Dest {
        match ws_idx {
            Some(ws_idx) if caller.team_ws == Some(ws_idx) => Dest::OwnTeam,
            Some(ws_idx)
                if self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .is_some_and(|ws| ws.team.is_some()) =>
            {
                Dest::OtherTeam
            }
            _ => Dest::PlainOrNewOrTop,
        }
    }

    /// The facts the policy and its hints need.
    pub(crate) fn model_facts(
        &self,
        caller: &ModelCaller,
        target_name: String,
        target_ws: Option<usize>,
    ) -> Facts {
        let own_plain_group = target_ws
            .filter(|ws_idx| *ws_idx == caller.ws_idx && *ws_idx != 0)
            .filter(|ws_idx| {
                self.state
                    .workspaces
                    .get(*ws_idx)
                    .is_some_and(|ws| ws.team.is_none())
            })
            .map(|ws_idx| self.group_label(ws_idx));
        Facts {
            // The coordinator's turn is the one `refuse_in_coordinator_turn`
            // reads (user turn and no live wake/message marker), so the
            // policy and that check cannot disagree.
            user_turn: match caller.actor {
                Actor::Coordinator => self.coordinator_user_turn(caller.ws_idx, caller.pane_id),
                _ => caller.turn.user_turn(),
            },
            reply_to_turn_starter: false,
            turn_desc: caller.turn.describe(),
            target: target_name,
            own_plain_group,
        }
    }

    /// Decide; a denial is logged. `Ok` when allowed.
    pub(crate) fn model_authorize(
        &mut self,
        caller: &ModelCaller,
        relation: Relation,
        action: Action,
        facts: &Facts,
        line: &LogLine,
    ) -> ModelResult<()> {
        match policy::authorize(caller.actor, relation, action, facts) {
            Decision::Allow => Ok(()),
            Decision::Deny { code, hint } => {
                let mut denied = line.clone();
                denied.code = Some(code.to_string());
                denied.detail = Some(hint.clone());
                self.log_model_action(Some(caller), AgentsActionOutcome::Denied, denied);
                Err(ModelError::new(code, hint))
            }
        }
    }

    /// The standard tab check: resolve the relation and facts, decide.
    pub(crate) fn authorize_on_tab(
        &mut self,
        caller: &ModelCaller,
        target: TabTarget,
        action: Action,
        line: &LogLine,
    ) -> ModelResult<()> {
        let relation = self.model_relation(caller, target.ws_idx, target.tab_idx);
        let facts = self.model_facts(
            caller,
            self.tab_label(target.ws_idx, target.tab_idx),
            Some(target.ws_idx),
        );
        self.model_authorize(caller, relation, action, &facts, line)
    }

    /// Count a soft edit against the caller's hourly budget (agents only).
    pub(crate) fn take_soft_edit(
        &mut self,
        caller: &ModelCaller,
        line: &LogLine,
    ) -> ModelResult<()> {
        if !matches!(caller.actor, Actor::Agent { .. }) {
            return Ok(());
        }
        if self
            .agents_model
            .limiter
            .take_soft_edit(&caller.terminal_id, now_unix())
        {
            return Ok(());
        }
        let mut denied = line.clone();
        denied.code = Some(error_code::RATE_LIMITED.into());
        self.log_model_action(Some(caller), AgentsActionOutcome::Denied, denied);
        Err(ModelError::new(
            error_code::RATE_LIMITED,
            format!(
                "at most {} edits per hour; this looks like a loop; stop and ask your user",
                crate::agents_model::limits::SOFT_EDITS_PER_HOUR
            ),
        ))
    }

    // ----- the action log ---------------------------------------------------

    /// Append one action-log line (no-op without a log directory).
    pub(crate) fn log_model_action(
        &mut self,
        caller: Option<&ModelCaller>,
        outcome: AgentsActionOutcome,
        line: LogLine,
    ) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        let entry = self.action_entry(caller, outcome, line);
        if let Err(err) = actions_log::append(&dir, &entry) {
            tracing::warn!(err = %err, action = %entry.action, "agents model: cannot append to the action log");
        }
    }

    pub(crate) fn action_entry(
        &self,
        caller: Option<&ModelCaller>,
        outcome: AgentsActionOutcome,
        line: LogLine,
    ) -> AgentActionEntry {
        let (actor, actor_pane, actor_name, team, turn_origin, origin_detail) = match caller {
            Some(caller) => (
                match caller.actor {
                    Actor::User => AgentActorKind::User,
                    Actor::Coordinator => AgentActorKind::Coordinator,
                    Actor::Agent { .. } => AgentActorKind::Agent,
                },
                Some(caller.public.clone()),
                Some(caller.name.clone()),
                caller
                    .team_ws
                    .map(|ws_idx| self.public_workspace_id(ws_idx)),
                Some(crate::agents_model::turn::wire_origin(&caller.turn.origin)),
                if caller.turn.bridged {
                    Some(AgentsOriginDetail::Bridged)
                } else if caller.turn.attach {
                    Some(AgentsOriginDetail::ClientAttach)
                } else {
                    None
                },
            ),
            None => (AgentActorKind::User, None, None, None, None, None),
        };
        AgentActionEntry {
            unix: now_unix(),
            id: crate::coordinator::messages::new_id().replacen('m', "a", 1),
            actor,
            actor_pane,
            actor_name,
            action: line.action.to_string(),
            target_tab: line.target_tab,
            target_pane: line.target_pane,
            target_name: line.target_name,
            team,
            turn_origin,
            origin_detail,
            outcome,
            code: line.code,
            detail: line.detail,
            closed_ids: line.closed_ids,
        }
    }

    pub(crate) fn tab_line(&self, action: &'static str, target: TabTarget) -> LogLine {
        LogLine {
            action,
            target_tab: self.public_tab_id(target.ws_idx, target.tab_idx),
            target_pane: target
                .pane_id
                .and_then(|pane| self.public_pane_id(target.ws_idx, pane)),
            target_name: Some(self.tab_label(target.ws_idx, target.tab_idx)),
            ..LogLine::default()
        }
    }

    /// The log line of a user request that changes tabs or teams (TUI, user
    /// CLI, or the JSON API without a caller), before it runs; `None` for
    /// every other request. Low frequency.
    pub(crate) fn user_request_line(&self, method: &Method) -> Option<LogLine> {
        if self.agents_model.dir.is_none() || self.agents_model.delegating > 0 {
            return None;
        }
        let (action, target): (&'static str, Option<&str>) = match method {
            Method::TabClose(params) => ("close_tab", Some(&params.tab_id)),
            Method::TabRename(params) => ("rename_tab", Some(&params.tab_id)),
            Method::TabCreate(_) => ("open_tab", None),
            Method::PaneMove(params) => ("move_tab", Some(&params.pane_id)),
            Method::SessionClosedReopen(params) => ("reopen_tab", Some(&params.id)),
            Method::TeamMake(params) if params.caller_pane.is_none() => {
                ("team_make", Some(&params.workspace_id))
            }
            Method::TeamDisband(params) => ("team_disband", Some(&params.workspace_id)),
            Method::TeamJoin(params) => ("team_join", Some(&params.pane_id)),
            Method::TeamLeave(params) => ("team_leave", Some(&params.pane_id)),
            Method::TeamSetRole(params) if params.caller_pane.is_none() => {
                ("set_meta", Some(&params.pane_id))
            }
            _ => return None,
        };
        Some(LogLine {
            action,
            target_name: target.map(str::to_string),
            detail: Some("requested by the user".into()),
            ..LogLine::default()
        })
    }

    /// Log a user request ([`Self::user_request_line`]) with its outcome,
    /// read from the encoded `response`: `failed` with the error code when
    /// it was refused.
    pub(crate) fn log_user_request(&mut self, mut line: LogLine, response: &str) {
        let error = serde_json::from_str::<serde_json::Value>(response)
            .ok()
            .and_then(|value| {
                let error = value.get("error").filter(|error| !error.is_null())?;
                Some(error["code"].as_str().unwrap_or("error").to_string())
            });
        let outcome = match error {
            Some(code) => {
                line.code = Some(code);
                AgentsActionOutcome::Failed
            }
            None => AgentsActionOutcome::Ok,
        };
        self.log_model_action(None, outcome, line);
    }

    // ----- delegation -------------------------------------------------------

    /// Run an existing method in-process and take its result.
    pub(crate) fn call_method(&mut self, method: Method) -> ModelResult<ResponseResult> {
        self.agents_model.delegating += 1;
        let raw = self.handle_api_request_after_internal_events_drained(Request {
            id: "agents-model".into(),
            method,
        });
        self.agents_model.delegating = self.agents_model.delegating.saturating_sub(1);
        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|err| ModelError::new(error_code::FAILED, err.to_string()))?;
        if let Some(error) = value.get("error") {
            let body: ErrorBody = serde_json::from_value(error.clone())
                .map_err(|err| ModelError::new(error_code::FAILED, err.to_string()))?;
            return Err(ModelError {
                code: body.code,
                message: body.message,
            });
        }
        serde_json::from_value(value["result"].clone())
            .map_err(|err| ModelError::new(error_code::FAILED, err.to_string()))
    }

    pub(crate) fn model_reply(id: String, result: ModelResult<ResponseResult>) -> String {
        match result {
            Ok(result) => encode_success(id, result),
            Err(err) => encode_error(id, &err.code, err.message),
        }
    }

    // ----- agents.actor ------------------------------------------------------

    pub(super) fn handle_agents_actor(&mut self, id: String, params: AgentsActorParams) -> String {
        let result = self.agents_actor(&params);
        Self::model_reply(
            id,
            result.map(|actor| ResponseResult::AgentsActor { actor }),
        )
    }

    fn agents_actor(&mut self, params: &AgentsActorParams) -> ModelResult<AgentsActorInfo> {
        let caller = self.required_caller(&params.caller_pane)?;
        let (role, note, session) = self
            .model_terminal(caller.ws_idx, caller.pane_id)
            .map(|terminal| {
                (
                    terminal.agent_meta().role.clone(),
                    terminal.agent_meta().note.clone(),
                    terminal
                        .persistable_agent_session()
                        .map(|session| session.session_ref.value),
                )
            })
            .unwrap_or_default();
        let member_role = self
            .state
            .workspaces
            .get(caller.ws_idx)
            .and_then(|ws| ws.team.as_ref())
            .and_then(|team| team.member(caller.pane_id))
            .and_then(|member| member.role.clone());
        let team = caller
            .team_ws
            .and_then(|ws_idx| self.team_ref(ws_idx, true));
        // The team change the caller has not been told (one RPC: the
        // `team.context` read and its ack fold in here).
        let (team_update, team_revision, ack_key) =
            match self.call_method(Method::TeamContext(crate::api::schema::TeamContextParams {
                caller_pane: caller.public.clone(),
                ack: params.ack,
                full: params.full,
                ack_revision: params.ack_revision,
                ack_key: params.ack_key.clone(),
            })) {
                Ok(ResponseResult::TeamContext {
                    text,
                    revision,
                    ack_key,
                    ..
                }) => (text, Some(revision).filter(|r| *r > 0), ack_key),
                _ => (None, None, None),
            };
        let rights = self.rights_line(&caller);
        let turn = caller
            .turn
            .info(|terminal_id| self.public_pane_of_terminal(terminal_id));
        Ok(AgentsActorInfo {
            pane_id: caller.public.clone(),
            kind: caller.kind,
            live: caller.live,
            workspace_id: self.public_workspace_id(caller.ws_idx),
            tab_id: self.public_tab_id(caller.ws_idx, caller.tab_idx),
            name: Some(caller.name.clone()),
            agent: caller.agent.clone(),
            session,
            team,
            role: role.or(member_role),
            note,
            turn,
            rights,
            team_update,
            team_revision,
            ack_key,
        })
    }

    /// One line: what the caller may do.
    fn rights_line(&self, caller: &ModelCaller) -> String {
        match caller.actor {
            Actor::User => "your user's shell: everything".into(),
            Actor::Coordinator => "coordinator: a member of every team; every change needs your user's request in this turn".into(),
            Actor::Agent { .. } => match caller.team_ws {
                Some(ws_idx) => {
                    let tabs = self
                        .state
                        .workspaces
                        .get(ws_idx)
                        .map(|ws| ws.tabs.len())
                        .unwrap_or(0);
                    format!(
                        "edit: your team {} ({tabs} tabs) · read+message: everyone · shell screens: your team only · closing: your user's request",
                        self.group_label(ws_idx)
                    )
                }
                None => "edit: your own tab · read+message: everyone · shell screens: your own only · closing: your user's request".into(),
            },
        }
    }

    /// The public pane id of a terminal (O(panes); rare).
    pub(crate) fn public_pane_of_terminal(&self, terminal_id: &TerminalId) -> Option<String> {
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    if &pane.attached_terminal_id == terminal_id {
                        return self.public_pane_id(ws_idx, *pane_id);
                    }
                }
            }
        }
        None
    }

    /// Where a terminal is: its group, tab and pane.
    pub(crate) fn locate_terminal(
        &self,
        terminal_id: &TerminalId,
    ) -> Option<(usize, usize, PaneId)> {
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                for (pane_id, pane) in &tab.panes {
                    if &pane.attached_terminal_id == terminal_id {
                        return Some((ws_idx, tab_idx, *pane_id));
                    }
                }
            }
        }
        None
    }

    /// A team group's reference, with its members when asked.
    fn team_ref(&self, ws_idx: usize, members: bool) -> Option<AgentsTeamRef> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let team = ws.team.as_ref()?;
        let info = members.then(|| self.team_info(ws_idx, true)).flatten();
        Some(AgentsTeamRef {
            workspace_id: ws.id.clone(),
            label: self.group_label(ws_idx),
            purpose: team.purpose.clone(),
            purpose_by: team
                .purpose
                .as_ref()
                .and(team.purpose_by.as_ref())
                .map(|by| by.describe()),
            member_count: team.members.len() as u32,
            members: info.map(|info| info.members).unwrap_or_default(),
        })
    }

    // ----- agents.directory --------------------------------------------------

    pub(super) fn handle_agents_directory(
        &mut self,
        id: String,
        params: AgentsDirectoryParams,
    ) -> String {
        let result = self.agents_directory(&params);
        Self::model_reply(
            id,
            result.map(|directory| ResponseResult::AgentsDirectory { directory }),
        )
    }

    fn agents_directory(&mut self, params: &AgentsDirectoryParams) -> ModelResult<AgentsDirectory> {
        let caller = self.model_caller(params.caller_pane.as_deref())?;
        let filter_group = match (&params.group, &params.team) {
            (Some(group), _) | (None, Some(group)) => {
                Some(self.resolve_group(group).ok_or_else(|| {
                    ModelError::new(error_code::NOT_FOUND, format!("no group {group}"))
                })?)
            }
            (None, None) => None,
        };
        if params.team.is_some()
            && filter_group.is_some_and(|ws_idx| self.state.workspaces[ws_idx].team.is_none())
        {
            return Err(ModelError::new(
                error_code::NOT_FOUND,
                "that group is not a team",
            ));
        }
        // Default scope: the caller's group in full (counts for the rest).
        let scope = match (filter_group, params.all, &caller) {
            (Some(ws_idx), _, _) => Some(ws_idx),
            (None, true, _) => None,
            (None, false, Some(caller)) => Some(caller.ws_idx),
            (None, false, None) => None,
        };
        let limit = params.limit.unwrap_or(DIRECTORY_DEFAULT_LIMIT) as usize;
        let mut groups = Vec::new();
        let mut tabs = Vec::new();
        let mut truncated = 0u32;
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            let mut agent_count = 0u32;
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                let in_scope = scope.is_none_or(|scope| scope == ws_idx);
                if !in_scope {
                    // Counts only: no row is built for out-of-scope tabs.
                    agent_count += tab
                        .panes
                        .values()
                        .filter_map(|pane| self.state.terminals.get(&pane.attached_terminal_id))
                        .filter(|terminal| terminal.is_agent_terminal())
                        .count() as u32;
                    continue;
                }
                let panes: Vec<AgentsPaneInfo> = tab
                    .layout
                    .pane_ids()
                    .into_iter()
                    .filter_map(|pane| self.directory_pane(caller.as_ref(), ws_idx, tab_idx, pane))
                    .collect();
                let agents = panes
                    .iter()
                    .filter(|p| p.kind == AgentPaneKind::Agent)
                    .count() as u32;
                agent_count += agents;
                if params.agents_only && agents == 0 {
                    continue;
                }
                if tabs.len() >= limit {
                    truncated += 1;
                    continue;
                }
                let access = caller.as_ref().map(|caller| {
                    match (caller.actor, self.model_relation(caller, ws_idx, tab_idx)) {
                        (_, Relation::SelfPane) => AgentsAccess::SelfTab,
                        (Actor::User | Actor::Coordinator, _) => AgentsAccess::Edit,
                        (_, Relation::Teammate) => AgentsAccess::Edit,
                        _ => AgentsAccess::ReadMessage,
                    }
                });
                tabs.push(AgentsTabInfo {
                    tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap_or_default(),
                    workspace_id: ws.id.clone(),
                    label: self.tab_label(ws_idx, tab_idx),
                    protected: self.tab_is_protected(ws_idx, tab_idx),
                    access,
                    panes,
                });
            }
            groups.push(AgentsGroupInfo {
                workspace_id: ws.id.clone(),
                label: self.group_label(ws_idx),
                number: ws_idx as u32 + 1,
                team: ws.team.as_ref().map(|team| AgentsGroupTeam {
                    purpose: team.purpose.clone(),
                    member_count: team.members.len() as u32,
                }),
                tab_count: ws.tabs.len() as u32,
                agent_count,
            });
        }
        let recently_closed = if params.include_closed {
            self.recently_closed_for(caller.as_ref())
        } else {
            Vec::new()
        };
        Ok(AgentsDirectory {
            generated_unix: now_unix(),
            caller: caller.map(|caller| caller.public),
            groups,
            tabs,
            truncated,
            recently_closed,
        })
    }

    /// A group by id, unique label, or sidebar number.
    pub(crate) fn resolve_group(&self, group: &str) -> Option<usize> {
        let group = group.trim();
        if let Some(ws_idx) = self.state.workspaces.iter().position(|ws| ws.id == group) {
            return Some(ws_idx);
        }
        let hits: Vec<usize> = (0..self.state.workspaces.len())
            .filter(|ws_idx| self.group_label(*ws_idx) == group)
            .collect();
        if let [only] = hits.as_slice() {
            return Some(*only);
        }
        if !hits.is_empty() {
            return None;
        }
        self.parse_workspace_id(group)
            .filter(|ws_idx| self.state.workspaces.get(*ws_idx).is_some())
    }

    /// One directory pane: scalar accessors only (no process inspection).
    fn directory_pane(
        &self,
        caller: Option<&ModelCaller>,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    ) -> Option<AgentsPaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let public = self.public_pane_id(ws_idx, pane_id)?;
        let agent_pane = terminal.is_agent_terminal();
        let kind = if agent_pane {
            AgentPaneKind::Agent
        } else {
            AgentPaneKind::Shell
        };
        // U6: a shell's screen (and cwd) only for itself, its team and the
        // coordinator.
        let screen = match (kind, caller) {
            (AgentPaneKind::Agent, _) | (_, None) => AgentsScreenAccess::Full,
            (_, Some(caller)) => {
                let relation = self.model_relation(caller, ws_idx, tab_idx);
                if policy::authorize(
                    caller.actor,
                    relation,
                    Action::ReadShellScreen,
                    &Facts::default(),
                )
                .is_allowed()
                {
                    AgentsScreenAccess::Full
                } else {
                    AgentsScreenAccess::Exists
                }
            }
        };
        let meta = terminal.agent_meta();
        let suspended = terminal.suspended_agent.is_some();
        Some(AgentsPaneInfo {
            pane_id: public,
            kind,
            agent: agent_pane
                .then(|| self.model_agent_kind(ws_idx, pane_id))
                .flatten(),
            name: terminal.agent_name.clone().or_else(|| {
                terminal
                    .suspended_agent
                    .as_ref()
                    .and_then(|record| record.name.clone())
            }),
            status: agent_pane
                .then(|| crate::workspace::agent_status(terminal.state, pane.seen, suspended)),
            suspended,
            launch_pending: terminal.managed_agent_launch_pending(),
            session: agent_pane
                .then(|| terminal.persistable_agent_session())
                .flatten()
                .map(|session| session.session_ref.value),
            role: meta.role.clone().or_else(|| {
                ws.team
                    .as_ref()
                    .and_then(|team| team.member(pane_id))
                    .and_then(|member| member.role.clone())
            }),
            note: meta.note.clone(),
            member: ws.team.as_ref().is_some_and(|team| team.is_member(pane_id)),
            opened_by: meta.opened_by.clone(),
            subagents: terminal.active_subagent_count(),
            cwd: (screen == AgentsScreenAccess::Full)
                .then(|| terminal.cwd.to_string_lossy().into_owned()),
            screen,
        })
    }

    // ----- agents.read --------------------------------------------------------

    pub(super) fn handle_agents_read(&mut self, id: String, params: AgentsReadParams) -> String {
        let result = self.agents_read(&params);
        Self::model_reply(id, result.map(|read| ResponseResult::AgentsRead { read }))
    }

    fn agents_read(&mut self, params: &AgentsReadParams) -> ModelResult<AgentsReadResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let agent_pane = self
            .model_terminal(target.ws_idx, pane)
            .is_some_and(crate::terminal::TerminalState::is_agent_terminal);
        let public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        if !agent_pane {
            let mut line = self.tab_line("read_shell_screen", target);
            line.target_pane = Some(public.clone());
            self.authorize_on_tab(&caller, target, Action::ReadShellScreen, &line)?;
        }
        let source = match params.source.unwrap_or_default() {
            AgentsReadSource::Visible => crate::api::schema::ReadSource::Visible,
            AgentsReadSource::RecentUnwrapped => crate::api::schema::ReadSource::RecentUnwrapped,
            AgentsReadSource::Detection => crate::api::schema::ReadSource::Detection,
            AgentsReadSource::Recent | AgentsReadSource::Unknown => {
                crate::api::schema::ReadSource::Recent
            }
        };
        let format = match params.format.unwrap_or_default() {
            AgentsReadFormat::Ansi => crate::api::schema::ReadFormat::Ansi,
            AgentsReadFormat::Text | AgentsReadFormat::Unknown => {
                crate::api::schema::ReadFormat::Text
            }
        };
        let result = self.call_method(Method::PaneRead(crate::api::schema::PaneReadParams {
            pane_id: public.clone(),
            source,
            lines: params.lines,
            format,
            strip_ansi: format == crate::api::schema::ReadFormat::Text,
            intent: Default::default(),
        }))?;
        let ResponseResult::PaneRead { read } = result else {
            return Err(ModelError::new(
                error_code::FAILED,
                "unexpected pane.read reply",
            ));
        };
        Ok(AgentsReadResult {
            pane_id: public,
            kind: if agent_pane {
                AgentPaneKind::Agent
            } else {
                AgentPaneKind::Shell
            },
            text: read.text,
            truncated: read.truncated,
        })
    }

    // ----- agents.rename_tab --------------------------------------------------

    pub(super) fn handle_agents_rename_tab(
        &mut self,
        id: String,
        params: AgentsRenameTabParams,
    ) -> String {
        let result = self.agents_rename_tab(&params);
        Self::model_reply(
            id,
            result.map(|rename| ResponseResult::AgentsRenameTab { rename }),
        )
    }

    fn agents_rename_tab(
        &mut self,
        params: &AgentsRenameTabParams,
    ) -> ModelResult<AgentsRenameResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let name = one_line(&params.name, LABEL_MAX_CHARS);
        if name.is_empty() {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "name is required",
            ));
        }
        let mut line = self.tab_line("rename_tab", target);
        line.detail = Some(format!("to {name}"));
        // The protected check runs first (D3): an agent cannot un-protect
        // the coordinator's tab by renaming it.
        self.authorize_on_tab(
            &caller,
            target,
            Action::SoftEdit(policy::SoftEdit::RenameTab),
            &line,
        )?;
        self.take_soft_edit(&caller, &line)?;
        let tab_id = self
            .public_tab_id(target.ws_idx, target.tab_idx)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "tab not found"))?;
        self.call_method(Method::TabRename(crate::api::schema::TabRenameParams {
            tab_id: tab_id.clone(),
            label: name.clone(),
        }))?;
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(AgentsRenameResult { tab_id, name })
    }

    // ----- agents.move_tab ----------------------------------------------------

    pub(super) fn handle_agents_move_tab(
        &mut self,
        id: String,
        params: AgentsMoveTabParams,
    ) -> String {
        let result = self.agents_move_tab(&params);
        Self::model_reply(
            id,
            result.map(|moved| ResponseResult::AgentsMoveTab { moved }),
        )
    }

    /// The destination group of a move or an open: an existing group, a new
    /// one (`None`), or the top space.
    fn model_destination(
        &self,
        group: &Option<String>,
        new_group: &Option<String>,
        priority: bool,
    ) -> ModelResult<Option<Option<usize>>> {
        match (group, new_group, priority) {
            (Some(group), None, false) => {
                Ok(Some(Some(self.resolve_group(group).ok_or_else(|| {
                    ModelError::new(
                        error_code::NOT_FOUND,
                        format!("no group {group}; pass new_group to create one"),
                    )
                })?)))
            }
            (None, Some(_), false) => Ok(Some(None)),
            (None, None, true) => Ok(Some(Some(0))),
            (None, None, false) => Ok(None),
            _ => Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "pass at most one of group, new_group and priority",
            )),
        }
    }

    fn agents_move_tab(&mut self, params: &AgentsMoveTabParams) -> ModelResult<AgentsMoveResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let dest_group = self
            .model_destination(&params.group, &params.new_group, params.priority)?
            .ok_or_else(|| {
                ModelError::new(
                    error_code::INVALID_PARAMS,
                    "pass one of group, new_group and priority",
                )
            })?;
        let tab = &self.state.workspaces[target.ws_idx].tabs[target.tab_idx];
        let pane = match (target.pane_id, tab.panes.len()) {
            (_, 1) => tab.root_pane,
            (Some(pane), _) => pane,
            (None, _) => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "a split tab moves one pane at a time; name the pane",
                ))
            }
        };
        let label = tab.custom_name.clone();
        let mut line = self.tab_line("move_tab", target);
        line.detail = Some(match (&params.new_group, dest_group) {
            (Some(new_group), _) => format!("to new group {new_group}"),
            (None, Some(0)) if params.priority => "to the top space".to_string(),
            (None, Some(ws_idx)) => format!("to {}", self.group_label(ws_idx)),
            (None, None) => String::new(),
        });
        let dest = self.model_dest(&caller, dest_group);
        self.authorize_on_tab(&caller, target, Action::MoveTab { dest }, &line)?;
        self.take_soft_edit(&caller, &line)?;
        let previous = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        let destination = match (&params.new_group, dest_group) {
            (Some(new_group), _) => crate::api::schema::PaneMoveDestination::NewWorkspace {
                label: Some(one_line(new_group, LABEL_MAX_CHARS)),
                tab_label: label,
            },
            (None, Some(ws_idx)) => crate::api::schema::PaneMoveDestination::NewTab {
                workspace_id: Some(self.public_workspace_id(ws_idx)),
                label,
            },
            (None, None) => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "no destination",
                ))
            }
        };
        let result = self.call_method(Method::PaneMove(crate::api::schema::PaneMoveParams {
            pane_id: previous.clone(),
            destination,
            focus: false,
        }))?;
        let ResponseResult::PaneMove { move_result } = result else {
            return Err(ModelError::new(
                error_code::FAILED,
                "unexpected pane.move reply",
            ));
        };
        line.target_pane = Some(move_result.pane.pane_id.clone());
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(AgentsMoveResult {
            previous_pane_id: previous,
            pane_id: move_result.pane.pane_id.clone(),
            tab_id: move_result.pane.tab_id.clone(),
            workspace_id: move_result.pane.workspace_id.clone(),
        })
    }

    // ----- agents.set_meta ----------------------------------------------------

    pub(super) fn handle_agents_set_meta(
        &mut self,
        id: String,
        params: AgentsSetMetaParams,
    ) -> String {
        let result = self.agents_set_meta(&params);
        Self::model_reply(
            id,
            result.map(|meta| ResponseResult::AgentsSetMeta { meta }),
        )
    }

    fn agents_set_meta(
        &mut self,
        params: &AgentsSetMetaParams,
    ) -> ModelResult<AgentsSetMetaResult> {
        let caller = self.model_caller(params.caller_pane.as_deref())?;
        let target = self.resolve_model_tab(&params.target)?;
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        let mut line = self.tab_line("set_meta", target);
        line.target_pane = Some(public.clone());
        line.detail = Some(
            [
                params.role.as_ref().map(|role| format!("role {role:?}")),
                params.note.as_ref().map(|note| format!("note {note:?}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", "),
        );
        if params.role.as_deref().is_some_and(|role| {
            role.trim()
                .eq_ignore_ascii_case(crate::coordinator::COORDINATOR_ROLE)
        }) {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "the coordinator role is set by herdr",
            ));
        }
        if let Some(caller) = &caller {
            self.authorize_on_tab(
                caller,
                target,
                Action::SoftEdit(policy::SoftEdit::SetMeta),
                &line,
            )?;
            self.take_soft_edit(caller, &line)?;
        }
        let renamed = self.set_pane_meta(
            target.ws_idx,
            pane,
            params.role.as_deref(),
            params.note.as_deref(),
        )?;
        self.log_model_action(caller.as_ref(), AgentsActionOutcome::Ok, line);
        // A role change can move the pane's public id (never) or name.
        let (ws_idx, pane) = self.parse_pane_id(&public).unwrap_or((target.ws_idx, pane));
        let meta = self
            .model_terminal(ws_idx, pane)
            .map(|terminal| terminal.agent_meta().clone())
            .unwrap_or_default();
        Ok(AgentsSetMetaResult {
            pane_id: public,
            role: meta.role,
            note: meta.note,
            name: self.model_agent_name(ws_idx, pane),
            renamed,
        })
    }

    /// Write a pane's role and note: a team member's role goes through the
    /// team rename rules (`team.set_role`, which also writes the meta), a
    /// non-member's is a plain label. An empty value clears the field and
    /// records a tombstone.
    pub(crate) fn set_pane_meta(
        &mut self,
        ws_idx: usize,
        pane: PaneId,
        role: Option<&str>,
        note: Option<&str>,
    ) -> ModelResult<Option<bool>> {
        let mut renamed = None;
        if let Some(role) = role {
            let sanitized = crate::workspace::team::sanitize_role(role);
            if self.pane_team_member(ws_idx, pane) {
                let public = self.public_pane_id(ws_idx, pane).unwrap_or_default();
                if let ResponseResult::TeamReply { renamed: r, .. } =
                    self.call_method(Method::TeamSetRole(crate::api::schema::TeamSetRoleParams {
                        pane_id: public,
                        role: Some(sanitized.clone().unwrap_or_default()),
                        caller_pane: None,
                    }))?
                {
                    renamed = r;
                }
            }
            self.write_meta(ws_idx, pane, |meta| {
                meta.role_cleared = sanitized.is_none();
                meta.role = sanitized;
            });
        }
        if let Some(note) = note {
            let note = Some(one_line(note, NOTE_MAX_CHARS)).filter(|note| !note.is_empty());
            self.write_meta(ws_idx, pane, |meta| {
                meta.note_cleared = note.is_none();
                meta.note = note;
            });
        }
        Ok(renamed)
    }

    /// Change a pane's meta; save the session when it changed.
    pub(crate) fn write_meta(
        &mut self,
        ws_idx: usize,
        pane: PaneId,
        change: impl FnOnce(&mut crate::agents_model::PaneAgentMeta),
    ) {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return;
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return;
        };
        let meta = terminal.agent_meta_mut();
        let before = meta.clone();
        change(meta);
        if *meta != before {
            meta.updated_unix = now_unix();
            self.state.mark_session_dirty();
            self.schedule_session_save();
            self.mark_coordinator_input_dirty();
        }
    }

    // ----- agents.notes_append / agents.checkpoint ---------------------------

    pub(super) fn handle_agents_notes_append(
        &mut self,
        id: String,
        params: AgentsNotesAppendParams,
    ) -> String {
        let result = self.agents_notes_append(params);
        Self::model_reply(id, result)
    }

    /// The pane a notes write targets, after the check.
    fn notes_target(
        &mut self,
        caller: &ModelCaller,
        target: &str,
        action: &'static str,
        soft: policy::SoftEdit,
    ) -> ModelResult<(String, bool, LogLine)> {
        let target = self.resolve_model_tab(target)?;
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        let mut line = self.tab_line(action, target);
        line.target_pane = Some(public.clone());
        self.authorize_on_tab(caller, target, Action::SoftEdit(soft), &line)?;
        self.take_soft_edit(caller, &line)?;
        let own = caller.ws_idx == target.ws_idx && caller.pane_id == pane;
        Ok((public, own, line))
    }

    fn agents_notes_append(
        &mut self,
        params: AgentsNotesAppendParams,
    ) -> ModelResult<ResponseResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let (public, own, line) = self.notes_target(
            &caller,
            &params.target,
            "notes_append",
            policy::SoftEdit::NotesAppend,
        )?;
        let text = if own {
            params.text
        } else {
            format!(
                "[from {}] {}",
                one_line(&caller.name, LABEL_MAX_CHARS),
                params.text
            )
        };
        let result = self.call_method(Method::NotesAppend(
            crate::api::schema::notes::NotesAppendParams {
                target: crate::api::schema::notes::NotesTarget {
                    pane_id: Some(public),
                    ..Default::default()
                },
                text,
                section: params.section,
                stamp: params.stamp,
                author: crate::api::schema::notes::NotesAuthor::Agent,
            },
        ))?;
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(result)
    }

    pub(super) fn handle_agents_checkpoint(
        &mut self,
        id: String,
        params: AgentsCheckpointParams,
    ) -> String {
        let result = self.agents_checkpoint(params);
        Self::model_reply(id, result)
    }

    fn agents_checkpoint(&mut self, params: AgentsCheckpointParams) -> ModelResult<ResponseResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let kind: crate::api::schema::notes::CheckpointKind = serde_json::from_value(
            serde_json::Value::String(params.kind.trim().to_ascii_lowercase()),
        )
        .ok()
        .filter(|kind| *kind != crate::api::schema::notes::CheckpointKind::Unknown)
        .ok_or_else(|| {
            ModelError::new(
                error_code::INVALID_PARAMS,
                "kind: decision, milestone, failure, bookmark or note",
            )
        })?;
        let (public, own, line) = self.notes_target(
            &caller,
            &params.target,
            "checkpoint",
            policy::SoftEdit::Checkpoint,
        )?;
        let mut tags = params.tags;
        if !own {
            tags.push(format!("from:{}", one_line(&caller.name, LABEL_MAX_CHARS)));
        }
        let result = self.call_method(Method::CheckpointsAdd(
            crate::api::schema::notes::CheckpointsAddParams {
                target: crate::api::schema::notes::NotesTarget {
                    pane_id: Some(public),
                    ..Default::default()
                },
                kind,
                title: params.title,
                detail: params.detail,
                tags,
                author: crate::api::schema::notes::NotesAuthor::Agent,
            },
        ))?;
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(result)
    }

    // ----- agents.actions ------------------------------------------------------

    pub(super) fn handle_agents_actions(
        &mut self,
        id: String,
        params: AgentsActionsParams,
    ) -> String {
        let limit = params.limit.unwrap_or(50).clamp(1, 500) as usize;
        let entries = match self.agents_model.dir.clone() {
            Some(dir) => {
                // Filters read further back so a filtered list still fills.
                let scan = if params.target.is_some() || params.actor.is_some() {
                    limit * 20
                } else {
                    limit
                };
                actions_log::read_tail(&dir, scan)
                    .into_iter()
                    .filter(|entry| {
                        params.target.as_deref().is_none_or(|target| {
                            entry.target_tab.as_deref() == Some(target)
                                || entry.target_pane.as_deref() == Some(target)
                                || entry.target_name.as_deref() == Some(target)
                        }) && params.actor.as_deref().is_none_or(|actor| {
                            entry.actor_pane.as_deref() == Some(actor)
                                || entry.actor_name.as_deref() == Some(actor)
                        })
                    })
                    .take(limit)
                    .collect()
            }
            None => Vec::new(),
        };
        encode_success(id, ResponseResult::AgentsActions { entries })
    }

    // ----- agents.check --------------------------------------------------------

    pub(super) fn handle_agents_check(&mut self, id: String, params: AgentsCheckParams) -> String {
        let result = self.agents_check(&params);
        Self::model_reply(
            id,
            result.map(|()| ResponseResult::AgentsCheck { allowed: true }),
        )
    }

    fn agents_check(&mut self, params: &AgentsCheckParams) -> ModelResult<()> {
        let caller = self.required_caller(&params.caller_pane)?;
        let (action, name): (Action, &'static str) = match params.action {
            AgentsCheckAction::Cosmetic => {
                (Action::SoftEdit(policy::SoftEdit::Cosmetic), "cosmetic")
            }
            AgentsCheckAction::SuspendRestart => (
                Action::SoftEdit(policy::SoftEdit::SuspendRestart),
                "suspend_restart",
            ),
            AgentsCheckAction::ShellInput => (
                Action::SoftEdit(policy::SoftEdit::ShellInput),
                "shell_input",
            ),
            AgentsCheckAction::ClosePane => (Action::Close, "close_pane"),
            AgentsCheckAction::TeamStructure => (
                Action::TeamStructure(policy::TeamOp::Purpose),
                "team_structure",
            ),
            // (Make is chosen below when the group is not a team yet.)
            AgentsCheckAction::Activate => (
                Action::SoftEdit(policy::SoftEdit::Activate { by_agent: false }),
                "activate",
            ),
            AgentsCheckAction::Unknown => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "an action this server does not know",
                ))
            }
        };
        if params.action == AgentsCheckAction::TeamStructure {
            // A group: the caller's own (self) or another.
            let ws_idx = match params.target.as_deref() {
                Some(target) => self.resolve_group(target).ok_or_else(|| {
                    ModelError::new(error_code::NOT_FOUND, format!("no group {target}"))
                })?,
                None => caller.ws_idx,
            };
            let relation = if ws_idx == caller.ws_idx {
                Relation::SelfPane
            } else {
                Relation::Other
            };
            let action = if self.state.workspaces[ws_idx].team.is_none() {
                Action::TeamStructure(policy::TeamOp::Make)
            } else {
                action
            };
            let line = LogLine {
                action: name,
                target_name: Some(self.group_label(ws_idx)),
                ..LogLine::default()
            };
            let facts = self.model_facts(&caller, self.group_label(ws_idx), Some(ws_idx));
            self.model_authorize(&caller, relation, action, &facts, &line)?;
            self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
            return Ok(());
        }
        let target = match params.target.as_deref() {
            Some(target) => self.resolve_model_tab(target)?,
            None => TabTarget {
                ws_idx: caller.ws_idx,
                tab_idx: caller.tab_idx,
                pane_id: Some(caller.pane_id),
            },
        };
        if params.action == AgentsCheckAction::ShellInput {
            // Typing into another agent is a message (envelope, limits,
            // idle only), never raw keys.
            let pane = self.target_pane(target);
            let other_agent = pane.is_some_and(|pane| {
                !(target.ws_idx == caller.ws_idx && pane == caller.pane_id)
                    && self
                        .model_terminal(target.ws_idx, pane)
                        .is_some_and(crate::terminal::TerminalState::is_agent_terminal)
            });
            if other_agent {
                let mut line = self.tab_line(name, target);
                line.code = Some(error_code::INVALID_TARGET.into());
                self.log_model_action(Some(&caller), AgentsActionOutcome::Denied, line);
                return Err(ModelError::new(
                    error_code::INVALID_TARGET,
                    "that pane runs an agent; use agents_send_message to reach it",
                ));
            }
        }
        let action = match params.action {
            AgentsCheckAction::Activate => {
                let by_agent = self
                    .target_pane(target)
                    .and_then(|pane| self.model_terminal(target.ws_idx, pane))
                    .and_then(|terminal| terminal.suspended_agent.as_ref())
                    .and_then(|record| record.suspended_by.as_ref())
                    .is_some_and(AgentsWho::is_agent);
                Action::SoftEdit(policy::SoftEdit::Activate { by_agent })
            }
            _ => action,
        };
        let line = self.tab_line(name, target);
        self.authorize_on_tab(&caller, target, action, &line)?;
        if matches!(action, Action::SoftEdit(_)) {
            self.take_soft_edit(&caller, &line)?;
        }
        self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
        Ok(())
    }

    // ----- agents.suspend / agents.activate / agents.restart -------------------

    pub(super) fn handle_agents_lifecycle(
        &mut self,
        id: String,
        op: AgentLifecycle,
        params: AgentsLifecycleParams,
    ) -> String {
        let result = self.agents_lifecycle(op, &params);
        Self::model_reply(id, result)
    }

    /// Check (one soft edit; activating what the user suspended needs the
    /// caller's user turn, D4), act and log, in one `&mut App` borrow. The
    /// suspend record names the caller, so its teammates may activate it.
    fn agents_lifecycle(
        &mut self,
        op: AgentLifecycle,
        params: &AgentsLifecycleParams,
    ) -> ModelResult<ResponseResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let target = self.resolve_model_tab(&params.target)?;
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        let terminal = self.model_terminal(target.ws_idx, pane);
        let suspended_by_agent = terminal
            .and_then(|terminal| terminal.suspended_agent.as_ref())
            .and_then(|record| record.suspended_by.as_ref())
            .is_some_and(AgentsWho::is_agent);
        let name = terminal
            .and_then(|terminal| {
                terminal.agent_name.clone().or_else(|| {
                    terminal
                        .suspended_agent
                        .as_ref()
                        .and_then(|record| record.name.clone())
                })
            })
            .unwrap_or_else(|| public.clone());
        let mut line = self.tab_line(op.action_name(), target);
        line.target_pane = Some(public.clone());
        line.target_name = Some(name);
        let action = match op {
            AgentLifecycle::Suspend | AgentLifecycle::Restart => {
                Action::SoftEdit(policy::SoftEdit::SuspendRestart)
            }
            AgentLifecycle::Activate => Action::SoftEdit(policy::SoftEdit::Activate {
                by_agent: suspended_by_agent,
            }),
        };
        self.authorize_on_tab(&caller, target, action, &line)?;
        // The typing guard: suspend and restart type the exit command into
        // the agent's input box, activate types the resume command into the
        // pane's shell; neither lands while its user types there or holds a
        // draft. Before the soft edit, so a refusal costs nothing.
        if self.pane_user_typing(target.ws_idx, pane) {
            line.code = Some(error_code::USER_TYPING.into());
            self.log_model_action(Some(&caller), AgentsActionOutcome::Denied, line);
            return Err(ModelError::new(
                error_code::USER_TYPING,
                format!("your user is typing in {public}; ask again later"),
            ));
        }
        self.take_soft_edit(&caller, &line)?;
        let by = Some(caller.who());
        let result = match op {
            AgentLifecycle::Suspend => self
                .suspend_agent_by(&public, by)
                .map(|pane_id| ResponseResult::AgentSuspended { pane_id })
                .map_err(|err| self.agent_suspend_error_body(err)),
            AgentLifecycle::Activate => self
                .activate_agent(&public)
                .map(|pane_id| ResponseResult::AgentActivated { pane_id })
                .map_err(|err| self.agent_activate_error_body(err)),
            AgentLifecycle::Restart => self
                .restart_agent_by(&public, by)
                .map(|pane_id| ResponseResult::AgentRestarted { pane_id })
                .map_err(|err| self.agent_restart_error_body(err)),
        };
        match result {
            Ok(result) => {
                self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
                Ok(result)
            }
            Err(body) => {
                line.code = Some(body.code.clone());
                line.detail = Some(body.message.clone());
                self.log_model_action(Some(&caller), AgentsActionOutcome::Failed, line);
                Err(ModelError::new(&body.code, body.message))
            }
        }
    }

    // ----- agents.send_message --------------------------------------------------

    pub(super) fn handle_agents_send_message(
        &mut self,
        id: String,
        params: AgentsSendMessageParams,
    ) -> String {
        let result = self.agents_send_message(&params);
        Self::model_reply(
            id,
            result.map(|message| ResponseResult::AgentsMessage { message }),
        )
    }

    /// The reply index, seeded from the message log on first use.
    fn reply_index(&mut self) -> &mut ReplyIndex {
        if self.agents_model.reply_index.is_none() {
            let now = now_unix();
            let index = match self.agents_model.dir.as_deref() {
                Some(dir) => {
                    let log = crate::coordinator::messages::recent(dir, 5_000, None);
                    ReplyIndex::seeded(&log, now)
                }
                None => ReplyIndex::default(),
            };
            self.agents_model.reply_index = Some(index);
        }
        self.agents_model
            .reply_index
            .get_or_insert_with(ReplyIndex::default)
    }

    fn agents_send_message(
        &mut self,
        params: &AgentsSendMessageParams,
    ) -> ModelResult<AgentsMessageResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        if params.text.trim().is_empty() {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "text is required",
            ));
        }
        let text: String = crate::agents_model::envelope::message_text(&params.text)
            .chars()
            .take(MESSAGE_MAX_CHARS)
            .collect();
        let reply_to = params
            .reply_to
            .as_deref()
            .filter(|id| crate::coordinator::messages::is_id(id));
        let target = match params.to.trim().strip_prefix("role:") {
            Some(role) => self.resolve_team_role(&caller, role)?,
            None => self.resolve_model_tab(&params.to)?,
        };
        let pane = self
            .target_pane(target)
            .ok_or_else(|| ModelError::new(error_code::NOT_FOUND, "no pane"))?;
        let to_public = self.public_pane_id(target.ws_idx, pane).unwrap_or_default();
        let to_name = self
            .model_agent_name(target.ws_idx, pane)
            .unwrap_or_else(|| to_public.clone());
        let mut line = self.tab_line("send_message", target);
        line.target_pane = Some(to_public.clone());
        line.target_name = Some(to_name.clone());
        let Some(to_terminal) = self
            .state
            .workspaces
            .get(target.ws_idx)
            .and_then(|ws| ws.pane_state(pane))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return Err(ModelError::new(error_code::NOT_FOUND, "no terminal"));
        };
        let agent_pane = self
            .model_terminal(target.ws_idx, pane)
            .is_some_and(crate::terminal::TerminalState::is_agent_terminal);
        if !agent_pane {
            return Err(ModelError::new(
                error_code::NOT_AN_AGENT,
                format!("{to_public} is a shell; messages go to agents"),
            ));
        }
        let relation = if caller.ws_idx == target.ws_idx && caller.pane_id == pane {
            Relation::SelfPane
        } else {
            match self.model_relation(&caller, target.ws_idx, target.tab_idx) {
                // The coordinator's tab only guards tab changes; another
                // pane of the caller's own tab is a teammate pane here.
                Relation::Protected => Relation::Other,
                Relation::SelfPane => Relation::Teammate,
                relation => relation,
            }
        };
        let reply = self
            .reply_index()
            .is_reply(reply_to, &caller.public, &to_public);
        let mut facts = self.model_facts(&caller, to_name.clone(), Some(target.ws_idx));
        facts.reply_to_turn_starter = match (&caller.turn.origin, reply_to) {
            (TurnOrigin::AgentMessage { id, .. }, Some(reply_to)) => id == reply_to && reply,
            _ => false,
        };
        self.model_authorize(&caller, relation, Action::Message, &facts, &line)?;
        let for_user = matches!(caller.actor, Actor::Coordinator) && facts.user_turn;
        let now = now_unix();
        if let Err(refusal) =
            self.agents_model
                .limiter
                .check_message(&caller.terminal_id, &to_terminal, reply, now)
        {
            line.code = Some(refusal.code().to_string());
            self.log_message(
                &caller,
                &to_public,
                &to_name,
                &text,
                None,
                reply_to,
                refusal.code(),
                None,
            );
            self.log_model_action(Some(&caller), AgentsActionOutcome::Denied, line);
            return Err(ModelError::new(refusal.code(), refusal.hint()));
        }
        let id = crate::coordinator::messages::new_id();
        let target_team = self
            .state
            .workspaces
            .get(target.ws_idx)
            .filter(|ws| ws.team.as_ref().is_some_and(|team| team.is_member(pane)))
            .map(|ws| ws.id.clone());
        let teammate = caller.team_ws.is_some()
            && caller.team_ws == Some(target.ws_idx)
            && target_team.is_some();
        let log_team = target_team.clone().filter(|_| teammate);
        // One delivery path (src/app/message_queue.rs): typed in now when
        // the target can take it (idle, settled, its user neither typing nor
        // holding a draft, no coordinator turn live, nothing queued ahead),
        // else queued and typed in once it is free. Checked in this same
        // `&mut App` borrow as the write: no client input lands in between.
        let status = self.message_status(target.ws_idx, pane);
        let Ok(queue_target) = self.resolve_agent_target(&to_public) else {
            self.log_message(
                &caller,
                &to_public,
                &to_name,
                &text,
                Some(&id),
                reply_to,
                error_code::OFFLINE,
                None,
            );
            return Err(ModelError::new(
                error_code::OFFLINE,
                format!("{to_name} is not running"),
            ));
        };
        let check = self.agent_message_check(&queue_target);
        let sender = crate::agents_model::envelope::EnvelopeSender {
            name: caller.name.clone(),
            pane: caller.public.clone(),
            agent: caller.agent.clone(),
            team: caller.team_ws.map(|ws_idx| self.group_label(ws_idx)),
            role: self
                .model_terminal(caller.ws_idx, caller.pane_id)
                .and_then(|terminal| terminal.agent_meta().role.clone()),
            // Server-resolved: only the real coordinator, in a turn its user
            // started (a reply in a message turn stays a plain message).
            coordinator_for_user: for_user,
        };
        let typed =
            crate::agents_model::envelope::envelope(&sender, &id, reply_to, &text, now, teammate);
        let line = self.message_line(
            &caller,
            &to_public,
            &to_name,
            &text,
            Some(&id),
            reply_to,
            crate::coordinator::messages::OUTCOME_QUEUED,
            log_team.clone(),
        );
        let delivered = self.deliver_agent_message(
            &format!("agents-model:{id}"),
            &queue_target,
            check,
            line,
            typed,
            Some(caller.terminal_id.clone()),
            now,
        );
        let reason = match delivered {
            Ok(crate::app::message_queue::MessageDelivery::Sent) => {
                self.log_message(
                    &caller,
                    &to_public,
                    &to_name,
                    &text,
                    Some(&id),
                    reply_to,
                    crate::coordinator::messages::OUTCOME_SENT,
                    log_team,
                );
                None
            }
            // Logged `queued` by the queue.
            Ok(crate::app::message_queue::MessageDelivery::Queued { reason }) => Some(reason),
            Err(refused) => {
                self.log_message(
                    &caller,
                    &to_public,
                    &to_name,
                    &text,
                    Some(&id),
                    reply_to,
                    &refused.code,
                    None,
                );
                return Err(ModelError::new(&refused.code, refused.message));
            }
        };
        // Queued messages count against the limits from when they were sent.
        self.agents_model
            .limiter
            .record_message(&caller.terminal_id, &to_terminal, now);
        self.reply_index().insert(
            id.clone(),
            caller.public.clone(),
            to_public.clone(),
            now,
            now,
        );
        Ok(AgentsMessageResult {
            id,
            outcome: if reason.is_some() {
                AgentsMessageOutcome::Queued
            } else {
                AgentsMessageOutcome::Sent
            },
            to_pane: to_public,
            to_name,
            status,
            team: target_team,
            cross_team: !teammate,
            reason,
        })
    }

    /// The target's status for the sender's result.
    fn message_status(
        &self,
        ws_idx: usize,
        pane: PaneId,
    ) -> Option<crate::api::schema::AgentStatus> {
        let pane_state = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.pane_state(pane))?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        Some(crate::workspace::agent_status(
            terminal.state,
            pane_state.seen,
            terminal.suspended_agent.is_some(),
        ))
    }

    /// Append the message log line (every outcome but `queued`, which the
    /// queue logs).
    #[allow(clippy::too_many_arguments)] // One flat log line; a struct would only rename the fields.
    fn log_message(
        &mut self,
        caller: &ModelCaller,
        to_pane: &str,
        to_name: &str,
        text: &str,
        id: Option<&str>,
        reply_to: Option<&str>,
        outcome: &str,
        team: Option<String>,
    ) {
        let Some(dir) = self.agents_model.dir.clone() else {
            return;
        };
        let message =
            self.message_line(caller, to_pane, to_name, text, id, reply_to, outcome, team);
        if let Err(err) = crate::coordinator::messages::append(&dir, &message) {
            tracing::warn!(err = %err, "agents model: cannot append to the message log");
        }
    }

    /// A message log line; anything but sent or queued is a refusal.
    #[allow(clippy::too_many_arguments)] // One flat log line; a struct would only rename the fields.
    fn message_line(
        &self,
        caller: &ModelCaller,
        to_pane: &str,
        to_name: &str,
        text: &str,
        id: Option<&str>,
        reply_to: Option<&str>,
        outcome: &str,
        team: Option<String>,
    ) -> crate::coordinator::messages::AgentMessage {
        let delivered = outcome == crate::coordinator::messages::OUTCOME_SENT
            || outcome == crate::coordinator::messages::OUTCOME_QUEUED;
        crate::coordinator::messages::AgentMessage {
            unix: now_unix(),
            from_pane: Some(caller.public.clone()),
            from_name: Some(caller.name.clone()),
            to_pane: to_pane.to_string(),
            to_name: Some(to_name.to_string()),
            text: text.to_string(),
            outcome: outcome.to_string(),
            id: id.map(str::to_string),
            reply_to: reply_to.map(str::to_string),
            from_role: self
                .model_terminal(caller.ws_idx, caller.pane_id)
                .and_then(|terminal| terminal.agent_meta().role.clone()),
            kind: (!delivered).then(|| crate::coordinator::messages::KIND_REFUSAL.to_string()),
            team,
        }
    }

    // ----- agents.open_tab ------------------------------------------------------

    pub(super) fn handle_agents_open_tab(
        &mut self,
        id: String,
        params: AgentsOpenTabParams,
    ) -> String {
        let result = self.agents_open_tab(&params);
        Self::model_reply(
            id,
            result.map(|open| ResponseResult::AgentsOpenTab { open }),
        )
    }

    fn agents_open_tab(&mut self, params: &AgentsOpenTabParams) -> ModelResult<AgentsOpenResult> {
        let caller = self.required_caller(&params.caller_pane)?;
        let kind = params
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string);
        match kind.as_deref() {
            Some("claude" | "codex") => {}
            Some(_) => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "agent must be claude or codex",
                ))
            }
            None if params.role.is_some() || params.kickoff.is_some() => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "role and kickoff need an agent to start",
                ))
            }
            None => {}
        }
        if params.role.as_deref().is_some_and(|role| {
            role.trim()
                .eq_ignore_ascii_case(crate::coordinator::COORDINATOR_ROLE)
        }) {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "the coordinator role is set by herdr",
            ));
        }
        let dest_group = match self.model_destination(&params.group, &params.new_group, params.priority)? {
            Some(dest) => dest,
            None if matches!(caller.actor, Actor::Coordinator) && kind.is_some() => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "choose the placement: group (the best-fitting existing group) or priority for urgent work",
                ))
            }
            None => Some(caller.ws_idx),
        };
        let dest = self.model_dest(&caller, dest_group);
        let target_name = match (&params.new_group, dest_group) {
            (Some(new_group), _) => new_group.clone(),
            (None, Some(ws_idx)) => self.group_label(ws_idx),
            (None, None) => String::new(),
        };
        let mut line = LogLine {
            action: "open_tab",
            target_name: Some(target_name.clone()),
            detail: kind.as_ref().map(|kind| {
                format!(
                    "{kind} {}",
                    params
                        .name
                        .clone()
                        .or(params.role.clone())
                        .unwrap_or_default()
                )
            }),
            ..LogLine::default()
        };
        let facts = self.model_facts(&caller, target_name, dest_group);
        self.model_authorize(
            &caller,
            Relation::SelfPane,
            Action::OpenTab { dest },
            &facts,
            &line,
        )?;
        // Spawn caps: agents outside a user turn.
        let capped = matches!(caller.actor, Actor::Agent { .. }) && !caller.turn.user_turn();
        let now = now_unix();
        if capped {
            if !self
                .agents_model
                .limiter
                .check_spawn(&caller.terminal_id, now)
            {
                return Err(self.spawn_refusal(&caller, &line, "tabs per hour"));
            }
            if let Some(ws_idx) = dest_group.filter(|_| dest == Dest::OwnTeam) {
                if self.team_agent_opened_count(ws_idx)
                    >= crate::agents_model::limits::TEAM_AGENT_SPAWNED_MAX
                {
                    return Err(self.spawn_refusal(
                        &caller,
                        &line,
                        "agent-opened agents in this team",
                    ));
                }
            }
            self.agents_model
                .limiter
                .record_spawn(&caller.terminal_id, now);
        }
        let opened = self.open_model_tab(&caller, params, kind.as_deref(), dest_group);
        match opened {
            Ok(result) => {
                line.target_tab = Some(result.tab_id.clone());
                line.target_pane = Some(result.pane_id.clone());
                self.log_model_action(Some(&caller), AgentsActionOutcome::Ok, line);
                Ok(result)
            }
            Err(err) => {
                if capped {
                    self.agents_model.limiter.release_spawn(&caller.terminal_id);
                }
                line.code = Some(err.code.clone());
                line.detail = Some(err.message.clone());
                self.log_model_action(Some(&caller), AgentsActionOutcome::Failed, line);
                Err(err)
            }
        }
    }

    fn spawn_refusal(&mut self, caller: &ModelCaller, line: &LogLine, what: &str) -> ModelError {
        let mut denied = line.clone();
        denied.code = Some(error_code::SPAWN_LIMIT.into());
        self.log_model_action(Some(caller), AgentsActionOutcome::Denied, denied);
        ModelError::new(
            error_code::SPAWN_LIMIT,
            format!("spawn limit ({what}); ask your user"),
        )
    }

    /// Live agent-opened panes in a team's group.
    fn team_agent_opened_count(&self, ws_idx: usize) -> usize {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return 0;
        };
        ws.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .filter(|terminal| {
                terminal.is_agent_terminal()
                    && terminal
                        .agent_meta()
                        .opened_by
                        .as_ref()
                        .is_some_and(AgentsWho::is_agent)
            })
            .count()
    }

    /// Create the tab, set the meta, pre-join the team, start the agent.
    /// A failure after the create closes the new tab again (atomic).
    fn open_model_tab(
        &mut self,
        caller: &ModelCaller,
        params: &AgentsOpenTabParams,
        kind: Option<&str>,
        dest_group: Option<usize>,
    ) -> ModelResult<AgentsOpenResult> {
        let team_group = dest_group.filter(|ws_idx| {
            kind.is_some()
                && self
                    .state
                    .workspaces
                    .get(*ws_idx)
                    .is_some_and(|ws| ws.team.is_some())
        });
        let role = params
            .role
            .as_deref()
            .and_then(crate::workspace::team::sanitize_role);
        let name = match (team_group, kind) {
            (_, None) => params.name.clone(),
            (Some(_), Some(kind)) => Some(
                role.as_deref()
                    .and_then(crate::agent_wrap::team::role_slug)
                    .or(params.name.clone())
                    .unwrap_or_else(|| kind.to_string()),
            ),
            (None, Some(_)) => Some(
                params
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| {
                        ModelError::new(
                            error_code::INVALID_PARAMS,
                            "name is required with agent (outside team groups, where the role names it)",
                        )
                    })?,
            ),
        };
        if kind.is_some() && name.as_deref().is_some_and(|name| !valid_agent_name(name)) {
            return Err(ModelError::new(
                error_code::INVALID_PARAMS,
                "name must match [a-z][a-z0-9_-]{0,31}",
            ));
        }
        let tab_label = params
            .label
            .as_deref()
            .map(|l| one_line(l, LABEL_MAX_CHARS))
            .filter(|l| !l.is_empty())
            .or_else(|| name.clone());
        // The launch context first: nothing is created when it cannot be had.
        let launch = match kind {
            Some(_) => Some(self.launch_ctx()?),
            None => None,
        };
        let created = match (&params.new_group, dest_group) {
            (Some(new_group), _) => {
                let result = self.call_method(Method::WorkspaceCreate(
                    crate::api::schema::WorkspaceCreateParams {
                        source_workspace_id: None,
                        cwd: params.cwd.clone(),
                        focus: false,
                        label: Some(one_line(new_group, LABEL_MAX_CHARS)),
                        env: Default::default(),
                    },
                ))?;
                let created = created_ids(&result)?;
                if let Some(label) = &tab_label {
                    let _ =
                        self.call_method(Method::TabRename(crate::api::schema::TabRenameParams {
                            tab_id: created.1.clone(),
                            label: label.clone(),
                        }));
                }
                created
            }
            (None, Some(ws_idx)) => {
                let result =
                    self.call_method(Method::TabCreate(crate::api::schema::TabCreateParams {
                        workspace_id: Some(self.public_workspace_id(ws_idx)),
                        cwd: params.cwd.clone(),
                        focus: false,
                        label: tab_label.clone(),
                        env: Default::default(),
                    }))?;
                created_ids(&result)?
            }
            (None, None) => {
                return Err(ModelError::new(
                    error_code::INVALID_PARAMS,
                    "no destination",
                ))
            }
        };
        let (workspace_public, tab_public, pane_public) = created;
        let Some((ws_idx, pane)) = self.parse_pane_id(&pane_public) else {
            return Err(ModelError::new(error_code::FAILED, "the new tab vanished"));
        };
        // Meta: role, note and who opened it (server-set only).
        let note = params
            .note
            .as_deref()
            .map(|n| one_line(n, NOTE_MAX_CHARS))
            .filter(|n| !n.is_empty());
        let opened_by = caller.who();
        let meta_role = role.clone();
        self.write_meta(ws_idx, pane, |meta| {
            meta.role = meta_role;
            meta.note = note;
            meta.opened_by = Some(opened_by);
        });
        let (Some(kind), Some(name), Some(ctx)) = (kind, name.clone(), launch) else {
            return Ok(AgentsOpenResult {
                tab_id: tab_public,
                pane_id: pane_public,
                workspace_id: workspace_public,
                name,
                member: false,
            });
        };
        let started = self.start_model_agent(
            &ctx,
            &pane_public,
            kind,
            &name,
            role.as_deref(),
            params.kickoff.as_deref(),
            team_group.is_some(),
        );
        match started {
            Ok(member) => Ok(AgentsOpenResult {
                tab_id: tab_public,
                pane_id: pane_public,
                workspace_id: workspace_public,
                name: Some(name),
                member,
            }),
            Err(err) => {
                // Atomic: no half-joined member, no orphan tab.
                if let Some((ws_idx, pane)) = self.parse_pane_id(&pane_public) {
                    if let Some(team) = self
                        .state
                        .workspaces
                        .get_mut(ws_idx)
                        .and_then(|ws| ws.team.as_mut())
                    {
                        team.remove(pane, false);
                    }
                    self.state.rebuild_team_index();
                }
                // The start's own error code, and what became of the tab.
                let fate = match self.call_method(Method::TabClose(crate::api::schema::TabTarget {
                    tab_id: tab_public.clone(),
                })) {
                    Ok(_) => format!("tab {tab_public} closed"),
                    Err(close) => {
                        tracing::warn!(code = %close.code, tab = %tab_public, "agents model: cannot close the tab of a failed start");
                        format!(
                            "tab {tab_public} pane {pane_public} stays open: {}",
                            close.message
                        )
                    }
                };
                Err(ModelError::new(
                    &err.code,
                    format!("{} ({fate})", err.message),
                ))
            }
        }
    }

    fn launch_ctx(&self) -> ModelResult<crate::coordinator::launch::LaunchCtx> {
        let dir = self.agents_model.dir.clone().ok_or_else(|| {
            ModelError::new(
                error_code::FAILED,
                "agents cannot be started here (no coordinator directory)",
            )
        })?;
        crate::coordinator::launch::LaunchCtx::current(dir, self.coordinator.dashboard_port)
            .map_err(|err| ModelError::new(error_code::FAILED, format!("launch failed: {err}")))
    }

    /// Pre-join the team (with the role) and start the agent with the
    /// herdr+ tools. Whether it joined a team.
    #[allow(clippy::too_many_arguments)] // The launch's parts, each from a different source.
    fn start_model_agent(
        &mut self,
        ctx: &crate::coordinator::launch::LaunchCtx,
        pane: &str,
        kind: &str,
        name: &str,
        role: Option<&str>,
        kickoff: Option<&str>,
        team: bool,
    ) -> ModelResult<bool> {
        use crate::coordinator::launch;
        let mut joined = false;
        let mut team_launch = None;
        if team {
            if let Some(role) = role {
                joined = self
                    .call_method(Method::TeamJoin(crate::api::schema::TeamJoinParams {
                        pane_id: pane.to_string(),
                        role: Some(role.to_string()),
                    }))
                    .is_ok();
            }
            if let Ok(ResponseResult::TeamContext { eligible, text, .. }) =
                self.call_method(Method::TeamContext(crate::api::schema::TeamContextParams {
                    caller_pane: pane.to_string(),
                    ack: false,
                    full: true,
                    ack_revision: None,
                    ack_key: None,
                }))
            {
                team_launch = crate::agent_wrap::team::launch_from_context(Ok(serde_json::json!({
                    "eligible": eligible,
                    "text": text,
                })));
            }
        }
        let mut text = launch::agent_kickoff(&ctx.dir, name, role, None, kickoff);
        if let Some(team) = &team_launch {
            text = launch::team_kickoff(&team.text, &text);
        }
        let argv = if kind == "claude" {
            launch::claude_args_with_team(
                ctx,
                &launch::ClaudeSession::New(launch::new_uuid()),
                false,
                Some(&text),
                team_launch.as_ref(),
            )
            .map_err(|err| ModelError::new(error_code::FAILED, format!("launch failed: {err}")))?
        } else {
            launch::codex_args_with_team(ctx, Some(&text), team_launch.as_ref())
        };
        self.call_method(Method::AgentStart(crate::api::schema::AgentStartParams {
            name: name.to_string(),
            kind: kind.to_string(),
            pane_id: pane.to_string(),
            args: argv,
            timeout_ms: None,
        }))?;
        Ok(joined || team_launch.is_some())
    }
}

/// What `agents.suspend`, `agents.activate` and `agents.restart` do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentLifecycle {
    Suspend,
    Activate,
    Restart,
}

impl AgentLifecycle {
    /// The action-log name.
    fn action_name(self) -> &'static str {
        match self {
            Self::Suspend => "suspend",
            Self::Activate => "activate",
            Self::Restart => "restart",
        }
    }
}

/// `[a-z][a-z0-9_-]{0,31}`.
pub(crate) fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The workspace, tab and pane ids of a `workspace.create` / `tab.create`
/// reply.
fn created_ids(result: &ResponseResult) -> ModelResult<(String, String, String)> {
    match result {
        ResponseResult::WorkspaceCreated { tab, root_pane, .. }
        | ResponseResult::TabCreated { tab, root_pane } => Ok((
            tab.workspace_id.clone(),
            tab.tab_id.clone(),
            root_pane.pane_id.clone(),
        )),
        _ => Err(ModelError::new(
            error_code::FAILED,
            "unexpected create reply",
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use super::*;
    use crate::agents_model::turn::EdgeStatus;
    use crate::api::schema::agents_model::{
        AgentsCheckAction, AgentsCheckParams, AgentsCloseTabParams,
    };
    use crate::api::schema::{TeamMakeParams, TeamPaneParams, TeamSetRoleParams};
    use crate::config::Config;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    /// `bucket` (w1, one tab), a team group `team` (two tabs) and a plain
    /// group `plain` (two tabs). Agents (idle Claude) run in team tab 0,
    /// team tab 1 and plain tab 0; plain tab 1 and bucket are shells.
    pub(crate) fn model_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut team = Workspace::test_new("team");
        team.test_add_tab(Some("fixer"));
        let mut plain = Workspace::test_new("plain");
        plain.test_add_tab(Some("notes"));
        app.state.workspaces = vec![Workspace::test_new("bucket"), team, plain];
        app.state.ensure_test_terminals();
        app.state.active = Some(1);
        app.state.selected = 1;
        for (ws, tab) in [(1, 0), (1, 1), (2, 0)] {
            let pane = pane(&app, ws, tab);
            terminal_mut(&mut app, pane).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        }
        let workspace_id = app.state.workspaces[1].id.clone();
        call(
            &mut app,
            Method::TeamMake(TeamMakeParams {
                workspace_id,
                purpose: None,
                caller_pane: None,
            }),
        );
        app
    }

    pub(crate) fn pane(app: &App, ws_idx: usize, tab_idx: usize) -> PaneId {
        app.state.workspaces[ws_idx].tabs[tab_idx].root_pane
    }

    pub(crate) fn public(app: &App, ws_idx: usize, tab_idx: usize) -> String {
        app.public_pane_id(ws_idx, pane(app, ws_idx, tab_idx))
            .expect("public id")
    }

    pub(crate) fn terminal_mut(app: &mut App, pane: PaneId) -> &mut crate::terminal::TerminalState {
        let (ws_idx, _) = app.find_pane(pane).expect("pane exists");
        let id = app.state.workspaces[ws_idx]
            .pane_state(pane)
            .expect("pane state")
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&id).expect("terminal")
    }

    pub(crate) fn call(app: &mut App, method: Method) -> serde_json::Value {
        let response = app.handle_api_request(Request {
            id: "agents-model-test".into(),
            method,
        });
        serde_json::from_str(&response).expect("json response")
    }

    pub(crate) fn code(value: &serde_json::Value) -> &str {
        value["error"]["code"].as_str().unwrap_or("ok")
    }

    /// The pane's agent starts a turn its user submitted.
    pub(crate) fn user_turn(app: &mut App, pane: PaneId) {
        let terminal = terminal_mut(app, pane);
        let now = Instant::now();
        terminal.turn_mut().note_input(
            &InputSource::Client {
                submit: true,
                attach: false,
            },
            now,
        );
        terminal
            .turn_mut()
            .on_status_edge(EdgeStatus::Idle, EdgeStatus::Working, false, now, 1);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-agents-model-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn the_actor_is_the_servers_record_with_team_turn_and_rights() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let result = call(
            &mut app,
            Method::AgentsActor(AgentsActorParams {
                caller_pane: lead.clone(),
                ..Default::default()
            }),
        );
        let actor = &result["result"]["actor"];
        assert_eq!(actor["kind"], "agent", "{result}");
        assert_eq!(actor["live"], true);
        assert_eq!(actor["team"]["label"], "team");
        assert_eq!(actor["turn"]["origin"], "unknown");
        assert!(actor["rights"]
            .as_str()
            .unwrap()
            .starts_with("edit: your team team"));
        let shell = public(&app, 2, 1);
        let result = call(
            &mut app,
            Method::AgentsActor(AgentsActorParams {
                caller_pane: shell,
                ..Default::default()
            }),
        );
        assert_eq!(result["result"]["actor"]["kind"], "shell");
        let result = call(
            &mut app,
            Method::AgentsActor(AgentsActorParams {
                caller_pane: "w9:p9".into(),
                ..Default::default()
            }),
        );
        assert_eq!(code(&result), "caller_unresolved");
    }

    #[test]
    fn the_directory_hides_shell_screens_outside_the_team() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let result = call(
            &mut app,
            Method::AgentsDirectory(AgentsDirectoryParams {
                caller_pane: Some(lead.clone()),
                all: true,
                ..Default::default()
            }),
        );
        let tabs = result["result"]["directory"]["tabs"]
            .as_array()
            .unwrap()
            .clone();
        let shell_notes = tabs
            .iter()
            .find(|tab| tab["label"] == "notes")
            .expect("plain notes tab");
        assert_eq!(shell_notes["access"], "read_message");
        assert_eq!(shell_notes["panes"][0]["kind"], "shell");
        assert_eq!(shell_notes["panes"][0]["screen"], "exists");
        assert!(shell_notes["panes"][0].get("cwd").is_none());
        let fixer = tabs.iter().find(|tab| tab["label"] == "fixer").unwrap();
        assert_eq!(fixer["access"], "edit");
        // Default scope: the caller's group in full, counts for the rest.
        let result = call(
            &mut app,
            Method::AgentsDirectory(AgentsDirectoryParams {
                caller_pane: Some(lead.clone()),
                ..Default::default()
            }),
        );
        let directory = &result["result"]["directory"];
        assert_eq!(directory["tabs"].as_array().unwrap().len(), 2);
        assert_eq!(directory["groups"].as_array().unwrap().len(), 3);
        assert_eq!(directory["groups"][2]["agent_count"], 1);
        // The row cap counts the rest.
        let result = call(
            &mut app,
            Method::AgentsDirectory(AgentsDirectoryParams {
                caller_pane: Some(lead),
                all: true,
                limit: Some(2),
                ..Default::default()
            }),
        );
        assert_eq!(result["result"]["directory"]["truncated"], 3);
    }

    #[test]
    fn shell_screens_are_read_only_in_the_team_u6() {
        let mut app = model_app();
        let other = public(&app, 2, 0);
        let notes = public(&app, 2, 1);
        let lead = public(&app, 1, 0);
        let result = call(
            &mut app,
            Method::AgentsRead(AgentsReadParams {
                caller_pane: lead,
                target: notes.clone(),
                ..Default::default()
            }),
        );
        assert_eq!(code(&result), "shell_private", "{result}");
        // The plain group's own agent reads a shell of its own tab? No:
        // a plain group is not a team, so only its own tab.
        let result = call(
            &mut app,
            Method::AgentsRead(AgentsReadParams {
                caller_pane: other,
                target: notes,
                ..Default::default()
            }),
        );
        assert_eq!(code(&result), "shell_private", "{result}");
    }

    #[test]
    fn renames_are_free_in_the_team_and_denials_are_logged() {
        let mut app = model_app();
        let dir = temp_dir("rename");
        app.agents_model.dir = Some(dir.clone());
        let lead = public(&app, 1, 0);
        let fixer = public(&app, 1, 1);
        let result = call(
            &mut app,
            Method::AgentsRenameTab(AgentsRenameTabParams {
                caller_pane: lead.clone(),
                target: fixer,
                name: "reviewer".into(),
            }),
        );
        assert_eq!(code(&result), "ok", "{result}");
        assert_eq!(
            app.state.workspaces[1].tabs[1].custom_name.as_deref(),
            Some("reviewer")
        );
        let other = public(&app, 2, 0);
        let result = call(
            &mut app,
            Method::AgentsRenameTab(AgentsRenameTabParams {
                caller_pane: lead,
                target: other,
                name: "mine".into(),
            }),
        );
        assert_eq!(code(&result), "outside_team");
        let log = actions_log::read_tail(&dir, 10);
        assert_eq!(log[0].outcome, AgentsActionOutcome::Denied);
        assert_eq!(log[0].code.as_deref(), Some("outside_team"));
        assert_eq!(log[1].outcome, AgentsActionOutcome::Ok);
        assert_eq!(log[1].action, "rename_tab");
        // The delegated tab.rename was not logged again as a user action.
        assert_eq!(log.len(), 2, "{log:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn set_meta_labels_non_members_and_renames_members_by_role() {
        let mut app = model_app();
        let plain_agent = public(&app, 2, 0);
        let result = call(
            &mut app,
            Method::AgentsSetMeta(AgentsSetMetaParams {
                caller_pane: None,
                target: plain_agent.clone(),
                role: Some("researcher".into()),
                note: Some("reads papers".into()),
            }),
        );
        assert_eq!(result["result"]["meta"]["role"], "researcher", "{result}");
        assert_eq!(result["result"]["meta"]["note"], "reads papers");
        let fixer = pane(&app, 1, 1);
        let fixer_public = public(&app, 1, 1);
        let result = call(
            &mut app,
            Method::AgentsSetMeta(AgentsSetMetaParams {
                caller_pane: None,
                target: fixer_public.clone(),
                role: Some("fixer".into()),
                note: None,
            }),
        );
        assert_eq!(result["result"]["meta"]["role"], "fixer", "{result}");
        let member_role = app.state.workspaces[1]
            .team
            .as_ref()
            .and_then(|team| team.member(fixer))
            .and_then(|member| member.role.clone());
        assert_eq!(member_role.as_deref(), Some("fixer"));
        app.state.assert_invariants_for_test();
        // Clearing records a tombstone.
        call(
            &mut app,
            Method::AgentsSetMeta(AgentsSetMetaParams {
                caller_pane: None,
                target: fixer_public,
                role: Some(String::new()),
                note: None,
            }),
        );
        let meta = terminal_mut(&mut app, fixer).agent_meta().clone();
        assert_eq!(meta.role, None);
        assert!(meta.role_cleared);
        app.state.assert_invariants_for_test();
    }

    #[test]
    fn a_role_survives_leaving_and_the_exclusion_follows_the_pane() {
        let mut app = model_app();
        let fixer = pane(&app, 1, 1);
        let fixer_public = public(&app, 1, 1);
        call(
            &mut app,
            Method::TeamSetRole(TeamSetRoleParams {
                pane_id: fixer_public.clone(),
                role: Some("fixer".into()),
                caller_pane: None,
            }),
        );
        call(
            &mut app,
            Method::TeamLeave(TeamPaneParams {
                pane_id: fixer_public,
            }),
        );
        let meta = terminal_mut(&mut app, fixer).agent_meta().clone();
        assert_eq!(meta.role.as_deref(), Some("fixer"));
        assert_eq!(
            meta.team_excluded.as_deref(),
            Some(app.state.workspaces[1].id.as_str())
        );
        assert!(!app.pane_team_member(1, fixer));
        // A pane reopened with that meta does not count as a teammate.
        let lead_caller = app
            .model_caller(Some(&public(&app, 1, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(app.model_relation(&lead_caller, 1, 1), Relation::Other);
    }

    #[tokio::test]
    async fn closing_needs_the_users_turn_and_a_teammate_shell_tab_closes_now() {
        let mut app = model_app();
        // A shell tab in the team.
        app.state.workspaces[1].test_add_tab(Some("shell"));
        app.state.ensure_test_terminals();
        let lead_pane = pane(&app, 1, 0);
        let lead = public(&app, 1, 0);
        let shell_tab = app.public_tab_id(1, 2).unwrap();
        let close = |app: &mut App, target: &str| {
            call(
                app,
                Method::AgentsCloseTab(AgentsCloseTabParams {
                    caller_pane: lead.clone(),
                    target: target.to_string(),
                    ..Default::default()
                }),
            )
        };
        let result = close(&mut app, &shell_tab);
        assert_eq!(code(&result), "non_user_turn", "{result}");
        user_turn(&mut app, lead_pane);
        let result = close(&mut app, &shell_tab);
        assert_eq!(result["result"]["close"]["outcome"], "closed", "{result}");
        assert_eq!(app.state.workspaces[1].tabs.len(), 2);
        // Another group: outside the team, whatever the turn.
        let other_tab = app.public_tab_id(2, 1).unwrap();
        let result = close(&mut app, &other_tab);
        assert_eq!(code(&result), "outside_team");
        // An agent that only just went idle may be between steps.
        let fixer = pane(&app, 1, 1);
        terminal_mut(&mut app, fixer).turn_mut().on_status_edge(
            EdgeStatus::Working,
            EdgeStatus::Idle,
            false,
            Instant::now(),
            1,
        );
        let fixer_tab = app.public_tab_id(1, 1).unwrap();
        let result = close(&mut app, &fixer_tab);
        assert_eq!(code(&result), "target_busy", "{result}");
        // Typing in a target refuses the close (the typing guard).
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.note_user_input(Instant::now());
        app.state.insert_test_runtime(fixer, runtime);
        let result = close(&mut app, &fixer_tab);
        assert_eq!(code(&result), "user_typing", "{result}");
        // Its own tab: deferred until it is stably idle.
        let own_tab = app.public_tab_id(1, 0).unwrap();
        let result = close(&mut app, &own_tab);
        assert_eq!(result["result"]["close"]["outcome"], "deferred", "{result}");
        assert!(!app.agents_model.closes.is_empty());
        // A keystroke in its pane cancels the deferral.
        terminal_mut(&mut app, lead_pane).turn_mut().note_input(
            &InputSource::Client {
                submit: false,
                attach: false,
            },
            Instant::now() + std::time::Duration::from_millis(5),
        );
        app.drive_pending_agent_closes(Instant::now() + std::time::Duration::from_secs(1));
        assert!(app.agents_model.closes.is_empty());
        assert_eq!(app.state.workspaces[1].tabs.len(), 2);
    }

    #[test]
    fn the_coordinators_tab_is_protected_from_every_agent() {
        let mut app = model_app();
        let coordinator_pane = pane(&app, 0, 0);
        terminal_mut(&mut app, coordinator_pane)
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        app.state.coordinator_terminal_id = Some(
            app.state.workspaces[0]
                .pane_state(coordinator_pane)
                .unwrap()
                .attached_terminal_id
                .clone(),
        );
        let lead_pane = pane(&app, 1, 0);
        user_turn(&mut app, lead_pane);
        let tab = app.public_tab_id(0, 0).unwrap();
        let lead = public(&app, 1, 0);
        let result = call(
            &mut app,
            Method::AgentsRenameTab(AgentsRenameTabParams {
                caller_pane: lead,
                target: tab,
                name: "mine".into(),
            }),
        );
        assert_eq!(code(&result), "protected_tab", "{result}");
    }

    /// A live Claude with a native session and a runtime in `pane` (what
    /// agent.suspend needs).
    fn host_suspendable(
        app: &mut App,
        pane: PaneId,
        name: &str,
    ) -> tokio::sync::mpsc::Receiver<bytes::Bytes> {
        let terminal = terminal_mut(app, pane);
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal
            .set_agent_session_ref(
                "herdr:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id(format!("s-{name}")),
                Some(1),
            )
            .expect("session ref accepted");
        terminal.set_agent_name(name.into());
        let terminal_id = terminal.id.clone();
        let (runtime, rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(terminal_id, runtime);
        rx
    }

    /// Detection saw the suspended agent's process exit.
    fn observe_exit(app: &mut App, pane_id: PaneId) {
        let observed_at = Instant::now();
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: Some(Agent::Claude),
            state: AgentState::Idle,
            visible_blocker: false,
            visible_working: false,
            process_exited: true,
            observed_at,
        });
        app.handle_internal_event(crate::events::AppEvent::StateChanged {
            pane_id,
            agent: None,
            state: AgentState::Unknown,
            visible_blocker: false,
            visible_working: false,
            process_exited: false,
            observed_at: observed_at + std::time::Duration::from_millis(10),
        });
    }

    fn lifecycle(
        app: &mut App,
        op: AgentLifecycle,
        caller: &str,
        target: &str,
    ) -> serde_json::Value {
        let params = AgentsLifecycleParams {
            caller_pane: caller.to_string(),
            target: target.to_string(),
        };
        call(
            app,
            match op {
                AgentLifecycle::Suspend => Method::AgentsSuspend(params),
                AgentLifecycle::Activate => Method::AgentsActivate(params),
                AgentLifecycle::Restart => Method::AgentsRestart(params),
            },
        )
    }

    fn suspended_by(app: &mut App, pane: PaneId) -> Option<AgentsWho> {
        terminal_mut(app, pane)
            .suspended_agent
            .as_ref()
            .and_then(|record| record.suspended_by.clone())
    }

    #[tokio::test]
    async fn a_teammate_suspends_and_activates_freely_and_is_recorded() {
        let mut app = model_app();
        let dir = temp_dir("lifecycle");
        app.agents_model.dir = Some(dir.clone());
        let lead = public(&app, 1, 0);
        let fixer = pane(&app, 1, 1);
        let fixer_tab = app.public_tab_id(1, 1).unwrap();
        let mut rx = host_suspendable(&mut app, fixer, "fixer");
        // No user turn needed: a soft edit in the team.
        let result = lifecycle(&mut app, AgentLifecycle::Suspend, &lead, &fixer_tab);
        assert_eq!(result["result"]["type"], "agent_suspended", "{result}");
        assert_eq!(result["result"]["pane_id"], public(&app, 1, 1));
        assert!(
            suspended_by(&mut app, fixer)
                .as_ref()
                .is_some_and(AgentsWho::is_agent),
            "the record names the agent that suspended it"
        );
        let exit = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(exit, Ok(Some(_))), "the exit input was sent");
        // An agent-suspended entry: activating it is free in the team too.
        observe_exit(&mut app, fixer);
        let result = lifecycle(&mut app, AgentLifecycle::Activate, &lead, &fixer_tab);
        assert_eq!(result["result"]["type"], "agent_activated", "{result}");
        assert!(terminal_mut(&mut app, fixer).suspended_agent.is_none());
        let log = crate::agents_model::actions_log::read_tail(&dir, 10);
        let actions: Vec<(&str, AgentsActionOutcome)> = log
            .iter()
            .map(|entry| (entry.action.as_str(), entry.outcome))
            .collect();
        assert!(
            actions.contains(&("suspend", AgentsActionOutcome::Ok)),
            "{actions:?}"
        );
        assert!(
            actions.contains(&("activate", AgentsActionOutcome::Ok)),
            "{actions:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn lifecycle_waits_while_the_user_types_in_the_target() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let fixer = pane(&app, 1, 1);
        let fixer_tab = app.public_tab_id(1, 1).unwrap();
        let typing = |app: &mut App| {
            let id = terminal_mut(app, fixer).id.clone();
            app.terminal_runtimes
                .get(&id)
                .expect("runtime")
                .note_user_input(Instant::now());
        };
        // Suspend and restart would type the exit command into the user's
        // half-written prompt.
        let _rx = host_suspendable(&mut app, fixer, "fixer");
        typing(&mut app);
        for op in [AgentLifecycle::Suspend, AgentLifecycle::Restart] {
            let result = lifecycle(&mut app, op, &lead, &fixer_tab);
            assert_eq!(code(&result), "user_typing", "{result}");
            assert!(terminal_mut(&mut app, fixer).suspended_agent.is_none());
        }
        // Activate would type the resume command into the user's shell line.
        // (A fresh runtime: the user has been quiet since.)
        let id = terminal_mut(&mut app, fixer).id.clone();
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes.insert(id, runtime);
        let result = lifecycle(&mut app, AgentLifecycle::Suspend, &lead, &fixer_tab);
        assert_eq!(result["result"]["type"], "agent_suspended", "{result}");
        observe_exit(&mut app, fixer);
        typing(&mut app);
        let result = lifecycle(&mut app, AgentLifecycle::Activate, &lead, &fixer_tab);
        assert_eq!(code(&result), "user_typing", "{result}");
        assert!(terminal_mut(&mut app, fixer).suspended_agent.is_some());
    }

    #[test]
    fn a_user_request_is_logged_with_its_real_outcome() {
        let mut app = model_app();
        let dir = temp_dir("user-log");
        app.agents_model.dir = Some(dir.clone());
        let bogus = call(
            &mut app,
            Method::TabClose(crate::api::schema::TabTarget {
                tab_id: "w9:t9".into(),
            }),
        );
        let refused = code(&bogus).to_string();
        assert_ne!(refused, "ok", "{bogus}");
        let tab_id = app.public_tab_id(2, 1).unwrap();
        let closed = call(
            &mut app,
            Method::TabClose(crate::api::schema::TabTarget { tab_id }),
        );
        assert_eq!(code(&closed), "ok", "{closed}");
        let log = crate::agents_model::actions_log::read_tail(&dir, 10);
        let lines: Vec<(&str, AgentsActionOutcome, Option<&str>)> = log
            .iter()
            .map(|entry| (entry.action.as_str(), entry.outcome, entry.code.as_deref()))
            .collect();
        assert_eq!(
            lines,
            [
                ("close_tab", AgentsActionOutcome::Ok, None),
                (
                    "close_tab",
                    AgentsActionOutcome::Failed,
                    Some(refused.as_str())
                ),
            ],
            "newest first"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn activating_what_the_user_suspended_needs_a_user_turn() {
        let mut app = model_app();
        let lead_pane = pane(&app, 1, 0);
        let lead = public(&app, 1, 0);
        let fixer = pane(&app, 1, 1);
        let fixer_public = public(&app, 1, 1);
        let _rx = host_suspendable(&mut app, fixer, "fixer");
        // The user suspends it (agent.suspend: no caller).
        let suspended = call(
            &mut app,
            Method::AgentSuspend(crate::api::schema::AgentSuspendParams {
                target: fixer_public.clone(),
            }),
        );
        assert_eq!(
            suspended["result"]["type"], "agent_suspended",
            "{suspended}"
        );
        assert_eq!(suspended_by(&mut app, fixer), None);
        observe_exit(&mut app, fixer);
        let result = lifecycle(&mut app, AgentLifecycle::Activate, &lead, &fixer_public);
        assert_eq!(code(&result), "non_user_turn", "{result}");
        assert!(terminal_mut(&mut app, fixer).suspended_agent.is_some());
        user_turn(&mut app, lead_pane);
        let result = lifecycle(&mut app, AgentLifecycle::Activate, &lead, &fixer_public);
        assert_eq!(result["result"]["type"], "agent_activated", "{result}");
    }

    #[tokio::test]
    async fn lifecycle_outside_the_team_and_on_the_coordinator_is_refused_and_self_is_free() {
        let mut app = model_app();
        let lead_pane = pane(&app, 1, 0);
        let lead = public(&app, 1, 0);
        // A plain group's agent: outside the team, whatever the turn.
        let other = pane(&app, 2, 0);
        let other_public = public(&app, 2, 0);
        let _other_rx = host_suspendable(&mut app, other, "other");
        user_turn(&mut app, lead_pane);
        for op in [
            AgentLifecycle::Suspend,
            AgentLifecycle::Restart,
            AgentLifecycle::Activate,
        ] {
            let result = lifecycle(&mut app, op, &lead, &other_public);
            assert_eq!(code(&result), "outside_team", "{op:?} {result}");
        }
        assert!(terminal_mut(&mut app, other).suspended_agent.is_none());
        // The coordinator's tab: protected from every agent.
        let coordinator_pane = pane(&app, 0, 0);
        let _coordinator_rx = host_suspendable(&mut app, coordinator_pane, "coordinator");
        app.state.coordinator_terminal_id = Some(
            app.state.workspaces[0]
                .pane_state(coordinator_pane)
                .unwrap()
                .attached_terminal_id
                .clone(),
        );
        let coordinator_tab = app.public_tab_id(0, 0).unwrap();
        for op in [AgentLifecycle::Suspend, AgentLifecycle::Restart] {
            let result = lifecycle(&mut app, op, &lead, &coordinator_tab);
            assert_eq!(code(&result), "protected_tab", "{op:?} {result}");
        }
        // Itself: allowed (restart: it exits and resumes the same session).
        let _lead_rx = host_suspendable(&mut app, lead_pane, "lead");
        let result = lifecycle(&mut app, AgentLifecycle::Restart, &lead, &lead);
        assert_eq!(result["result"]["type"], "agent_restarted", "{result}");
        assert!(
            suspended_by(&mut app, lead_pane)
                .as_ref()
                .is_some_and(AgentsWho::is_agent),
            "a restart that is abandoned leaves it suspended by the agent"
        );
    }

    #[tokio::test]
    async fn messages_go_to_agents_only_and_wait_for_the_users_typing() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let notes = public(&app, 2, 1);
        let fixer_public = public(&app, 1, 1);
        let result = call(
            &mut app,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead.clone(),
                to: notes,
                text: "hi".into(),
                reply_to: None,
            }),
        );
        assert_eq!(code(&result), "not_an_agent", "{result}");
        let result = call(
            &mut app,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead.clone(),
                to: lead.clone(),
                text: "hi".into(),
                reply_to: None,
            }),
        );
        assert_eq!(code(&result), "invalid_target", "{result}");
        // The user types in the target: queued (src/app/message_queue.rs),
        // nothing typed in.
        let fixer = pane(&app, 1, 1);
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.note_user_input(Instant::now());
        app.state.insert_test_runtime(fixer, runtime);
        let result = call(
            &mut app,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: lead,
                to: fixer_public,
                text: "hi".into(),
                reply_to: None,
            }),
        );
        let message = &result["result"]["message"];
        assert_eq!(message["outcome"], "queued", "{result}");
        assert_eq!(message["reason"], "its user is typing in it", "{result}");
        assert!(rx.try_recv().is_err(), "nothing typed");
        assert_eq!(app.message_queue.entries.len(), 1);
        let queued = &app.message_queue.entries[0];
        assert!(queued.envelope.contains("hi"));
        assert_eq!(
            queued.from_terminal.as_ref(),
            app.state.workspaces[1].terminal_id(pane(&app, 1, 0)),
            "the sender, for the target's turn origin"
        );
        let id = queued.message.id.clone().unwrap();
        let from = queued.from_terminal.clone().unwrap();
        // The user is quiet again: typed in once settled, and the turn it
        // starts is the sender's message, not the user's.
        let (runtime, mut rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.state.insert_test_runtime(fixer, runtime);
        app.coordinator.assume_shell_ready = true;
        let t0 = Instant::now();
        let now = now_unix();
        app.message_queue_pass(t0, now);
        assert!(app.message_queue_pass(t0 + crate::app::message_queue::SETTLE, now));
        assert!(app.message_queue.entries.is_empty());
        let typed = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        assert!(matches!(typed, Ok(Some(_))), "typed in");
        let terminal = app.state.workspaces[1].terminal_id(fixer).cloned().unwrap();
        assert_eq!(
            app.effective_turn(&terminal).expect("a turn").origin,
            TurnOrigin::AgentMessage { id, from }
        );
    }

    #[tokio::test]
    async fn the_coordinators_message_in_its_users_turn_carries_the_users_authority() {
        let mut app = model_app();
        // Its turn marker lives in the coordinator dir: never the user's.
        app.coordinator.dir = std::env::temp_dir().join(format!(
            "herdr-agents-model-coordinator-{}-{}",
            std::process::id(),
            crate::coordinator::launch::new_uuid()
        ));
        std::fs::create_dir_all(&app.coordinator.dir).expect("the coordinator dir");
        let coordinator_pane = pane(&app, 0, 0);
        terminal_mut(&mut app, coordinator_pane)
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        app.state.coordinator_terminal_id = Some(
            app.state.workspaces[0]
                .pane_state(coordinator_pane)
                .unwrap()
                .attached_terminal_id
                .clone(),
        );
        user_turn(&mut app, coordinator_pane);
        let fixer = pane(&app, 1, 1);
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.note_user_input(Instant::now());
        app.state.insert_test_runtime(fixer, runtime);
        let (from, to) = (public(&app, 0, 0), public(&app, 1, 1));
        let result = call(
            &mut app,
            Method::AgentsSendMessage(AgentsSendMessageParams {
                caller_pane: from,
                to,
                text: "run the tests".into(),
                reply_to: None,
            }),
        );
        assert_eq!(result["result"]["message"]["outcome"], "queued", "{result}");
        let envelope = &app.message_queue.entries[0].envelope;
        assert!(
            envelope.contains("from the coordinator") && envelope.contains("acting for your user]"),
            "{envelope}"
        );
        assert!(envelope.contains(crate::agents_model::envelope::COORDINATOR_RULE));
        assert!(!envelope.contains("untrusted request"));
        let _ = std::fs::remove_dir_all(&app.coordinator.dir);
    }

    #[tokio::test]
    async fn role_targets_resolve_to_one_teammate_only() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let fixer = pane(&app, 1, 1);
        let fixer_public = public(&app, 1, 1);
        let send = |app: &mut App, from: &str, to: &str| {
            call(
                app,
                Method::AgentsSendMessage(AgentsSendMessageParams {
                    caller_pane: from.to_string(),
                    to: to.to_string(),
                    text: "done: see notes".into(),
                    reply_to: None,
                }),
            )
        };
        let set_role = |app: &mut App, pane: PaneId, role: &str| {
            let team = app.state.workspaces[1].team.as_mut().expect("team");
            team.member_mut(pane).expect("member").role = Some(role.into());
        };
        let lead_pane = pane(&app, 1, 0);
        set_role(&mut app, lead_pane, "Team Lead");
        set_role(&mut app, fixer, "fixer");
        // Its user types in the target: queued, so nothing needs a PTY.
        let (runtime, _rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.note_user_input(Instant::now());
        app.state.insert_test_runtime(fixer, runtime);
        let result = send(&mut app, &lead, "role:FIXER");
        let message = &result["result"]["message"];
        assert_eq!(message["to_pane"], fixer_public.as_str(), "{result}");
        let queued = &app.message_queue.entries[0];
        assert!(queued.envelope.contains("your teammate"));
        assert!(!queued.envelope.contains("acting for your user"));
        // The fixer reaches the lead by its role slug; never itself.
        let (runtime, _lead_rx) = crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        runtime.note_user_input(Instant::now());
        app.state.insert_test_runtime(lead_pane, runtime);
        let result = send(&mut app, &fixer_public, "role:team-lead");
        assert_eq!(
            result["result"]["message"]["to_pane"],
            lead.as_str(),
            "{result}"
        );
        let result = send(&mut app, &fixer_public, "role:fixer");
        assert_eq!(code(&result), "not_found", "{result}");
        let result = send(&mut app, &lead, "role: ");
        assert_eq!(code(&result), "invalid_params", "{result}");
        // Not in a team: no teammates to name.
        let notes = public(&app, 2, 0);
        let result = send(&mut app, &notes, "role:fixer");
        assert_eq!(code(&result), "outside_team", "{result}");
        // Two teammates with the role: an error naming both.
        app.state.workspaces[1].test_add_tab(Some("fixer-2"));
        app.state.ensure_test_terminals();
        let second = pane(&app, 1, 2);
        terminal_mut(&mut app, second).set_detected_state(Some(Agent::Claude), AgentState::Idle);
        let team = app.state.workspaces[1].team.as_mut().expect("team");
        team.members.push(crate::workspace::team::TeamMember::new(
            second,
            Some("Fixer".into()),
            0,
        ));
        let result = send(&mut app, &lead, "role:fixer");
        assert_eq!(code(&result), "invalid_params", "{result}");
        let error = result.to_string();
        assert!(error.contains("2 teammates have role fixer"), "{error}");
        assert!(error.contains(&fixer_public), "{error}");
        assert!(error.contains(&public(&app, 1, 2)), "{error}");
    }

    #[test]
    fn shell_input_into_another_agent_is_refused_and_logged_checks_pass() {
        let mut app = model_app();
        let lead = public(&app, 1, 0);
        let check = |app: &mut App, action, target: String| {
            call(
                app,
                Method::AgentsCheck(AgentsCheckParams {
                    caller_pane: lead.clone(),
                    action,
                    target: Some(target),
                }),
            )
        };
        let fixer = public(&app, 1, 1);
        let result = check(&mut app, AgentsCheckAction::ShellInput, fixer.clone());
        assert_eq!(code(&result), "invalid_target");
        let result = check(&mut app, AgentsCheckAction::Cosmetic, fixer);
        assert_eq!(result["result"]["allowed"], true, "{result}");
        let other = public(&app, 2, 0);
        let result = check(&mut app, AgentsCheckAction::Cosmetic, other);
        assert_eq!(code(&result), "outside_team");
        let result = check(&mut app, AgentsCheckAction::Unknown, lead.clone());
        assert_eq!(code(&result), "invalid_params");
    }

    #[tokio::test]
    async fn opening_outside_the_team_needs_the_user_and_shell_tabs_open_with_meta() {
        let mut app = model_app();
        let lead_pane = pane(&app, 1, 0);
        let lead = public(&app, 1, 0);
        let plain = app.state.workspaces[2].id.clone();
        let result = call(
            &mut app,
            Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: lead.clone(),
                group: Some(plain),
                ..Default::default()
            }),
        );
        assert_eq!(code(&result), "non_user_turn", "{result}");
        // In its own team: free (a shell tab), with who opened it.
        let result = call(
            &mut app,
            Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: lead.clone(),
                note: Some("scratch".into()),
                ..Default::default()
            }),
        );
        let open = &result["result"]["open"];
        assert_eq!(code(&result), "ok", "{result}");
        let (ws_idx, new_pane) = app
            .parse_pane_id(open["pane_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(ws_idx, 1);
        let meta = terminal_mut(&mut app, new_pane).agent_meta().clone();
        assert_eq!(meta.note.as_deref(), Some("scratch"));
        assert!(meta.opened_by.as_ref().is_some_and(AgentsWho::is_agent));
        // Starting an agent needs the coordinator directory (none in tests):
        // the tab is not left behind.
        user_turn(&mut app, lead_pane);
        let tabs_before = app.state.workspaces[1].tabs.len();
        let result = call(
            &mut app,
            Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: lead,
                agent: Some("claude".into()),
                role: Some("tester".into()),
                ..Default::default()
            }),
        );
        assert_eq!(code(&result), "failed", "{result}");
        assert_eq!(app.state.workspaces[1].tabs.len(), tabs_before);
    }

    #[tokio::test]
    async fn a_failed_start_closes_the_new_tab_and_keeps_the_start_error_code() {
        let mut app = model_app();
        let dir = temp_dir("open-start-fails");
        app.agents_model.dir = Some(dir.clone());
        let lead_pane = pane(&app, 1, 0);
        let lead = public(&app, 1, 0);
        user_turn(&mut app, lead_pane);
        let tabs_before = app.state.workspaces[1].tabs.len();
        let members_before = app.state.workspaces[1]
            .team
            .as_ref()
            .map(|team| team.members.len());
        // Another agent already holds the name the role gives: agent.start
        // fails after the tab and the team pre-join were made.
        let fixer = pane(&app, 1, 1);
        terminal_mut(&mut app, fixer).set_agent_name("tester".into());
        let result = call(
            &mut app,
            Method::AgentsOpenTab(AgentsOpenTabParams {
                caller_pane: lead,
                agent: Some("claude".into()),
                role: Some("tester".into()),
                kickoff: Some("Line one.\n\nLine two.".into()),
                ..Default::default()
            }),
        );
        let code = code(&result).to_string();
        assert_eq!(code, "agent_name_taken", "the start's own code: {result}");
        let message = result["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("closed)"),
            "the tab's fate is named: {result}"
        );
        assert_eq!(
            app.state.workspaces[1].tabs.len(),
            tabs_before,
            "no orphan tab"
        );
        assert_eq!(
            app.state.workspaces[1]
                .team
                .as_ref()
                .map(|team| team.members.len()),
            members_before,
            "no half-joined member"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adversarial_identity_state_keeps_the_meta_invariants() {
        let state = crate::app::state::AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();
    }

    /// Every runtime write site is classed; Client and Programmatic sites
    /// record provenance in the same function (design §3.1).
    #[test]
    fn every_pane_write_site_is_classed_and_records_provenance() {
        const FILES: &[(&str, &str)] = &[
            ("src/app/agent_resume.rs", include_str!("agent_resume.rs")),
            ("src/app/agents.rs", include_str!("agents.rs")),
            ("src/app/news.rs", include_str!("news.rs")),
            ("src/app/agent_suspend.rs", include_str!("agent_suspend.rs")),
            ("src/app/api.rs", include_str!("api.rs")),
            (
                "src/app/closed_sessions.rs",
                include_str!("closed_sessions.rs"),
            ),
            ("src/app/api/agents.rs", include_str!("api/agents.rs")),
            ("src/app/api/panes.rs", include_str!("api/panes.rs")),
            (
                "src/server/pane_input.rs",
                include_str!("../server/pane_input.rs"),
            ),
            (
                "src/server/alt_screen_read.rs",
                include_str!("../server/alt_screen_read.rs"),
            ),
            (
                "src/server/headless.rs",
                include_str!("../server/headless.rs"),
            ),
            (
                "src/persist/snapshot.rs",
                include_str!("../persist/snapshot.rs"),
            ),
            (
                "src/persist/restore.rs",
                include_str!("../persist/restore.rs"),
            ),
        ];
        // (file, function, class); "client" and "programmatic" functions
        // must call note_* (or queue_agent_prompt); pane_input's are noted by
        // their headless callers.
        const ALLOW: &[(&str, &str, &str)] = &[
            (
                "src/app/agent_resume.rs",
                "start_pending_agent_resume",
                "internal",
            ),
            ("src/app/agents.rs", "start_agent", "programmatic"),
            ("src/app/news.rs", "news_pane_bytes", "internal"),
            (
                "src/app/agent_suspend.rs",
                "suspend_resolved_agent",
                "programmatic",
            ),
            (
                "src/app/agent_suspend.rs",
                "activate_resolved_agent",
                "programmatic",
            ),
            ("src/app/api.rs", "send_pane_focus_event", "internal"),
            (
                "src/app/closed_sessions.rs",
                "launch_closed_session_resume",
                "programmatic",
            ),
            (
                "src/app/api/agents.rs",
                "queue_agent_prompt",
                "programmatic",
            ),
            (
                "src/app/api/agents.rs",
                "handle_agent_send_keys",
                "programmatic",
            ),
            (
                "src/app/api/panes.rs",
                "handle_pane_send_text",
                "programmatic",
            ),
            (
                "src/app/api/panes.rs",
                "handle_pane_send_input",
                "programmatic",
            ),
            (
                "src/app/api/panes.rs",
                "handle_pane_send_keys",
                "programmatic",
            ),
            ("src/server/pane_input.rs", "apply_scroll", "internal"),
            (
                "src/server/pane_input.rs",
                "apply_terminal_attach_input",
                "client-noted-by-caller",
            ),
            (
                "src/server/pane_input.rs",
                "apply_client_terminal_input_events",
                "client-noted-by-caller",
            ),
            // The attach write itself: apply_terminal_attach_input's client
            // input, or apply_scroll's page keys (internal).
            (
                "src/server/pane_input.rs",
                "send_terminal_attach_input",
                "client-noted-by-caller",
            ),
            ("src/server/alt_screen_read.rs", "send_wheel", "internal"),
            (
                "src/server/headless.rs",
                "paste_client_clipboard_image_path",
                "client",
            ),
            ("src/persist/snapshot.rs", "*", "internal"),
            ("src/persist/restore.rs", "*", "internal"),
        ];
        let writes = [
            "try_send_bytes(",
            "queue_user_input_submission(",
            "try_send_paste(",
            "try_send_focus_event(",
        ];
        for (file, source) in FILES {
            // Production code only.
            let production = source
                .split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(source);
            let lines: Vec<&str> = production.lines().collect();
            for (index, line) in lines.iter().enumerate() {
                if !writes.iter().any(|write| line.contains(write))
                    || line.trim_start().starts_with("//")
                {
                    continue;
                }
                if line.contains("fn try_send") || line.contains("fn queue_user_input_submission") {
                    continue;
                }
                let function = lines[..=index]
                    .iter()
                    .rev()
                    .find_map(|line| {
                        let trimmed = line.trim_start();
                        let start = trimmed.find("fn ")?;
                        if !(trimmed.starts_with("fn ")
                            || trimmed.starts_with("pub")
                            || trimmed.starts_with("async fn")
                            || trimmed.starts_with("unsafe fn"))
                        {
                            return None;
                        }
                        let name = &trimmed[start + 3..];
                        Some(name.split(['(', '<']).next().unwrap_or(name).to_string())
                    })
                    .unwrap_or_default();
                let class = ALLOW
                    .iter()
                    .find(|(f, name, _)| f == file && (*name == "*" || *name == function))
                    .map(|(_, _, class)| *class)
                    .unwrap_or_else(|| {
                        panic!("{file}:{} writes to a pane in `{function}` without a class in the allowlist", index + 1)
                    });
                if class == "client" || class == "programmatic" {
                    // The function's body records provenance.
                    let start = lines[..=index]
                        .iter()
                        .rposition(|line| line.contains(&format!("fn {function}")))
                        .unwrap_or(0);
                    let end = (index + 60).min(lines.len());
                    let body = lines[start..end].join("\n");
                    assert!(
                        body.contains("note_input")
                            || body.contains("note_pane_input")
                            || body.contains(".turn_mut()")
                            || function == "queue_agent_prompt",
                        "{file}: `{function}` writes ({class}) without recording provenance"
                    );
                }
            }
        }
    }
}
