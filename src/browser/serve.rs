//! `browser.run` on the connection lane: resolve the caller through the App
//! (one `browser.resolve_caller` round trip), run the operation on the hub
//! (seconds, blocking this connection's thread only), answer.

use std::time::Duration;

use crate::api::schema::{
    BrowserActor, BrowserCaller, BrowserRunParams, Method, Request, ResponseResult,
};
use crate::api::ApiRequestSender;

/// How long the App may take to resolve a pane id.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The caller's actor: resolved through the App when a pane id was given,
/// `external` otherwise (or when the pane is unknown).
pub fn resolve_actor(
    request_id: &str,
    caller: Option<&BrowserCaller>,
    api_tx: &ApiRequestSender,
) -> BrowserActor {
    let Some(caller) = caller else {
        return BrowserActor::External { raw: None };
    };
    let pane_id = caller.pane_id.trim();
    if pane_id.is_empty() {
        return BrowserActor::External { raw: None };
    }
    let response = crate::api::dispatch_to_app_with_timeout(
        Request {
            id: format!("{request_id}:caller"),
            method: Method::BrowserResolveCaller(BrowserCaller {
                pane_id: pane_id.to_string(),
            }),
        },
        api_tx,
        Some(RESOLVE_TIMEOUT),
    );
    actor_from_response(&response, pane_id)
}

/// Parse the App's answer; anything but a `browser_actor` result is external
/// with the raw pane id kept for the log.
pub fn actor_from_response(response: &str, pane_id: &str) -> BrowserActor {
    serde_json::from_str::<crate::api::schema::SuccessResponse>(response)
        .ok()
        .and_then(|success| match success.result {
            ResponseResult::BrowserActor { actor } => Some(actor),
            _ => None,
        })
        .unwrap_or(BrowserActor::External {
            raw: Some(pane_id.to_string()),
        })
}

/// Handle one `browser.run`; returns the JSON response line.
pub fn run(request_id: String, params: BrowserRunParams, api_tx: &ApiRequestSender) -> String {
    let actor = resolve_actor(&request_id, params.caller.as_ref(), api_tx);
    match super::hub().run(&actor, params) {
        Ok(result) => encode_success(request_id, ResponseResult::BrowserRun { result }),
        Err(err) => encode_error(request_id, err.code(), err.message()),
    }
}

fn encode_success(id: String, result: ResponseResult) -> String {
    serde_json::to_string(&crate::api::schema::SuccessResponse { id, result })
        .unwrap_or_else(|err| encode_error(String::new(), "internal_error", err.to_string()))
}

fn encode_error(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&crate::api::schema::ErrorResponse {
        id,
        error: crate::api::schema::ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode error response"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_parsing_falls_back_to_external_with_the_raw_pane() {
        let ok = r#"{"id":"x","result":{"type":"browser_actor","actor":{"kind":"pane","pane_id":"w2:pD","tab_id":"w2:tD","workspace_id":"w2","tab_label":"planner","agent":"claude","session":"default"}}}"#;
        let actor = actor_from_response(ok, "w2:pD");
        assert_eq!(actor.pane_id(), Some("w2:pD"));
        assert_eq!(actor.label(), "planner · claude");
        let err = r#"{"id":"x","error":{"code":"pane_not_found","message":"no"}}"#;
        assert_eq!(
            actor_from_response(err, "w9:p9"),
            BrowserActor::External {
                raw: Some("w9:p9".into())
            }
        );
        assert_eq!(
            actor_from_response("garbage", "p"),
            BrowserActor::External {
                raw: Some("p".into())
            }
        );
    }
}
