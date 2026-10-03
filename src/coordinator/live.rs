//! The live data (`live.json`): what the dashboard reads, rebuilt without any
//! LLM in the loop. [`build_facts`] is pure over typed agent facts the herdr
//! server builds from scalar App accessors (never from per-pane process or
//! cwd inspection), the group and tab names and the wake scope.
//!
//! Agents v2: every agent is listed (every tab is part of herdr+); `managed`
//! now means "in the coordinator's wake scope" (`[coordinator] wake_scope`),
//! which is also what `coordinator.get.managed` reports. The registry
//! (`managed.json`) is not read here any more.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::messages::AgentMessage;
use super::watch::WakeScope;
use crate::agents_model::OpenedBy;
use crate::api::schema::agents_model::AgentActionEntry;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveAgent {
    /// Agent name, else tab label, else agent kind, else pane id.
    pub name: String,
    pub pane_id: String,
    pub tab_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_label: Option<String>,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// In the coordinator's wake scope (the coordinator itself included).
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Who opened the tab: `the user`, `the coordinator` or an agent's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opened_by: Option<String>,
    #[serde(default)]
    pub coordinator: bool,
    #[serde(default)]
    pub subagents: u64,
    /// Last status change the watcher saw (0 = not seen changing yet).
    #[serde(default)]
    pub last_change_unix: u64,
    /// The team (its group's workspace id) this agent is a member of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveGroup {
    pub workspace_id: String,
    pub label: String,
    pub tab_count: u64,
    /// Agents in the wake scope in this group.
    pub managed: u64,
    /// Agents in this group (any scope).
    #[serde(default)]
    pub agents: u64,
    /// Set when the group is a team: its purpose (empty until one is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_purpose: Option<String>,
}

/// One tab (shell tabs included: every tab is part of herdr+).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveTab {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Panes in the tab.
    #[serde(default)]
    pub panes: u64,
    /// Agent panes in the tab (live or suspended).
    #[serde(default)]
    pub agents: u64,
    /// The coordinator's tab (no agent may change it).
    #[serde(default)]
    pub protected: bool,
}

/// The watcher's wake-up bookkeeping, published for the dashboard; left at
/// its default by [`build_facts`] and filled in by the watcher.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchSummary {
    pub last_wake_unix: u64,
    pub wake_seq: u64,
    /// Changes waiting for the next wake-up.
    pub pending: u64,
    /// The hourly or daily wake cap is reached.
    pub capped: bool,
    /// The coordinator stopped being relaunched (relaunch cap reached).
    pub coordinator_down: bool,
    /// A non-user turn marker is live.
    pub turn_live: bool,
    pub wakes_last_hour: u64,
    /// Wake-ups within the last 24 hours.
    #[serde(default)]
    pub wakes_last_day: u64,
    /// When the coordinator last wrote `dashboard/board.json` (its mtime):
    /// the coordinator has no clock to stamp the board itself.
    #[serde(default)]
    pub board_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveData {
    pub generated_unix: u64,
    /// Every agent; `managed` marks the wake scope.
    pub agents: Vec<LiveAgent>,
    pub groups: Vec<LiveGroup>,
    /// Agents v1's offline registry entries and unmanaged count: always
    /// empty since every tab is part of herdr+, kept so a dashboard page
    /// written against the v1 contract still renders.
    #[serde(default)]
    pub offline: Vec<serde_json::Value>,
    #[serde(default)]
    pub unmanaged_count: u64,
    /// Every tab, shell tabs included.
    #[serde(default)]
    pub tabs: Vec<LiveTab>,
    /// `[coordinator] wake_scope`.
    #[serde(default)]
    pub wake_scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_pane: Option<String>,
    pub messages: Vec<AgentMessage>,
    /// The newest action-log lines (`actions.jsonl`), newest last.
    #[serde(default)]
    pub actions: Vec<AgentActionEntry>,
    #[serde(default)]
    pub watch: WatchSummary,
}

/// One live agent as herdr reports it: scalar facts only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoordinatorAgentFact {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    /// The agent name (`agent.rename` / `agent.start`), when set.
    pub name: Option<String>,
    /// The display label herdr shows for the agent.
    pub display_agent: Option<String>,
    /// The agent kind (`claude`, `codex`, ...).
    pub agent: Option<String>,
    /// `idle`, `working`, `blocked`, `done` or `unknown`.
    pub status: String,
    /// The native session id, when known.
    pub session: Option<String>,
    /// Never filled by the server's own pass (it does not inspect the
    /// pane's process); kept for the dashboard's field.
    pub cwd: Option<String>,
    pub subagents: u64,
    /// The team (workspace id) this agent's pane is a member of.
    pub team: Option<String>,
    /// The pane's role (its agent meta; a member's team role).
    pub role: Option<String>,
    /// The pane's one-line note (its agent meta).
    pub note: Option<String>,
    /// Who opened the tab (`agents.open_tab`); `None` is the user.
    pub opened_by: Option<OpenedBy>,
    /// A member of its group's team.
    pub member: bool,
    /// The coordinator's own pane (the server's record, not a registry role).
    pub coordinator: bool,
}

