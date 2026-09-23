use std::collections::HashMap;

use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::{TerminalId, TerminalState};

use super::{Tab, Workspace};

/// Detail info for a single pane, used by the agent detail panel.
pub struct PaneDetail {
    pub pane_id: PaneId,
    pub tab_idx: usize,
    pub agent_kind_label: Option<String>,
    pub state: AgentState,
    pub seen: bool,
    /// The pane hosts a parked agent whose process was asked to exit.
    pub suspended: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub tokens: HashMap<String, String>,
}

/// One pane's contribution to a workspace attention rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneAttention {
    pub state: AgentState,
    pub seen: bool,
    pub suspended: bool,
}

impl PaneAttention {
    fn for_terminal(terminal: &TerminalState, seen: bool) -> Self {
        Self {
            state: terminal.state,
            seen,
            suspended: terminal.suspended_agent.is_some(),
        }
    }
}

impl Tab {
    fn pane_details(
        &self,
        terminals: &HashMap<TerminalId, TerminalState>,
        tab_idx: usize,
    ) -> Vec<PaneDetail> {
        self.layout
            .pane_ids()
            .iter()
            .filter_map(|id| {
                let pane = self.panes.get(id)?;
                let terminal = terminals.get(&pane.attached_terminal_id)?;
                let agent_kind_label = terminal
                    .effective_agent_label()
                    .map(str::to_string)
                    .or_else(|| {
                        terminal
                            .suspended_agent
                            .as_ref()
                            .map(|record| record.agent.clone())
                    });
                if terminal.agent_name.is_none() && agent_kind_label.is_none() {
                    return None;
                }
                Some(PaneDetail {
                    pane_id: *id,
                    tab_idx,
                    agent_kind_label,
                    state: terminal.state,
                    seen: pane.seen,
                    suspended: terminal.suspended_agent.is_some(),
                    last_agent_state_change_seq: terminal.last_agent_state_change_seq,
                    tokens: terminal.metadata_tokens.values(),
                })
            })
            .collect()
    }
}

/// The API status of one pane: a parked agent reports `suspended` regardless
/// of the residual detection state; otherwise idle splits into `done` (not yet
/// seen) and `idle`.
pub(crate) fn agent_status(
    state: AgentState,
    seen: bool,
    suspended: bool,
) -> crate::api::schema::AgentStatus {
    use crate::api::schema::AgentStatus;
    if suspended {
        return AgentStatus::Suspended;
    }
    match (state, seen) {
        (AgentState::Idle, false) => AgentStatus::Done,
        (AgentState::Idle, true) => AgentStatus::Idle,
        (AgentState::Working, _) => AgentStatus::Working,
        (AgentState::Blocked, _) => AgentStatus::Blocked,
        (AgentState::Unknown, _) => AgentStatus::Unknown,
    }
}

/// Attention ranking shared by the API's tab and workspace rollups and the
/// workspace aggregate: blocked first, unseen completions next, then
/// activity; a suspended agent ranks below everything else, including an
/// unclassified one, because nothing is running.
pub(crate) fn agent_status_priority(status: crate::api::schema::AgentStatus) -> u8 {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => 5,
        AgentStatus::Done => 4,
        AgentStatus::Working => 3,
        AgentStatus::Idle => 2,
        AgentStatus::Unknown => 1,
        AgentStatus::Suspended => 0,
    }
}

fn pane_attention_priority(attention: PaneAttention) -> u8 {
    agent_status_priority(agent_status(
        attention.state,
        attention.seen,
        attention.suspended,
    ))
}

