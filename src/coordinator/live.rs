//! The live data (`live.json`): what the dashboard reads, rebuilt without any
//! LLM in the loop. [`build_facts`] is pure over typed agent facts (the herdr
//! server builds them from scalar App accessors, never from per-pane process
//! or cwd inspection), the group and tab names, the registry and the message
//! log. [`build`] is the JSON edge for callers that read the API over the
//! socket (`agent.list`, `workspace.list`, `tab.list`): it converts once and
//! calls [`build_facts`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::messages::AgentMessage;
use super::registry::Registry;

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
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub coordinator: bool,
    #[serde(default)]
    pub subagents: u64,
    /// Last status change the watcher saw (0 = not seen changing yet).
    #[serde(default)]
    pub last_change_unix: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveGroup {
    pub workspace_id: String,
    pub label: String,
    pub tab_count: u64,
    pub managed: u64,
}

/// A registry entry whose agent is not running anywhere right now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// The watcher's wake-up bookkeeping, published for the dashboard; left at
/// its default by [`build`] and filled in by the watcher.
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
    /// Managed agents (and, when asked for, unmanaged ones too).
    pub agents: Vec<LiveAgent>,
    pub offline: Vec<OfflineAgent>,
    pub groups: Vec<LiveGroup>,
    pub unmanaged_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_pane: Option<String>,
    pub messages: Vec<AgentMessage>,
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
    /// Only the socket edge fills this (the API reports it); the server's
    /// own pass leaves it unset rather than inspect the pane's process.
    pub cwd: Option<String>,
    pub subagents: u64,
}

/// One sidebar group (space), in sidebar order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupFact {
    pub workspace_id: String,
    pub label: String,
    pub tab_count: u64,
}

/// Typed inputs for [`build_facts`].
pub struct FactInputs<'a> {
    pub agents: &'a [CoordinatorAgentFact],
    pub groups: &'a [GroupFact],
    /// Tab id to tab label.
    pub tab_labels: &'a HashMap<String, String>,
}

/// The API inputs, as returned by `agent.list`, `workspace.list` and `tab.list`.
pub struct Inputs<'a> {
    pub agents: &'a [Value],
    pub workspaces: &'a [Value],
    pub tabs: &'a [Value],
}

