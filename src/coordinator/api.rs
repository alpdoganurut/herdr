//! The coordinator contract over the herdr socket: one [`Api::call`] per JSON API
//! request, and typed helpers that pick the result keys out of the response.
//!
//! Coordinator code never opens the socket itself; `src/cli/coordinator.rs` implements
//! [`Api`] over the CLI's protocol-checked request path, and tests implement it
//! with a closure returning fixture JSON.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::api::schema::{
    AgentPromptParams, AgentReadParams, AgentStartParams, AgentTarget, BrowserActor, BrowserCaller,
    EmptyParams, Method, PaneMoveDestination, PaneMoveParams, PaneTarget, ReadFormat, ReadSource,
    TabCreateParams, TabListParams, TabRenameParams, TeamContextParams, TeamGetParams, TeamInfo,
    TeamJoinParams, TeamMakeParams, TeamSetPurposeParams, TeamSetRoleParams, WorkspaceCreateParams,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn missing(key: &str) -> Self {
        Self::new("bad_response", format!("response has no {key}"))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// One request; `Ok` is the response's `result` object.
pub trait Api {
    fn call(&self, method: Method) -> Result<Value, ApiError>;
}

impl<F: Fn(Method) -> Result<Value, ApiError>> Api for F {
    fn call(&self, method: Method) -> Result<Value, ApiError> {
        self(method)
    }
}

/// Whether an MCP server really runs inside the pane its environment names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The pane's shell is among the server's ancestors.
    Verified,
    /// Cannot tell (no pane, no `ps`, the server did not answer): read-only.
    Unverified,
    /// Started from outside that pane (a Codex daemon): every tool refuses.
    Wrong(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerPane {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    pub shell_pid: Option<u32>,
}

fn take(mut result: Value, key: &str) -> Result<Value, ApiError> {
    match result.get_mut(key).map(Value::take) {
        Some(value) if !value.is_null() => Ok(value),
        _ => Err(ApiError::missing(key)),
    }
}

fn take_list(result: Value, key: &str) -> Result<Vec<Value>, ApiError> {
    match take(result, key)? {
        Value::Array(items) => Ok(items),
        _ => Err(ApiError::missing(key)),
    }
}

fn str_at(value: &Value, path: &[&str]) -> Result<String, ApiError> {
    let mut node = value;
    for key in path {
        node = &node[*key];
    }
    node.as_str()
        .map(str::to_string)
        .ok_or_else(|| ApiError::missing(&path.join(".")))
}

/// The canonical pane behind `env_pane` (`browser.resolve_caller`, alias-aware).
pub fn resolve_caller(api: &impl Api, env_pane: &str) -> Result<CallerPane, ApiError> {
    let result = api.call(Method::BrowserResolveCaller(BrowserCaller {
        pane_id: env_pane.to_string(),
    }))?;
    let actor: BrowserActor = serde_json::from_value(take(result, "actor")?)
        .map_err(|err| ApiError::new("bad_response", format!("actor: {err}")))?;
    match actor {
        BrowserActor::Pane {
            pane_id,
            tab_id,
            workspace_id,
            shell_pid,
            gone: false,
            ..
        } => Ok(CallerPane {
            pane_id,
            tab_id,
            workspace_id,
            shell_pid,
        }),
        _ => Err(ApiError::new(
            "pane_not_found",
            format!("{env_pane} is not a live pane"),
        )),
    }
}

/// `agent.notify` from `caller_pane` → `(card id, outcome)` (`shown` or
/// `deduped`). The server names the sender from its own pane record.
pub fn agent_notify(
    api: &impl Api,
    caller_pane: &str,
    kind: crate::api::schema::AgentNoticeKind,
    title: &str,
    body: Option<&str>,
) -> Result<(String, String), ApiError> {
    let result = api.call(Method::AgentNotify(crate::api::schema::AgentNotifyParams {
        caller_pane: caller_pane.to_string(),
        kind,
        title: title.to_string(),
        body: body.map(str::to_string),
    }))?;
    Ok((str_at(&result, &["id"])?, str_at(&result, &["outcome"])?))
}

/// `agent.list` → `agents`.
pub fn agents(api: &impl Api) -> Result<Vec<Value>, ApiError> {
    take_list(api.call(Method::AgentList(EmptyParams {}))?, "agents")
}

/// `workspace.list` → `workspaces`.
pub fn workspaces(api: &impl Api) -> Result<Vec<Value>, ApiError> {
    take_list(
        api.call(Method::WorkspaceList(EmptyParams {}))?,
        "workspaces",
    )
}

/// `tab.list` over every workspace → `tabs`.
pub fn tabs(api: &impl Api) -> Result<Vec<Value>, ApiError> {
    take_list(api.call(Method::TabList(TabListParams::default()))?, "tabs")
}

/// `agent.get` → `agent` (an `AgentInfo`); target is a pane id (or alias) or an agent name.
pub fn agent_get(api: &impl Api, target: &str) -> Result<Value, ApiError> {
    take(
        api.call(Method::AgentGet(AgentTarget {
            target: target.to_string(),
        }))?,
        "agent",
    )
}

/// `pane.get` → `pane`. Unlike `agent.get`, pane lookups resolve the
/// aliases a pane keeps after `pane.move` changed its public id.
pub fn pane_get(api: &impl Api, pane_id: &str) -> Result<Value, ApiError> {
    take(
        api.call(Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_string(),
        }))?,
        "pane",
    )
}

