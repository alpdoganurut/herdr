//! The coordinator contract over the herdr socket: one [`Api::call`] per JSON API
//! request, and typed helpers that pick the result keys out of the response.
//!
//! Coordinator code never opens the socket itself; `src/cli/coordinator.rs` implements
//! [`Api`] over the CLI's protocol-checked request path, and tests implement it
//! with a closure returning fixture JSON.

use std::fmt;
use std::time::Duration;

use serde_json::Value;

use crate::api::schema::{
    AgentTarget, BrowserActor, BrowserCaller, Method, PaneTarget, TeamContextParams,
    TeamMakeParams, TeamSetPurposeParams,
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
        ack_key: None,
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

/// `agent.message_claim`: whether the queued message `id` was taken off the
/// queue for `pane` (its target): agents_wait_for_message returned it.
pub fn message_claim(api: &impl Api, id: &str, pane: &str) -> Result<bool, ApiError> {
    let result = api.call(Method::AgentMessageClaim(
        crate::api::schema::AgentMessageClaimParams {
            id: id.to_string(),
            pane: pane.to_string(),
        },
    ))?;
    Ok(result["claimed"].as_bool().unwrap_or(false))
}

/// Upper bound for every coordinator wait (a socket is never held for
/// minutes). Fork: under Claude Code's 120 s mark, after which it moves a
/// still-running MCP call to the background and reports its end as a task.
pub const MAX_WAIT_S: u64 = 100;

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
    // The wall clock bounds the wait too: each poll takes time on top of
    // its one-second sleep.
    let started = std::time::Instant::now();
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
        let waited_s = waited.max(started.elapsed().as_secs());
        if waited_s >= timeout_s {
            return Err(ApiError::new(
                "timeout",
                format!("{target} still {status} after {waited_s}s"),
            ));
        }
        sleep(Duration::from_secs(1));
        waited += 1;
    }
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
    fn agent_get_and_pane_get_pick_their_result_keys() {
        let (api, calls) = recorder(|method, _| {
            Ok(match method {
                Method::AgentGet(_) => {
                    json!({ "type": "agent_info", "agent": { "pane_id": "w1:p2", "agent_status": "idle" } })
                }
                Method::PaneGet(_) => {
                    json!({ "type": "pane_info", "pane": { "pane_id": "w1:p2" } })
                }
                _ => json!({}),
            })
        });
        assert_eq!(agent_get(&api, "rev").unwrap()["agent_status"], "idle");
        assert_eq!(pane_get(&api, "w9:p1").unwrap()["pane_id"], "w1:p2");
        assert_eq!(calls.borrow().len(), 2);
        let empty = |_: Method| -> Result<Value, ApiError> { Ok(json!({ "type": "ok" })) };
        assert_eq!(agent_get(&empty, "rev").unwrap_err().code, "bad_response");
        let down = |_: Method| -> Result<Value, ApiError> { Err(err("server_unavailable")) };
        assert_eq!(
            pane_get(&down, "w1:p1").unwrap_err().code,
            "server_unavailable"
        );
    }
}
