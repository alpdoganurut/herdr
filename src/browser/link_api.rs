//! The companion's calls over the link (fork, remote browser): `api`
//! frames run the few API methods the companion needs ([`API_METHODS`]) on
//! this server, as the companion of this machine would call them, with
//! the link's own lease id put in; `events.subscribe` becomes `event`
//! frames. Everything else is refused, `browser.page_message` included.
//!
//! One thread per link runs the calls in order (the App answers them); a
//! subscription has a thread of its own until the link ends.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::link::{Link, LinkEnd, LinkHandler};
use super::link_proto::{
    code, ApiReply, ApiRequest, LinkError, ServerFrame, API_METHODS, EVENT_KINDS,
};
use crate::api::schema::Request;
use crate::api::{ApiRequestSender, EventHub};

/// Calls waiting for the thread; one more is `host_busy`.
const CALLS_WAITING: usize = 16;
/// Subscriptions one link may hold.
const SUBSCRIPTIONS_MAX: usize = 4;
/// How long the App may take for one call.
const APP_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a subscription looks for new events.
const EVENT_POLL: Duration = Duration::from_millis(100);

/// What the server's API lane lends the link.
#[derive(Clone)]
pub struct ApiLane {
    pub api_tx: ApiRequestSender,
    pub event_hub: EventHub,
}

/// One link's calls: the thread that runs them, once one was asked for.
pub(super) struct ApiRelay {
    lane: Option<Arc<ApiLane>>,
    calls: Option<SyncSender<ApiRequest>>,
    subscriptions: Arc<AtomicUsize>,
}

fn refusal(id: u64, code: &str, message: String) -> ServerFrame {
    ServerFrame::ApiRe(ApiReply {
        id,
        ok: false,
        result: Value::Null,
        error: Some(LinkError::new(code, message)),
    })
}

/// `<method> is not available over the browser link`.
pub(super) fn not_available(request: &ApiRequest) -> ServerFrame {
    refusal(
        request.id,
        code::INVALID_REQUEST,
        format!(
            "{} is not available over the browser link",
            super::link::clip_text(&request.method)
        ),
    )
}

impl ApiRelay {
    pub(super) fn new(lane: Option<Arc<ApiLane>>) -> Self {
        Self {
            lane,
            calls: None,
            subscriptions: Arc::default(),
        }
    }

    /// An `api` frame: refused at once, or queued for the thread.
    pub(super) fn call(
        &mut self,
        link: &Arc<Link>,
        handler: &Arc<dyn LinkHandler>,
        request: ApiRequest,
    ) {
        let Some(lane) = self.lane.clone() else {
            return link.answer(&not_available(&request));
        };
        if !API_METHODS.contains(&request.method.as_str()) {
            return link.answer(&not_available(&request));
        }
        if self.calls.is_none() {
            let (tx, rx) = sync_channel::<ApiRequest>(CALLS_WAITING);
            let (own, handler, subscriptions) = (
                Arc::clone(link),
                Arc::clone(handler),
                Arc::clone(&self.subscriptions),
            );
            let spawned = crate::thread_spawn::spawn_named("herdr-browser-link-api", move || {
                for request in rx {
                    if own.ended().is_some() {
                        return;
                    }
                    let answer = run(&own, &handler, &lane, &subscriptions, request);
                    own.answer(&answer);
                }
            });
            match spawned {
                Ok(_) => self.calls = Some(tx),
                Err(err) => {
                    tracing::warn!(event = "browser.link.api", %err, "no thread for the companion's calls");
                    return link.answer(&refusal(
                        request.id,
                        code::HOST_BUSY,
                        "the server could not take the call now".into(),
                    ));
                }
            }
        }
        if let Some(calls) = &self.calls {
            match calls.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request) | TrySendError::Disconnected(request)) => link
                    .answer(&refusal(
                        request.id,
                        code::HOST_BUSY,
                        format!("{CALLS_WAITING} calls are already waiting"),
                    )),
            }
        }
    }
}