/// `agent.read` as plain text → `read.text`.
pub fn read(
    api: &impl Api,
    target: &str,
    source: ReadSource,
    lines: u32,
) -> Result<String, ApiError> {
    let result = api.call(Method::AgentRead(AgentReadParams {
        target: target.to_string(),
        source,
        lines: Some(lines),
        format: ReadFormat::Text,
        strip_ansi: true,
    }))?;
    str_at(&result, &["read", "text"])
}

/// `agent.prompt` without waiting. The server accepts working targets: the
/// idle gate is the caller's job.
pub fn prompt(api: &impl Api, target: &str, text: &str) -> Result<(), ApiError> {
    api.call(Method::AgentPrompt(AgentPromptParams {
        target: target.to_string(),
        text: text.to_string(),
        wait: None,
    }))?;
    Ok(())
}

/// `tab.create` in the background → `(tab id, root pane id)`.
pub fn tab_create(
    api: &impl Api,
    workspace_id: Option<&str>,
    cwd: Option<&str>,
    label: Option<&str>,
) -> Result<(String, String), ApiError> {
    let result = api.call(Method::TabCreate(TabCreateParams {
        workspace_id: workspace_id.map(str::to_string),
        cwd: cwd.map(str::to_string),
        focus: false,
        label: label.map(str::to_string),
        env: HashMap::new(),
    }))?;
    Ok((
        str_at(&result, &["tab", "tab_id"])?,
        str_at(&result, &["root_pane", "pane_id"])?,
    ))
}

/// `workspace.create` (a sidebar group) in the background → `(workspace id, tab id, root pane id)`.
pub fn workspace_create(
    api: &impl Api,
    label: &str,
    cwd: Option<&str>,
) -> Result<(String, String, String), ApiError> {
    let result = api.call(Method::WorkspaceCreate(WorkspaceCreateParams {
        source_workspace_id: None,
        cwd: cwd.map(str::to_string),
        focus: false,
        label: Some(label.to_string()),
        env: HashMap::new(),
    }))?;
    Ok((
        str_at(&result, &["workspace", "workspace_id"])?,
        str_at(&result, &["tab", "tab_id"])?,
        str_at(&result, &["root_pane", "pane_id"])?,
    ))
}