impl CoordinatorAgentFact {
    /// Whether the coordinator opened this agent's tab.
    pub fn opened_by_coordinator(&self) -> bool {
        matches!(self.opened_by, Some(OpenedBy::Coordinator))
    }
}

/// One sidebar group (space), in sidebar order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupFact {
    pub workspace_id: String,
    pub label: String,
    pub tab_count: u64,
    /// `Some` when the group is a team: its purpose, empty until set.
    pub team_purpose: Option<String>,
}

/// One tab: its pane counts (scalar facts only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabFact {
    pub tab_id: String,
    pub workspace_id: String,
    pub panes: u64,
    pub agents: u64,
    pub protected: bool,
}

/// Typed inputs for [`build_facts`].
pub struct FactInputs<'a> {
    pub agents: &'a [CoordinatorAgentFact],
    pub groups: &'a [GroupFact],
    pub tabs: &'a [TabFact],
    /// Tab id to tab label.
    pub tab_labels: &'a HashMap<String, String>,
}

/// Build the live data: every agent, `managed` for the ones `scope` covers
/// (and the coordinator).
pub fn build_facts(
    inputs: &FactInputs<'_>,
    scope: WakeScope,
    messages: Vec<AgentMessage>,
    last_change: &HashMap<String, u64>,
    now: u64,
) -> LiveData {
    let groups_by_id: HashMap<&str, &str> = inputs
        .groups
        .iter()
        .map(|group| (group.workspace_id.as_str(), group.label.as_str()))
        .collect();
    let mut managed_per_group: HashMap<&str, u64> = HashMap::new();
    let mut agents_per_group: HashMap<&str, u64> = HashMap::new();
    let mut agents = Vec::with_capacity(inputs.agents.len());
    for fact in inputs.agents {
        let managed = fact.coordinator || scope.covers(fact.opened_by_coordinator(), fact.member);
        *agents_per_group
            .entry(fact.workspace_id.as_str())
            .or_default() += 1;
        if managed {
            *managed_per_group
                .entry(fact.workspace_id.as_str())
                .or_default() += 1;
        }
        let tab_label = inputs
            .tab_labels
            .get(&fact.tab_id)
            .filter(|label| !label.is_empty())
            .cloned();
        let name = fact
            .name
            .clone()
            .filter(|name| !name.is_empty())
            .or_else(|| tab_label.clone())
            .or_else(|| fact.display_agent.clone())
            .or_else(|| fact.agent.clone())
            .unwrap_or_else(|| fact.pane_id.clone());
        agents.push(LiveAgent {
            name,
            pane_id: fact.pane_id.clone(),
            tab_id: fact.tab_id.clone(),
            tab_label,
            group: groups_by_id
                .get(fact.workspace_id.as_str())
                .map(|label| label.to_string()),
            workspace_id: fact.workspace_id.clone(),
            agent: fact.agent.clone(),
            status: fact.status.clone(),
            session: fact.session.clone(),
            cwd: fact.cwd.clone(),
            managed,
            role: fact.role.clone(),
            note: fact.note.clone(),
            opened_by: fact.opened_by.as_ref().map(OpenedBy::describe),
            coordinator: fact.coordinator,
            subagents: fact.subagents,
            last_change_unix: last_change.get(&fact.pane_id).copied().unwrap_or(0),
            team: fact.team.clone(),
        });
    }
    let groups = inputs
        .groups
        .iter()
        .map(|group| LiveGroup {
            label: group.label.clone(),
            tab_count: group.tab_count,
            managed: managed_per_group
                .get(group.workspace_id.as_str())
                .copied()
                .unwrap_or(0),
            agents: agents_per_group
                .get(group.workspace_id.as_str())
                .copied()
                .unwrap_or(0),
            workspace_id: group.workspace_id.clone(),
            team_purpose: group.team_purpose.clone(),
        })
        .collect();
    let tabs = inputs
        .tabs
        .iter()
        .map(|tab| LiveTab {
            tab_id: tab.tab_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            label: inputs
                .tab_labels
                .get(&tab.tab_id)
                .filter(|label| !label.is_empty())
                .cloned(),
            panes: tab.panes,
            agents: tab.agents,
            protected: tab.protected,
        })
        .collect();
    let coordinator_pane = agents
        .iter()
        .find(|agent| agent.coordinator)
        .map(|agent| agent.pane_id.clone());
    LiveData {
        generated_unix: now,
        agents,
        groups,
        offline: Vec::new(),
        unmanaged_count: 0,
        tabs,
        wake_scope: scope.as_str().to_string(),
        coordinator_pane,
        messages,
        actions: Vec::new(),
        watch: WatchSummary::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(pane: &str, session: &str, status: &str) -> CoordinatorAgentFact {
        CoordinatorAgentFact {
            pane_id: pane.into(),
            tab_id: format!("{}:t1", &pane[..2]),
            workspace_id: pane[..2].into(),
            agent: Some("claude".into()),
            status: status.into(),
            session: (!session.is_empty()).then(|| session.to_string()),
            ..CoordinatorAgentFact::default()
        }
    }

    fn groups() -> Vec<GroupFact> {
        vec![
            GroupFact {
                workspace_id: "w1".into(),
                label: "coordinator".into(),
                tab_count: 1,
                team_purpose: None,
            },
            GroupFact {
                workspace_id: "w2".into(),
                label: "app".into(),
                tab_count: 3,
                team_purpose: Some("fix sync".into()),
            },
        ]
    }

    /// The coordinator, an agent it opened, a team member and a plain agent.
    fn facts() -> Vec<CoordinatorAgentFact> {
        let mut coordinator = fact("w1:p1", "s1", "idle");
        coordinator.coordinator = true;
        let mut opened = fact("w2:p4", "s4", "working");
        opened.opened_by = Some(OpenedBy::Coordinator);
        opened.subagents = 2;
        opened.role = Some("fixer".into());
        let mut member = fact("w2:p5", "s5", "blocked");
        member.member = true;
        member.team = Some("w2".into());
        member.tab_id = "w2:t2".into();
        let mut plain = fact("w2:p6", "", "idle");
        plain.tab_id = "w2:t3".into();
        vec![coordinator, opened, member, plain]
    }

    fn build(scope: WakeScope) -> LiveData {
        let facts = facts();
        let groups = groups();
        let tabs = vec![
            TabFact {
                tab_id: "w2:t1".into(),
                workspace_id: "w2".into(),
                panes: 1,
                agents: 1,
                protected: false,
            },
            TabFact {
                tab_id: "w2:t4".into(),
                workspace_id: "w2".into(),
                panes: 1,
                agents: 0,
                protected: false,
            },
        ];
        let tab_labels = HashMap::from([("w2:t1".to_string(), "api work".to_string())]);
        let last_change = HashMap::from([("w2:p4".to_string(), 50)]);
        build_facts(
            &FactInputs {
                agents: &facts,
                groups: &groups,
                tabs: &tabs,
                tab_labels: &tab_labels,
            },
            scope,
            Vec::new(),
            &last_change,
            99,
        )
    }

    fn managed(live: &LiveData) -> Vec<&str> {
        live.agents
            .iter()
            .filter(|agent| agent.managed)
            .map(|agent| agent.pane_id.as_str())
            .collect()
    }

    #[test]
    fn every_agent_is_listed_and_the_scope_marks_the_watched_ones() {
        let opened = build(WakeScope::Opened);
        assert_eq!(opened.agents.len(), 4, "every agent is listed");
        assert_eq!(managed(&opened), ["w1:p1", "w2:p4"]);
        assert_eq!(
            managed(&build(WakeScope::Teams)),
            ["w1:p1", "w2:p4", "w2:p5"]
        );
        assert_eq!(managed(&build(WakeScope::All)).len(), 4);
        assert_eq!(opened.wake_scope, "opened");
        assert_eq!(opened.coordinator_pane.as_deref(), Some("w1:p1"));
        let fixer = &opened.agents[1];
        assert_eq!(fixer.name, "api work", "the tab label names it");
        assert_eq!(fixer.opened_by.as_deref(), Some("the coordinator"));
        assert_eq!(fixer.role.as_deref(), Some("fixer"));
        assert_eq!(fixer.subagents, 2);
        assert_eq!(fixer.last_change_unix, 50);
        assert_eq!(fixer.cwd, None, "the server pass never reads a cwd");
        assert_eq!(opened.groups[1].managed, 1);
        assert_eq!(opened.groups[1].agents, 3);
        assert_eq!(opened.groups[1].team_purpose.as_deref(), Some("fix sync"));
        assert_eq!(opened.tabs.len(), 2, "shell tabs are listed");
        assert_eq!(opened.tabs[0].label.as_deref(), Some("api work"));
        assert_eq!(opened.tabs[1].agents, 0);
    }

    #[test]
    fn live_data_of_an_older_build_still_parses() {
        let old = r#"{"generated_unix":1,"agents":[],"offline":[],"groups":[],"unmanaged_count":0,"messages":[]}"#;
        let live: LiveData = serde_json::from_str(old).unwrap();
        assert_eq!(live.watch, WatchSummary::default());
        assert!(live.tabs.is_empty() && live.actions.is_empty());
        let json = serde_json::to_value(&live).unwrap();
        assert_eq!(json["watch"]["wake_seq"], 0);
    }
}