fn s(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

/// The native session id of an `agent.list` entry.
pub fn session_of(agent: &Value) -> Option<String> {
    s(&agent["agent_session"], "value")
}

/// An `agent.list` entry as a fact; `None` without a pane id.
pub fn fact_from_json(info: &Value) -> Option<CoordinatorAgentFact> {
    Some(CoordinatorAgentFact {
        pane_id: s(info, "pane_id")?,
        tab_id: s(info, "tab_id").unwrap_or_default(),
        workspace_id: s(info, "workspace_id").unwrap_or_default(),
        name: s(info, "name"),
        display_agent: s(info, "display_agent"),
        agent: s(info, "agent").or_else(|| s(&info["agent_session"], "agent")),
        status: s(info, "agent_status").unwrap_or_else(|| "unknown".into()),
        session: session_of(info),
        cwd: s(info, "foreground_cwd").or_else(|| s(info, "cwd")),
        subagents: info["subagents"].as_u64().unwrap_or(0),
    })
}

/// A `workspace.list` entry as a group; `None` without an id.
pub fn group_from_json(workspace: &Value) -> Option<GroupFact> {
    Some(GroupFact {
        workspace_id: s(workspace, "workspace_id")?,
        label: s(workspace, "label").unwrap_or_default(),
        tab_count: workspace["tab_count"].as_u64().unwrap_or(0),
    })
}

/// Tab id to label from a `tab.list` result (unlabelled tabs are left out).
pub fn tab_labels_from_json(tabs: &[Value]) -> HashMap<String, String> {
    tabs.iter()
        .filter_map(|t| Some((s(t, "tab_id")?, s(t, "label")?)))
        .collect()
}

/// [`build_facts`] over the JSON API results (the socket edge).
pub fn build(
    inputs: &Inputs<'_>,
    registry: &mut Registry,
    messages: Vec<AgentMessage>,
    last_change: &HashMap<String, u64>,
    include_unmanaged: bool,
    now: u64,
) -> (LiveData, bool) {
    let agents: Vec<CoordinatorAgentFact> =
        inputs.agents.iter().filter_map(fact_from_json).collect();
    let groups: Vec<GroupFact> = inputs
        .workspaces
        .iter()
        .filter_map(group_from_json)
        .collect();
    let tab_labels = tab_labels_from_json(inputs.tabs);
    build_facts(
        &FactInputs {
            agents: &agents,
            groups: &groups,
            tab_labels: &tab_labels,
        },
        registry,
        messages,
        last_change,
        include_unmanaged,
        now,
    )
}

/// Build the live data; relinks registry entries whose pane id or session id
/// went stale (the caller saves the registry when `relinked` is true).
pub fn build_facts(
    inputs: &FactInputs<'_>,
    registry: &mut Registry,
    messages: Vec<AgentMessage>,
    last_change: &HashMap<String, u64>,
    include_unmanaged: bool,
    now: u64,
) -> (LiveData, bool) {
    let groups_by_id: HashMap<&str, &str> = inputs
        .groups
        .iter()
        .map(|group| (group.workspace_id.as_str(), group.label.as_str()))
        .collect();
    let mut relinked = false;
    let mut seen = vec![false; registry.agents.len()];
    let mut agents = Vec::new();
    let mut unmanaged_count = 0;
    let mut managed_per_group: HashMap<String, u64> = HashMap::new();
    let facts = inputs.agents;
    // Session matches claim their entries first, so a pane-only match (an
    // agent in a pane an entry used to have) cannot take an entry whose own
    // session is live elsewhere.
    let mut claims: Vec<Option<usize>> = vec![None; facts.len()];
    for (claim, fact) in claims.iter_mut().zip(facts) {
        if let Some(index) = registry.find_by_session(fact.session.as_deref()) {
            if !seen[index] {
                seen[index] = true;
                *claim = Some(index);
            }
        }
    }
    for (claim, fact) in claims.iter_mut().zip(facts) {
        if claim.is_some() {
            continue;
        }
        *claim = (0..registry.agents.len()).find(|&index| {
            !seen[index]
                && registry.pane_matches(
                    index,
                    fact.session.as_deref(),
                    Some(&fact.pane_id),
                    fact.agent.as_deref(),
                )
        });
        if let Some(index) = *claim {
            seen[index] = true;
        }
    }
    for (claim, fact) in claims.iter().zip(facts) {
        if let Some(index) = claim {
            relinked |= registry.relink(
                *index,
                fact.session.as_deref(),
                Some(&fact.pane_id),
                fact.agent.as_deref(),
            );
        }
    }
    for (fact, entry) in facts.iter().zip(claims) {
        let managed = entry.is_some();
        if !managed {
            unmanaged_count += 1;
            if !include_unmanaged {
                continue;
            }
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
        let registered = entry.map(|index| registry.agents[index].clone());
        if managed {
            *managed_per_group
                .entry(fact.workspace_id.clone())
                .or_default() += 1;
        }
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
            coordinator: registered.as_ref().is_some_and(|r| r.is_coordinator()),
            role: registered.as_ref().and_then(|r| r.role.clone()),
            project: registered.as_ref().and_then(|r| r.project.clone()),
            note: registered.as_ref().and_then(|r| r.note.clone()),
            subagents: fact.subagents,
            last_change_unix: last_change.get(&fact.pane_id).copied().unwrap_or(0),
        });
    }
    // An entry that lost its last key (its pane now hosts an agent matched by
    // session, and it never had a session) can never match or be named
    // again: drop it rather than keep an offline row nothing can remove.
    let before = registry.agents.len();
    let mut kept_seen = Vec::with_capacity(before);
    let mut index = 0;
    registry.agents.retain(|entry| {
        let keep = entry.session.is_some() || entry.pane_id.is_some();
        if keep {
            kept_seen.push(seen[index]);
        }
        index += 1;
        keep
    });
    let seen = kept_seen;
    relinked |= registry.agents.len() != before;
    let offline = registry
        .agents
        .iter()
        .zip(&seen)
        .filter(|(_, seen)| !**seen)
        .map(|(entry, _)| OfflineAgent {
            pane_id: entry.pane_id.clone(),
            session: entry.session.clone(),
            agent: entry.agent.clone(),
            role: entry.role.clone(),
            project: entry.project.clone(),
        })
        .collect();
    let groups = inputs
        .groups
        .iter()
        .map(|group| LiveGroup {
            label: group.label.clone(),
            tab_count: group.tab_count,
            managed: managed_per_group
                .get(&group.workspace_id)
                .copied()
                .unwrap_or(0),
            workspace_id: group.workspace_id.clone(),
        })
        .collect();
    let coordinator_pane = agents
        .iter()
        .find(|agent| agent.coordinator)
        .map(|agent| agent.pane_id.clone());
    (
        LiveData {
            generated_unix: now,
            agents,
            offline,
            groups,
            unmanaged_count,
            coordinator_pane,
            messages,
            watch: WatchSummary::default(),
        },
        relinked,
    )
}