/// One allowlisted call, on the relay's thread.
fn run(
    link: &Arc<Link>,
    handler: &Arc<dyn LinkHandler>,
    lane: &ApiLane,
    subscriptions: &Arc<AtomicUsize>,
    request: ApiRequest,
) -> ServerFrame {
    let params = match handler.api_params(link, &request.method, request.params) {
        Ok(params) => params,
        Err(error) => {
            return ServerFrame::ApiRe(ApiReply {
                id: request.id,
                ok: false,
                result: Value::Null,
                error: Some(error),
            })
        }
    };
    if request.method == "events.subscribe" {
        return subscribe(link, lane, subscriptions, request.id, &params);
    }
    let parsed = serde_json::from_value::<Request>(json!({
        "id": format!("link-{}-{}", link.epoch, request.id),
        "method": request.method,
        "params": params,
    }));
    let call = match parsed {
        Ok(call) => call,
        Err(err) => {
            return refusal(
                request.id,
                code::INVALID_REQUEST,
                super::link::clip_text(&format!("invalid request: {err}")),
            )
        }
    };
    let answer = crate::api::dispatch_to_app_with_timeout(call, &lane.api_tx, Some(APP_TIMEOUT));
    let answer: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
    match answer.get("result") {
        Some(result) => ServerFrame::ApiRe(ApiReply {
            id: request.id,
            ok: true,
            result: handler.api_result(link, &request.method, result.clone()),
            error: None,
        }),
        None => {
            let text = |key: &str| {
                answer["error"][key]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            };
            refusal(request.id, &text("code"), text("message"))
        }
    }
}

/// `events.subscribe` over the link: only the kinds the companion follows;
/// its events go out as `event` frames until the link ends.
fn subscribe(
    link: &Arc<Link>,
    lane: &ApiLane,
    subscriptions: &Arc<AtomicUsize>,
    id: u64,
    params: &Value,
) -> ServerFrame {
    let kinds: Option<Vec<String>> = params["subscriptions"].as_array().map(|list| {
        list.iter()
            .filter_map(|subscription| subscription["type"].as_str().map(str::to_string))
            .collect()
    });
    let Some(kinds) = kinds.filter(|kinds| {
        !kinds.is_empty()
            && kinds
                .iter()
                .all(|kind| EVENT_KINDS.contains(&kind.as_str()))
    }) else {
        return refusal(
            id,
            code::INVALID_REQUEST,
            format!(
                "over the browser link only these events: {}",
                EVENT_KINDS.join(", ")
            ),
        );
    };
    if subscriptions.fetch_add(1, Ordering::AcqRel) >= SUBSCRIPTIONS_MAX {
        subscriptions.fetch_sub(1, Ordering::AcqRel);
        return refusal(
            id,
            code::HOST_BUSY,
            format!("{SUBSCRIPTIONS_MAX} subscriptions are already open"),
        );
    }
    let (link_for_thread, hub, count) = (
        Arc::clone(link),
        lane.event_hub.clone(),
        Arc::clone(subscriptions),
    );
    let mut last = hub.current_sequence();
    let spawned = crate::thread_spawn::spawn_named("herdr-browser-link-events", move || {
        let link = link_for_thread;
        while link.ended().is_none() {
            for (sequence, event) in hub.events_after(last) {
                last = sequence;
                // the same lines as the API's own stream
                if !kinds.iter().any(|kind| kind == event.event.dot_name()) {
                    continue;
                }
                let Ok(event) = serde_json::to_value(event) else {
                    continue;
                };
                if link.send_event(event).is_err() {
                    link.end(LinkEnd::Backlog);
                }
            }
            std::thread::sleep(EVENT_POLL);
        }
        count.fetch_sub(1, Ordering::AcqRel);
    });
    if let Err(err) = spawned {
        subscriptions.fetch_sub(1, Ordering::AcqRel);
        tracing::warn!(event = "browser.link.events", %err, "no thread for a subscription");
        return refusal(
            id,
            code::HOST_BUSY,
            "the server could not subscribe now".into(),
        );
    }
    ServerFrame::ApiRe(ApiReply {
        id,
        ok: true,
        result: json!({ "type": "subscription_started" }),
        error: None,
    })
}