impl Workspace {
    pub fn aggregate_state(&self, terminals: &HashMap<TerminalId, TerminalState>) -> PaneAttention {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|pane| {
                terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| PaneAttention::for_terminal(terminal, pane.seen))
            })
            .max_by_key(|attention| pane_attention_priority(*attention))
            .unwrap_or(PaneAttention {
                state: AgentState::Unknown,
                seen: true,
                suspended: false,
            })
    }

    pub fn pane_details(&self, terminals: &HashMap<TerminalId, TerminalState>) -> Vec<PaneDetail> {
        self.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_idx, tab)| tab.pane_details(terminals, tab_idx))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Direction;

    use super::*;
    use crate::detect::Agent;

    fn terminal_for_pane(ws: &Workspace, pane_id: PaneId) -> TerminalState {
        TerminalState::new(ws.terminal_id(pane_id).unwrap().clone(), "/tmp".into())
    }

    #[test]
    fn aggregate_state_all_unknown() {
        let ws = Workspace::test_new("test");
        let mut terminals = HashMap::new();
        let root = ws.tabs[0].root_pane;
        let terminal = terminal_for_pane(&ws, root);
        terminals.insert(terminal.id.clone(), terminal);
        let PaneAttention { state, seen, .. } = ws.aggregate_state(&terminals);
        assert_eq!(state, AgentState::Unknown);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_priority() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);

        let PaneAttention { state, seen, .. } = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Working);
        assert!(seen);
    }

    #[test]
    fn aggregate_state_done_unseen_beats_working() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut second_terminal = terminal_for_pane(&ws, id2);
        second_terminal.state = AgentState::Working;
        terminals.insert(second_terminal.id.clone(), second_terminal);
        let root = ws.tabs[0].panes.get_mut(&root_id).unwrap();
        root.seen = false;

        let PaneAttention { state, seen, .. } = ws.aggregate_state(&terminals);

        assert_eq!(state, AgentState::Idle);
        assert!(!seen);
    }

    #[test]
    fn suspended_pane_ranks_below_a_seen_idle_pane_and_stays_listed() {
        let mut ws = Workspace::test_new("test");
        let id2 = ws.test_split(Direction::Horizontal);
        let root_id = ws.tabs[0]
            .panes
            .keys()
            .find(|id| **id != id2)
            .copied()
            .unwrap();
        let session = crate::agent_resume::PersistedAgentSession {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        };
        let mut terminals = HashMap::new();
        let mut root_terminal = terminal_for_pane(&ws, root_id);
        root_terminal.state = AgentState::Idle;
        terminals.insert(root_terminal.id.clone(), root_terminal);
        let mut parked = terminal_for_pane(&ws, id2);
        parked.state = AgentState::Blocked;
        parked.restore_suspended_agent("claude".into(), None, session.clone());
        terminals.insert(parked.id.clone(), parked);

        let attention = ws.aggregate_state(&terminals);
        assert_eq!(attention.state, AgentState::Idle);
        assert!(
            !attention.suspended,
            "residual blocked state of a parked agent never wins"
        );

        let details = ws.pane_details(&terminals);
        let parked = details
            .iter()
            .find(|detail| detail.pane_id == id2)
            .expect("an unnamed suspended pane is still an agent pane");
        assert!(parked.suspended);
        assert_eq!(parked.agent_kind_label.as_deref(), Some("claude"));

        let mut only_parked = HashMap::new();
        let mut parked = terminal_for_pane(&ws, id2);
        parked.restore_suspended_agent("claude".into(), None, session);
        only_parked.insert(parked.id.clone(), parked);
        assert!(ws.aggregate_state(&only_parked).suspended);
    }

    #[test]
    fn pane_details_use_tab_vector_index_not_stable_public_tab_number() {
        let mut ws = Workspace::test_new("test");
        let removed_tab = ws.test_add_tab(Some("removed"));
        let survivor_tab = ws.test_add_tab(Some("survivor"));
        let survivor_pane = ws.tabs[survivor_tab].root_pane;
        assert!(ws.close_tab(removed_tab));

        let mut terminals = HashMap::new();
        let mut terminal = terminal_for_pane(&ws, survivor_pane);
        terminal.detected_agent = Some(Agent::Codex);
        terminals.insert(terminal.id.clone(), terminal);

        let details = ws.pane_details(&terminals);
        let survivor = details
            .iter()
            .find(|detail| detail.pane_id == survivor_pane)
            .expect("surviving tab agent should be listed");

        assert_eq!(ws.tabs[1].number, 3);
        assert_eq!(survivor.tab_idx, 1);
    }
}