/// The agent a tool target names, in order of precedence: pane id, name,
/// tab label, then the role alias `coordinator`. Within one tier a managed
/// agent wins over an unmanaged one (so an unmanaged pane that shares a name
/// does not shadow it); the caller still checks `managed`. `name` is the
/// display name [`build`] derives (agent name, else tab label, ...), so it
/// covers both the agent name and the display name.
pub fn find_live<'a>(live: &'a LiveData, target: &str) -> Option<&'a LiveAgent> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let tiers: [&dyn Fn(&LiveAgent) -> bool; 4] = [
        &|agent| agent.pane_id == target,
        &|agent| agent.name == target,
        &|agent| agent.tab_label.as_deref() == Some(target),
        &|agent| target == super::COORDINATOR_ROLE && agent.coordinator,
    ];
    tiers.iter().find_map(|matches| {
        let mut hits = live.agents.iter().filter(|agent| matches(agent));
        let first = hits.next()?;
        if first.managed {
            return Some(first);
        }
        Some(hits.find(|agent| agent.managed).unwrap_or(first))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::registry::ManagePatch;
    use serde_json::json;

    fn agent(pane: &str, session: &str, status: &str) -> Value {
        json!({
            "terminal_id": "t", "pane_id": pane, "tab_id": format!("{}:t1", &pane[..2]),
            "workspace_id": &pane[..2], "agent": "claude", "agent_status": status,
            "agent_session": { "source": "hook", "agent": "claude", "kind": "id", "value": session },
            "focused": false, "revision": 1, "cwd": "/p"
        })
    }

    #[test]
    fn keeps_managed_agents_counts_the_rest_and_relinks_remapped_panes() {
        let mut registry = Registry::default();
        let patch = |role: &str| ManagePatch {
            role: Some(role.into()),
            project: Some("app".into()),
            note: None,
        };
        registry
            .manage(Some("s1"), Some("w1:p1"), None, &patch("coordinator"))
            .unwrap();
        registry
            .manage(Some("s2"), Some("w9:p9"), None, &patch("lead"))
            .unwrap();
        registry
            .manage(Some("gone"), Some("w8:p8"), None, &patch("reviewer"))
            .unwrap();
        let agents = vec![
            agent("w1:p1", "s1", "idle"),
            agent("w2:p4", "s2", "working"), // restored under a new pane id
            agent("w2:p5", "s5", "blocked"),
        ];
        let workspaces = vec![
            json!({ "workspace_id": "w1", "label": "herdr+", "tab_count": 1 }),
            json!({ "workspace_id": "w2", "label": "app", "tab_count": 2 }),
        ];
        let tabs = vec![json!({ "tab_id": "w2:t1", "label": "api work" })];
        let last_change = HashMap::from([("w2:p4".to_string(), 50)]);
        let inputs = Inputs {
            agents: &agents,
            workspaces: &workspaces,
            tabs: &tabs,
        };
        let (live, relinked) = build(&inputs, &mut registry, Vec::new(), &last_change, false, 99);
        assert!(relinked);
        assert_eq!(registry.agents[1].pane_id.as_deref(), Some("w2:p4"));
        assert_eq!(live.agents.len(), 2);
        assert_eq!(live.unmanaged_count, 1);
        assert_eq!(live.coordinator_pane.as_deref(), Some("w1:p1"));
        let lead = &live.agents[1];
        assert_eq!(lead.name, "api work");
        assert_eq!(lead.group.as_deref(), Some("app"));
        assert_eq!(lead.status, "working");
        assert_eq!(lead.role.as_deref(), Some("lead"));
        assert_eq!(lead.last_change_unix, 50);
        assert_eq!(live.offline.len(), 1);
        assert_eq!(live.offline[0].role.as_deref(), Some("reviewer"));
        assert_eq!(live.groups[1].managed, 1);
        let (all, relinked) = build(&inputs, &mut registry, Vec::new(), &last_change, true, 99);
        assert!(!relinked, "already relinked");
        assert_eq!(all.agents.len(), 3);
        assert!(!all.agents[2].managed);
    }

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

    #[test]
    fn typed_facts_build_the_same_live_data_as_the_json_edge() {
        let patch = |role: &str| ManagePatch {
            role: Some(role.into()),
            project: Some("app".into()),
            note: None,
        };
        let seed = || {
            let mut registry = Registry::default();
            registry
                .manage(Some("s1"), Some("w1:p1"), None, &patch("coordinator"))
                .unwrap();
            registry
                .manage(Some("s2"), Some("w9:p9"), None, &patch("lead"))
                .unwrap();
            registry
                .manage(Some("gone"), Some("w8:p8"), None, &patch("reviewer"))
                .unwrap();
            registry
        };
        let mut lead = fact("w2:p4", "s2", "working");
        lead.subagents = 2;
        let facts = vec![
            fact("w1:p1", "s1", "idle"),
            lead,
            fact("w2:p5", "s5", "blocked"),
        ];
        let groups = vec![
            GroupFact {
                workspace_id: "w1".into(),
                label: "coordinator".into(),
                tab_count: 1,
            },
            GroupFact {
                workspace_id: "w2".into(),
                label: "app".into(),
                tab_count: 2,
            },
        ];
        let tab_labels = HashMap::from([("w2:t1".to_string(), "api work".to_string())]);
        let last_change = HashMap::from([("w2:p4".to_string(), 50)]);
        let mut typed_registry = seed();
        let (typed, relinked) = build_facts(
            &FactInputs {
                agents: &facts,
                groups: &groups,
                tab_labels: &tab_labels,
            },
            &mut typed_registry,
            Vec::new(),
            &last_change,
            true,
            99,
        );
        assert!(relinked);
        assert_eq!(typed_registry.agents[1].pane_id.as_deref(), Some("w2:p4"));
        assert_eq!(typed.coordinator_pane.as_deref(), Some("w1:p1"));
        assert_eq!(typed.agents.len(), 3);
        assert_eq!(typed.unmanaged_count, 1);
        assert_eq!(typed.agents[1].name, "api work");
        assert_eq!(typed.agents[1].subagents, 2);
        assert_eq!(
            typed.agents[1].cwd, None,
            "the server pass never reads a cwd"
        );
        assert_eq!(typed.offline.len(), 1);
        assert_eq!(typed.groups[1].managed, 1);

        let mut lead_json = agent("w2:p4", "s2", "working");
        lead_json["subagents"] = json!(2);
        lead_json.as_object_mut().unwrap().remove("cwd");
        let mut agents = vec![
            agent("w1:p1", "s1", "idle"),
            lead_json,
            agent("w2:p5", "s5", "blocked"),
        ];
        for agent in &mut agents {
            agent.as_object_mut().unwrap().remove("cwd");
        }
        let workspaces = vec![
            json!({ "workspace_id": "w1", "label": "coordinator", "tab_count": 1 }),
            json!({ "workspace_id": "w2", "label": "app", "tab_count": 2 }),
        ];
        let tabs = vec![json!({ "tab_id": "w2:t1", "label": "api work" })];
        let mut json_registry = seed();
        let (from_json, _) = build(
            &Inputs {
                agents: &agents,
                workspaces: &workspaces,
                tabs: &tabs,
            },
            &mut json_registry,
            Vec::new(),
            &last_change,
            true,
            99,
        );
        assert_eq!(typed, from_json);
        assert_eq!(typed_registry, json_registry);
    }

    #[test]
    fn a_session_match_wins_over_a_session_less_agent_in_the_old_pane() {
        let mut registry = Registry::default();
        registry
            .manage(
                Some("S"),
                Some("w1:p1"),
                Some("claude"),
                &ManagePatch {
                    role: Some("coordinator".into()),
                    ..ManagePatch::default()
                },
            )
            .unwrap();
        // After a restore: a session-less Codex sits in the coordinator's old
        // pane and is listed first; the coordinator itself moved to w2:p1.
        let mut codex = agent("w1:p1", "", "idle");
        codex["agent"] = json!("codex");
        codex["agent_session"] = Value::Null;
        let agents = vec![codex, agent("w2:p1", "S", "idle")];
        let inputs = Inputs {
            agents: &agents,
            workspaces: &[],
            tabs: &[],
        };
        let (live, relinked) = build(&inputs, &mut registry, Vec::new(), &HashMap::new(), true, 9);
        assert!(relinked);
        assert_eq!(live.coordinator_pane.as_deref(), Some("w2:p1"));
        assert!(!live.agents[0].managed, "the codex stays unmanaged");
        assert_eq!(registry.agents[0].pane_id.as_deref(), Some("w2:p1"));
        assert_eq!(registry.agents[0].agent.as_deref(), Some("claude"));

        // Restored the other way round: a session-matched Claude lands in the
        // pane a pane-only Codex entry had. That entry has no key left and is
        // dropped instead of lingering as an offline row nothing can remove.
        registry
            .manage(None, Some("w3:p1"), Some("codex"), &ManagePatch::default())
            .unwrap();
        let agents = vec![agent("w3:p1", "S", "idle")];
        let inputs = Inputs {
            agents: &agents,
            workspaces: &[],
            tabs: &[],
        };
        let (live, relinked) = build(&inputs, &mut registry, Vec::new(), &HashMap::new(), true, 9);
        assert!(relinked);
        assert_eq!(registry.agents.len(), 1, "{:?}", registry.agents);
        assert_eq!(registry.agents[0].pane_id.as_deref(), Some("w3:p1"));
        assert!(live.offline.is_empty());
    }

    #[test]
    fn find_live_prefers_pane_then_name_then_tab_label_then_coordinator() {
        let agent = |pane: &str, name: &str, tab: Option<&str>, managed: bool| LiveAgent {
            name: name.into(),
            pane_id: pane.into(),
            tab_label: tab.map(str::to_string),
            managed,
            ..LiveAgent::default()
        };
        let live = LiveData {
            agents: vec![
                agent("w1:p1", "boss", Some("control"), true),
                agent("w2:p1", "w1:p1", None, true), // a name that looks like a pane id
                agent("w2:p2", "rev", Some("lead"), false),
                agent("w2:p3", "rev", Some("review"), true),
                agent("w2:p4", "lead", None, true),
                agent("w2:p5", "docs", Some("api"), false),
                agent("w2:p6", "api", None, false),
            ],
            ..LiveData::default()
        };
        let pane = |target: &str| find_live(&live, target).map(|a| a.pane_id.as_str());
        assert_eq!(pane("w1:p1"), Some("w1:p1"), "pane id beats name");
        assert_eq!(pane("rev"), Some("w2:p3"), "managed wins within a tier");
        assert_eq!(pane("lead"), Some("w2:p4"), "name beats tab label");
        assert_eq!(pane("review"), Some("w2:p3"), "tab label");
        assert_eq!(pane("api"), Some("w2:p6"), "unmanaged hits are returned");
        assert_eq!(pane(" docs "), Some("w2:p5"));
        assert_eq!(pane("coordinator"), None, "no agent is flagged yet");
        assert_eq!(pane("nobody"), None);
        assert_eq!(pane(""), None);

        let mut live = live;
        live.agents[0].coordinator = true;
        assert_eq!(
            find_live(&live, "coordinator").map(|a| a.pane_id.as_str()),
            Some("w1:p1")
        );
    }

    #[test]
    fn live_data_without_a_watch_summary_still_parses() {
        let old = r#"{"generated_unix":1,"agents":[],"offline":[],"groups":[],"unmanaged_count":0,"messages":[]}"#;
        let live: LiveData = serde_json::from_str(old).unwrap();
        assert_eq!(live.watch, WatchSummary::default());
        let json = serde_json::to_value(&live).unwrap();
        assert_eq!(json["watch"]["wake_seq"], 0);
    }
}