pub fn tab_rename(api: &impl Api, tab_id: &str, label: &str) -> Result<(), ApiError> {
    api.call(Method::TabRename(TabRenameParams {
        tab_id: tab_id.to_string(),
        label: label.to_string(),
    }))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveDest {
    /// A new tab in an existing group.
    Group {
        workspace_id: String,
        label: Option<String>,
    },
    /// A new group holding the pane in its first tab.
    NewGroup {
        label: String,
        tab_label: Option<String>,
    },
}

/// `pane.move` in the background → `move_result`. The public pane id changes
/// with the group; the old id stays usable as an alias.
pub fn pane_move(api: &impl Api, pane_id: &str, dest: MoveDest) -> Result<Value, ApiError> {
    let destination = match dest {
        MoveDest::Group {
            workspace_id,
            label,
        } => PaneMoveDestination::NewTab {
            workspace_id: Some(workspace_id),
            label,
        },
        MoveDest::NewGroup { label, tab_label } => PaneMoveDestination::NewWorkspace {
            label: Some(label),
            tab_label,
        },
    };
    take(
        api.call(Method::PaneMove(PaneMoveParams {
            pane_id: pane_id.to_string(),
            destination,
            focus: false,
        }))?,
        "move_result",
    )
}

const START_RETRY: Duration = Duration::from_millis(250);
const START_RETRIES: u32 = 20; // 5 s
const START_NAME_SUFFIXES: u32 = 9;

/// `agent.start` → `(final name, agent)`. Retries `agent_pane_busy` /
/// `agent_pane_unavailable` every 250 ms for 5 s (a new tab's shell is not up
/// instantly); on `agent_name_taken` tries `name-2` .. `name-9`. `sleep` is
/// injected so tests and callers with their own clock skip real waits.
#[allow(clippy::too_many_arguments)] // agent.start's parameters plus the injected sleep
pub(crate) fn agent_start_with(
    api: &impl Api,
    name: &str,
    kind: &str,
    pane_id: &str,
    args: Vec<String>,
    timeout_ms: u64,
    sleep: &dyn Fn(Duration),
) -> Result<(String, Value), ApiError> {
    let mut candidate = name.to_string();
    let mut suffix = 1;
    let mut retries = 0;
    loop {
        let result = api.call(Method::AgentStart(AgentStartParams {
            name: candidate.clone(),
            kind: kind.to_string(),
            pane_id: pane_id.to_string(),
            args: args.clone(),
            timeout_ms: Some(timeout_ms),
        }));
        match result {
            Ok(result) => return Ok((candidate, take(result, "agent")?)),
            Err(err)
                if matches!(
                    err.code.as_str(),
                    "agent_pane_busy" | "agent_pane_unavailable"
                ) && retries < START_RETRIES =>
            {
                retries += 1;
                sleep(START_RETRY);
            }
            Err(err) if err.code == "agent_name_taken" && suffix < START_NAME_SUFFIXES => {
                suffix += 1;
                candidate = format!("{name}-{suffix}");
            }
            Err(err) => return Err(err),
        }
    }
}

// ----- teams (`team.*`) ----------------------------------------------------

/// `team.context` for `caller_pane` (the one agent-side read; `ack` marks
/// the current revision as told). The result: `{member, eligible, team,
/// text, revision}`.
pub fn team_context(
    api: &impl Api,
    caller_pane: &str,
    ack: bool,
    full: bool,
) -> Result<Value, ApiError> {
    api.call(Method::TeamContext(TeamContextParams {
        caller_pane: caller_pane.to_string(),
        ack,
        full,
        ack_revision: None,
    }))
}

/// `team.context` ack for `caller_pane`, marking only up to `revision` (the
/// revision of the read whose text was delivered) as told: a change that
/// lands between that read and this ack still reaches the member next time.
pub fn team_context_ack(
    api: &impl Api,
    caller_pane: &str,
    revision: Option<u64>,
) -> Result<Value, ApiError> {
    api.call(Method::TeamContext(TeamContextParams {
        caller_pane: caller_pane.to_string(),
        ack: true,
        full: false,
        ack_revision: revision,
    }))
}

/// `team.get` for a group (id or label) → the team (`None`: not a team
/// group, or a server without teams).
pub fn team_of_workspace(api: &impl Api, workspace_id: &str) -> Option<Value> {
    let result = api
        .call(Method::TeamGet(TeamGetParams {
            workspace_id: Some(workspace_id.to_string()),
            caller_pane: None,
        }))
        .ok()?;
    take(result, "team").ok().filter(Value::is_object)
}

/// `team.list` → `teams` (empty on a server without teams, or when the
/// answer does not parse: no team facts rather than no live view).
pub fn team_list(api: &impl Api) -> Vec<TeamInfo> {
    api.call(Method::TeamList(EmptyParams {}))
        .ok()
        .and_then(|result| take(result, "teams").ok())
        .and_then(|teams| serde_json::from_value(teams).ok())
        .unwrap_or_default()
}

/// `team.join` for `pane_id`; with a role it is the pre-launch join of a
/// tab whose agent is about to start.
pub fn team_join(api: &impl Api, pane_id: &str, role: Option<&str>) -> Result<Value, ApiError> {
    api.call(Method::TeamJoin(TeamJoinParams {
        pane_id: pane_id.to_string(),
        role: role.map(str::to_string),
    }))
}

/// `team.make` for a group, by `caller_pane` (the server names the actor).
pub fn team_make(
    api: &impl Api,
    workspace_id: &str,
    purpose: Option<&str>,
    caller_pane: &str,
) -> Result<Value, ApiError> {
    api.call(Method::TeamMake(TeamMakeParams {
        workspace_id: workspace_id.to_string(),
        purpose: purpose.map(str::to_string),
        caller_pane: Some(caller_pane.to_string()),
    }))
}

/// `team.set_purpose` for a group, by `caller_pane`.
pub fn team_set_purpose(
    api: &impl Api,
    workspace_id: &str,
    purpose: Option<&str>,
    caller_pane: &str,
) -> Result<Value, ApiError> {
    api.call(Method::TeamSetPurpose(TeamSetPurposeParams {
        workspace_id: workspace_id.to_string(),
        purpose: purpose.map(str::to_string),
        caller_pane: Some(caller_pane.to_string()),
    }))
}

/// `team.set_role` for a member's pane, by `caller_pane`.
pub fn team_set_role(
    api: &impl Api,
    pane_id: &str,
    role: Option<&str>,
    caller_pane: &str,
) -> Result<Value, ApiError> {
    api.call(Method::TeamSetRole(TeamSetRoleParams {
        pane_id: pane_id.to_string(),
        role: role.map(str::to_string),
        caller_pane: Some(caller_pane.to_string()),
    }))
}

/// Upper bound for every coordinator wait (a socket is never held for minutes).
pub const MAX_WAIT_S: u64 = 120;

/// Poll `agent.get` every second until the status is one of `until`; the
/// final status, or `Err` code `timeout` (message: the last status seen).
pub fn wait_status(
    api: &impl Api,
    target: &str,
    until: &[&str],
    timeout_s: u64,
    sleep: &dyn Fn(Duration),
) -> Result<String, ApiError> {
    let timeout_s = timeout_s.min(MAX_WAIT_S);
    let mut waited = 0;
    loop {
        let agent = agent_get(api, target)?;
        let status = agent["agent_status"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        if until.contains(&status.as_str()) {
            return Ok(status);
        }
        if waited >= timeout_s {
            return Err(ApiError::new(
                "timeout",
                format!("{target} still {status} after {waited}s"),
            ));
        }
        sleep(Duration::from_secs(1));
        waited += 1;
    }
}

/// The workspace id of a group given by id, else by exact label, else by
/// case-insensitive label.
pub fn group_by_label_or_id(workspaces: &[Value], key: &str) -> Option<String> {
    let id = |w: &Value| w["workspace_id"].as_str().map(str::to_string);
    let label = |w: &Value| w["label"].as_str().unwrap_or_default().to_string();
    workspaces
        .iter()
        .find(|w| w["workspace_id"].as_str() == Some(key))
        .or_else(|| workspaces.iter().find(|w| label(w) == key))
        .or_else(|| {
            workspaces
                .iter()
                .find(|w| label(w).eq_ignore_ascii_case(key))
        })
        .and_then(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};

    type Calls = std::rc::Rc<RefCell<Vec<Method>>>;

    /// A closure `Api` that records each method and answers from `reply`.
    fn recorder(
        reply: impl Fn(&Method, usize) -> Result<Value, ApiError>,
    ) -> (impl Fn(Method) -> Result<Value, ApiError>, Calls) {
        let calls = Calls::default();
        let seen = calls.clone();
        let api = move |method: Method| {
            let index = seen.borrow().len();
            let out = reply(&method, index);
            seen.borrow_mut().push(method);
            out
        };
        (api, calls)
    }

    fn err(code: &str) -> ApiError {
        ApiError::new(code, "fixture")
    }

    #[test]
    fn list_helpers_pick_their_result_keys() {
        let api = |method: Method| -> Result<Value, ApiError> {
            Ok(match method {
                Method::AgentList(_) => {
                    json!({ "type": "agent_list", "agents": [{ "pane_id": "w1:p1" }] })
                }
                Method::WorkspaceList(_) => {
                    json!({ "type": "workspace_list", "workspaces": [{ "workspace_id": "w1" }, { "workspace_id": "w2" }] })
                }
                Method::TabList(params) => {
                    assert_eq!(params.workspace_id, None);
                    json!({ "type": "tab_list", "tabs": [] })
                }
                _ => json!({}),
            })
        };
        assert_eq!(agents(&api).unwrap()[0]["pane_id"], "w1:p1");
        assert_eq!(workspaces(&api).unwrap().len(), 2);
        assert!(tabs(&api).unwrap().is_empty());
        let empty = |_: Method| -> Result<Value, ApiError> { Ok(json!({ "type": "ok" })) };
        assert_eq!(agents(&empty).unwrap_err().code, "bad_response");
        let down = |_: Method| -> Result<Value, ApiError> { Err(err("server_unavailable")) };
        assert_eq!(workspaces(&down).unwrap_err().code, "server_unavailable");
    }

    #[test]
    fn resolve_caller_returns_the_canonical_pane_and_refuses_other_actors() {
        let api = |method: Method| -> Result<Value, ApiError> {
            let Method::BrowserResolveCaller(caller) = method else {
                panic!("unexpected {method:?}");
            };
            Ok(match caller.pane_id.as_str() {
                "w2:p4" => json!({ "type": "browser_actor", "actor": {
                    "kind": "pane", "pane_id": "w3:p5", "tab_id": "w3:t2", "workspace_id": "w3",
                    "session": "default", "shell_pid": 4242 } }),
                "gone" => json!({ "type": "browser_actor", "actor": {
                    "kind": "pane", "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "gone": true } }),
                _ => json!({ "type": "browser_actor", "actor": { "kind": "external" } }),
            })
        };
        assert_eq!(
            resolve_caller(&api, "w2:p4").unwrap(),
            CallerPane {
                pane_id: "w3:p5".into(),
                tab_id: "w3:t2".into(),
                workspace_id: "w3".into(),
                shell_pid: Some(4242),
            }
        );
        assert_eq!(
            resolve_caller(&api, "gone").unwrap_err().code,
            "pane_not_found"
        );
        assert_eq!(
            resolve_caller(&api, "x").unwrap_err().code,
            "pane_not_found"
        );
    }

    #[test]
    fn agent_get_read_and_prompt_build_the_right_requests() {
        let (api, calls) = recorder(|method, _| {
            Ok(match method {
                Method::AgentGet(_) => {
                    json!({ "type": "agent_info", "agent": { "pane_id": "w1:p2", "agent_status": "idle" } })
                }
                Method::AgentRead(_) => {
                    json!({ "type": "pane_read", "read": { "text": "hello\n" } })
                }
                Method::AgentPrompt(_) => json!({ "type": "agent_prompted", "agent": {} }),
                _ => json!({}),
            })
        });
        assert_eq!(agent_get(&api, "rev").unwrap()["agent_status"], "idle");
        assert_eq!(
            read(&api, "rev", ReadSource::Visible, 60).unwrap(),
            "hello\n"
        );
        prompt(&api, "w1:p2", "hi").unwrap();
        let calls = calls.borrow();
        assert_eq!(
            calls[1],
            Method::AgentRead(AgentReadParams {
                target: "rev".into(),
                source: ReadSource::Visible,
                lines: Some(60),
                format: ReadFormat::Text,
                strip_ansi: true,
            })
        );
        assert_eq!(
            calls[2],
            Method::AgentPrompt(AgentPromptParams {
                target: "w1:p2".into(),
                text: "hi".into(),
                wait: None,
            })
        );
        let refused = |_: Method| -> Result<Value, ApiError> { Err(err("agent_blocked")) };
        assert_eq!(
            prompt(&refused, "w1:p2", "hi").unwrap_err().code,
            "agent_blocked"
        );
    }

    #[test]
    fn tab_workspace_rename_and_move_map_params_and_results() {
        let (api, calls) = recorder(|method, _| {
            Ok(match method {
                Method::TabCreate(_) => json!({ "type": "tab_created",
                    "tab": { "tab_id": "w1:t3" }, "root_pane": { "pane_id": "w1:p7" } }),
                Method::WorkspaceCreate(_) => json!({ "type": "workspace_created",
                    "workspace": { "workspace_id": "w4" }, "tab": { "tab_id": "w4:t1" },
                    "root_pane": { "pane_id": "w4:p1" } }),
                Method::TabRename(_) => json!({ "type": "ok" }),
                Method::PaneMove(_) => json!({ "type": "pane_move",
                    "move_result": { "changed": true, "pane": { "pane_id": "w4:p2" } } }),
                _ => json!({}),
            })
        });
        assert_eq!(
            tab_create(&api, Some("w1"), Some("/src"), Some("lead")).unwrap(),
            ("w1:t3".to_string(), "w1:p7".to_string())
        );
        assert_eq!(
            workspace_create(&api, "demo", None).unwrap(),
            ("w4".to_string(), "w4:t1".to_string(), "w4:p1".to_string())
        );
        tab_rename(&api, "w1:t3", "api").unwrap();
        let moved = pane_move(
            &api,
            "w1:p7",
            MoveDest::Group {
                workspace_id: "w4".into(),
                label: Some("rev".into()),
            },
        )
        .unwrap();
        assert_eq!(moved["pane"]["pane_id"], "w4:p2");
        pane_move(
            &api,
            "w1:p7",
            MoveDest::NewGroup {
                label: "review".into(),
                tab_label: None,
            },
        )
        .unwrap();
        let calls = calls.borrow();
        let Method::TabCreate(tab) = &calls[0] else {
            panic!("expected tab.create");
        };
        assert!(!tab.focus);
        assert_eq!(tab.workspace_id.as_deref(), Some("w1"));
        assert_eq!(tab.label.as_deref(), Some("lead"));
        let Method::WorkspaceCreate(workspace) = &calls[1] else {
            panic!("expected workspace.create");
        };
        assert!(!workspace.focus);
        assert_eq!(workspace.label.as_deref(), Some("demo"));
        assert_eq!(
            calls[3],
            Method::PaneMove(PaneMoveParams {
                pane_id: "w1:p7".into(),
                destination: PaneMoveDestination::NewTab {
                    workspace_id: Some("w4".into()),
                    label: Some("rev".into()),
                },
                focus: false,
            })
        );
        assert_eq!(
            calls[4],
            Method::PaneMove(PaneMoveParams {
                pane_id: "w1:p7".into(),
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("review".into()),
                    tab_label: None,
                },
                focus: false,
            })
        );
        let missing =
            |_: Method| -> Result<Value, ApiError> { Ok(json!({ "type": "tab_created" })) };
        assert_eq!(
            tab_create(&missing, None, None, None).unwrap_err().code,
            "bad_response"
        );
    }

    #[test]
    fn agent_start_retries_a_busy_pane_then_succeeds() {
        let (api, calls) = recorder(|_, index| match index {
            0 => Err(err("agent_pane_busy")),
            1 => Err(err("agent_pane_unavailable")),
            _ => Ok(
                json!({ "type": "agent_started", "agent": { "pane_id": "w1:p2" }, "argv": ["claude"] }),
            ),
        });
        let slept = Cell::new(Duration::ZERO);
        let sleep = |d: Duration| slept.set(slept.get() + d);
        let (name, agent) = agent_start_with(
            &api,
            "lead",
            "claude",
            "w1:p2",
            vec!["--x".into()],
            60_000,
            &sleep,
        )
        .unwrap();
        assert_eq!(name, "lead");
        assert_eq!(agent["pane_id"], "w1:p2");
        assert_eq!(slept.get(), Duration::from_millis(500));
        let calls = calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[2],
            Method::AgentStart(AgentStartParams {
                name: "lead".into(),
                kind: "claude".into(),
                pane_id: "w1:p2".into(),
                args: vec!["--x".into()],
                timeout_ms: Some(60_000),
            })
        );
    }

    #[test]
    fn agent_start_suffixes_a_taken_name_and_gives_up_on_a_busy_pane() {
        let (api, calls) = recorder(|method, _| {
            let Method::AgentStart(params) = method else {
                panic!("unexpected {method:?}");
            };
            match params.name.as_str() {
                "lead" | "lead-2" => Err(err("agent_name_taken")),
                _ => Ok(
                    json!({ "type": "agent_started", "agent": { "name": params.name }, "argv": [] }),
                ),
            }
        });
        let no_sleep = |_: Duration| {};
        let (name, _) = agent_start_with(
            &api,
            "lead",
            "claude",
            "w1:p2",
            Vec::new(),
            60_000,
            &no_sleep,
        )
        .unwrap();
        assert_eq!(name, "lead-3");
        assert_eq!(calls.borrow().len(), 3);

        let all_taken = |_: Method| -> Result<Value, ApiError> { Err(err("agent_name_taken")) };
        assert_eq!(
            agent_start_with(
                &all_taken,
                "x",
                "codex",
                "w1:p2",
                Vec::new(),
                60_000,
                &no_sleep
            )
            .unwrap_err()
            .code,
            "agent_name_taken"
        );
        let (busy, calls) = recorder(|_, _| Err(err("agent_pane_busy")));
        let slept = Cell::new(Duration::ZERO);
        let sleep = |d: Duration| slept.set(slept.get() + d);
        assert_eq!(
            agent_start_with(&busy, "x", "codex", "w1:p2", Vec::new(), 60_000, &sleep)
                .unwrap_err()
                .code,
            "agent_pane_busy"
        );
        assert_eq!(slept.get(), Duration::from_secs(5));
        assert_eq!(calls.borrow().len(), 21);
        let not_found = |_: Method| -> Result<Value, ApiError> { Err(err("agent_pane_not_found")) };
        assert_eq!(
            agent_start_with(
                &not_found,
                "x",
                "codex",
                "w1:p2",
                Vec::new(),
                60_000,
                &no_sleep
            )
            .unwrap_err()
            .code,
            "agent_pane_not_found"
        );
    }

    #[test]
    fn wait_status_polls_with_the_injected_clock() {
        let statuses = ["working", "working", "working", "idle"];
        let (api, calls) = recorder(move |_, index| {
            Ok(json!({ "type": "agent_info", "agent": { "agent_status": statuses[index.min(3)] } }))
        });
        let slept = Cell::new(0u64);
        let sleep = |d: Duration| slept.set(slept.get() + d.as_secs());
        assert_eq!(
            wait_status(&api, "rev", &["idle", "done"], 60, &sleep).unwrap(),
            "idle"
        );
        assert_eq!(slept.get(), 3);
        assert_eq!(calls.borrow().len(), 4);

        let working = |_: Method| -> Result<Value, ApiError> {
            Ok(json!({ "type": "agent_info", "agent": { "agent_status": "working" } }))
        };
        slept.set(0);
        let timeout = wait_status(&working, "rev", &["idle"], 5, &sleep).unwrap_err();
        assert_eq!(timeout.code, "timeout");
        assert_eq!(timeout.message, "rev still working after 5s");
        assert_eq!(slept.get(), 5);
        slept.set(0);
        wait_status(&working, "rev", &["idle"], 10_000, &sleep).unwrap_err();
        assert_eq!(slept.get(), MAX_WAIT_S, "capped");
        let gone = |_: Method| -> Result<Value, ApiError> { Err(err("agent_not_found")) };
        assert_eq!(
            wait_status(&gone, "rev", &["idle"], 5, &sleep)
                .unwrap_err()
                .code,
            "agent_not_found"
        );
    }

    #[test]
    fn groups_resolve_by_id_then_label() {
        let workspaces = vec![
            json!({ "workspace_id": "w1", "label": "herdr+" }),
            json!({ "workspace_id": "w2", "label": "Demo" }),
            json!({ "workspace_id": "w3", "label": "w1" }),
        ];
        assert_eq!(
            group_by_label_or_id(&workspaces, "w1").as_deref(),
            Some("w1")
        );
        assert_eq!(
            group_by_label_or_id(&workspaces, "herdr+").as_deref(),
            Some("w1")
        );
        assert_eq!(
            group_by_label_or_id(&workspaces, "demo").as_deref(),
            Some("w2")
        );
        assert_eq!(group_by_label_or_id(&workspaces, "nope"), None);
    }
}
