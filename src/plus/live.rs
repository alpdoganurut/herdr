//! The live data (`live.json`): what the dashboard reads, rebuilt from the
//! JSON API without any LLM in the loop. [`build`] is pure over the API's
//! `agent.list`, `workspace.list` and `tab.list` results, the registry and the
//! message log, so it is tested with fixture JSON.

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

/// Build the live data; relinks registry entries whose pane id or session id
/// went stale (the caller saves the registry when `relinked` is true).
pub fn build(
    inputs: &Inputs<'_>,
    registry: &mut Registry,
    messages: Vec<AgentMessage>,
    last_change: &HashMap<String, u64>,
    include_unmanaged: bool,
    now: u64,
) -> (LiveData, bool) {
    let groups_by_id: HashMap<String, String> = inputs
        .workspaces
        .iter()
        .filter_map(|w| Some((s(w, "workspace_id")?, s(w, "label").unwrap_or_default())))
        .collect();
    let tab_labels: HashMap<String, String> = inputs
        .tabs
        .iter()
        .filter_map(|t| Some((s(t, "tab_id")?, s(t, "label")?)))
        .collect();
    let mut relinked = false;
    let mut seen = vec![false; registry.agents.len()];
    let mut agents = Vec::new();
    let mut unmanaged_count = 0;
    let mut managed_per_group: HashMap<String, u64> = HashMap::new();
    for info in inputs.agents {
        let Some(pane_id) = s(info, "pane_id") else {
            continue;
        };
        let session = session_of(info);
        let kind = s(info, "agent").or_else(|| s(&info["agent_session"], "agent"));
        let entry = registry
            .find(session.as_deref(), Some(&pane_id))
            .filter(|index| !seen[*index]);
        if let Some(index) = entry {
            seen[index] = true;
            relinked |= registry.relink(index, session.as_deref(), Some(&pane_id), kind.as_deref());
        }
        let managed = entry.is_some();
        if !managed {
            unmanaged_count += 1;
            if !include_unmanaged {
                continue;
            }
        }
        let workspace_id = s(info, "workspace_id").unwrap_or_default();
        let tab_id = s(info, "tab_id").unwrap_or_default();
        let tab_label = tab_labels.get(&tab_id).cloned();
        let name = s(info, "name")
            .or_else(|| tab_label.clone())
            .or_else(|| s(info, "display_agent"))
            .or_else(|| kind.clone())
            .unwrap_or_else(|| pane_id.clone());
        let registered = entry.map(|index| registry.agents[index].clone());
        if managed {
            *managed_per_group.entry(workspace_id.clone()).or_default() += 1;
        }
        agents.push(LiveAgent {
            name,
            pane_id: pane_id.clone(),
            tab_id,
            tab_label,
            group: groups_by_id.get(&workspace_id).cloned(),
            workspace_id,
            agent: kind,
            status: s(info, "agent_status").unwrap_or_else(|| "unknown".into()),
            session,
            cwd: s(info, "foreground_cwd").or_else(|| s(info, "cwd")),
            managed,
            coordinator: registered.as_ref().is_some_and(|r| r.is_coordinator()),
            role: registered.as_ref().and_then(|r| r.role.clone()),
            project: registered.as_ref().and_then(|r| r.project.clone()),
            note: registered.as_ref().and_then(|r| r.note.clone()),
            subagents: info["subagents"].as_u64().unwrap_or(0),
            last_change_unix: last_change.get(&pane_id).copied().unwrap_or(0),
        });
    }
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
        .workspaces
        .iter()
        .filter_map(|w| {
            let workspace_id = s(w, "workspace_id")?;
            Some(LiveGroup {
                label: s(w, "label").unwrap_or_default(),
                tab_count: w["tab_count"].as_u64().unwrap_or(0),
                managed: managed_per_group.get(&workspace_id).copied().unwrap_or(0),
                workspace_id,
            })
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
    use crate::plus::registry::ManagePatch;
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
